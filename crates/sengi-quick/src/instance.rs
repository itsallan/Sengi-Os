//! One panel at a time. The open panel listens on a socket in
//! `$XDG_RUNTIME_DIR`; a second invocation asks it to close and does not
//! open one of its own, so the same click or key opens and closes it.
//!
//! This is sengi-launcher's scheme with a single kind of panel. Who owns
//! the socket path is settled under a lock file, so two panels starting
//! together cannot both take it, and a panel only ever removes the socket
//! it created.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

const SOCKET_NAME: &str = "sengi-quick.sock";
const LOCK_NAME: &str = "sengi-quick.lock";
/// A panel answers from a thread that does nothing else, so one that stays
/// silent this long is hung or gone.
const REPLY_TIMEOUT: Duration = Duration::from_millis(500);
const CLOSE: u8 = b'c';
const CLOSING: u8 = b'k';

pub enum Claim {
    /// This process may open the panel.
    Granted(Instance),
    /// A panel was open and has been told to close.
    Toggled,
}

/// Held by the process that owns the panel, for as long as it is open.
pub struct Instance {
    owned: Option<Owned>,
    slot: Arc<Mutex<Slot>>,
}

/// The socket this process bound.
struct Owned {
    socket: PathBuf,
    lock: PathBuf,
    /// Device and inode of the socket file, to tell ours from a successor's.
    identity: (u64, u64),
}

#[derive(Default)]
struct Slot {
    /// A later invocation has asked this one to close. From then on the
    /// socket path belongs to whoever comes next.
    requested: bool,
    close: Option<Box<dyn FnOnce() + Send>>,
}

/// Take the lock that guards the socket path. Released when the file drops.
fn lock(path: &Path) -> Option<File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .ok()?;
    file.lock().ok()?;
    Some(file)
}

fn identity(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

fn slot(slot: &Mutex<Slot>) -> MutexGuard<'_, Slot> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Ask the open panel, if there is one, to close.
fn ask_to_close(path: &Path) -> bool {
    let ask = || {
        let mut stream = UnixStream::connect(path).ok()?;
        stream.set_read_timeout(Some(REPLY_TIMEOUT)).ok()?;
        stream.set_write_timeout(Some(REPLY_TIMEOUT)).ok()?;
        stream.write_all(&[CLOSE]).ok()?;
        let mut reply = [0u8; 1];
        stream.read_exact(&mut reply).ok()?;
        Some(reply[0] == CLOSING)
    };
    ask().unwrap_or(false)
}

/// Become the one panel, or close the one that is open. Without a usable
/// runtime directory there is nowhere to meet, and every panel is granted.
pub fn claim(runtime_dir: Option<&Path>) -> Claim {
    let unowned = || {
        Claim::Granted(Instance {
            owned: None,
            slot: Arc::default(),
        })
    };
    let Some(dir) = runtime_dir else {
        return unowned();
    };
    let socket = dir.join(SOCKET_NAME);
    let lock_path = dir.join(LOCK_NAME);
    // Held until this function returns: asking, removing and binding are one
    // step as far as other panels can tell.
    let Some(_guard) = lock(&lock_path) else {
        return unowned();
    };

    let closing = ask_to_close(&socket);
    // Whether a panel just agreed to close or nothing answered at all, the
    // file at the path is finished with.
    let _ = std::fs::remove_file(&socket);
    if closing {
        return Claim::Toggled;
    }

    let Ok(listener) = UnixListener::bind(&socket) else {
        return unowned();
    };
    let Some(identity) = identity(&socket) else {
        return unowned();
    };
    let slot: Arc<Mutex<Slot>> = Arc::default();
    // Listening starts now, not when the panel is ready: a panel that is
    // slow to start must still answer, or it would be taken for dead.
    std::thread::spawn({
        let slot = slot.clone();
        move || serve(listener, &slot)
    });
    Claim::Granted(Instance {
        owned: Some(Owned {
            socket,
            lock: lock_path,
            identity,
        }),
        slot,
    })
}

/// Answer the first invocation that asks, then have this panel close.
fn serve(listener: UnixListener, shared: &Mutex<Slot>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
        let _ = stream.set_write_timeout(Some(REPLY_TIMEOUT));
        let mut request = [0u8; 1];
        // Something that connects and says nothing, or something else, is
        // not a second sengi-quick.
        if stream.read_exact(&mut request).is_err() || request[0] != CLOSE {
            continue;
        }
        // Marked before the reply goes out: once the other side hears from
        // us it treats the path as free.
        slot(shared).requested = true;
        let _ = stream.write_all(&[CLOSING]);
        break;
    }
    drop(listener);
    let close = slot(shared).close.take();
    if let Some(close) = close {
        close();
    }
}

impl Instance {
    /// Call `close` when a later invocation asks this one to go away. It
    /// runs on another thread, or at once if the request has already come.
    pub fn on_close_request(&self, close: impl FnOnce() + Send + 'static) {
        let mut shared = slot(&self.slot);
        if shared.requested {
            drop(shared);
            close();
        } else {
            shared.close = Some(Box::new(close));
        }
    }

    /// Whether a later invocation has already asked this one to close.
    pub fn close_requested(&self) -> bool {
        slot(&self.slot).requested
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let Some(owned) = self.owned.take() else {
            return;
        };
        let Some(_guard) = lock(&owned.lock) else {
            return;
        };
        // Checked under the lock. After a request the path is the next
        // panel's, and if we were taken for dead it holds someone else's
        // socket.
        if !slot(&self.slot).requested && identity(&owned.socket) == Some(owned.identity) {
            let _ = std::fs::remove_file(&owned.socket);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::sync::{mpsc, Barrier};

    fn granted(claim: Claim) -> Instance {
        match claim {
            Claim::Granted(instance) => instance,
            Claim::Toggled => panic!("expected to be granted"),
        }
    }

    fn socket(tmp: &TempDir) -> PathBuf {
        tmp.path().join(SOCKET_NAME)
    }

    #[test]
    fn the_first_panel_is_granted_and_leaves_a_socket() {
        let tmp = TempDir::new();
        let instance = granted(claim(Some(tmp.path())));
        assert!(instance.owned.is_some());
        assert!(socket(&tmp).exists());
        assert!(!instance.close_requested());
        drop(instance);
        assert!(!socket(&tmp).exists());
    }

    #[test]
    fn a_second_invocation_closes_the_first_and_does_not_open() {
        let tmp = TempDir::new();
        let (closed, was_closed) = mpsc::channel();
        let first = granted(claim(Some(tmp.path())));
        first.on_close_request(move || closed.send(()).unwrap());

        assert!(matches!(claim(Some(tmp.path())), Claim::Toggled));
        was_closed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(first.close_requested());
        assert!(!socket(&tmp).exists());

        // A third opens a fresh panel, even with the first not yet gone,
        // and the first going away does not disturb it.
        let third = granted(claim(Some(tmp.path())));
        drop(first);
        assert!(socket(&tmp).exists());
        drop(third);
        assert!(!socket(&tmp).exists());
    }

    #[test]
    fn a_request_that_came_before_the_panel_was_up_still_closes_it() {
        let tmp = TempDir::new();
        let slow = granted(claim(Some(tmp.path())));
        assert!(matches!(claim(Some(tmp.path())), Claim::Toggled));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !slow.close_requested() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(slow.close_requested());
        let (closed, was_closed) = mpsc::channel();
        slow.on_close_request(move || closed.send(()).unwrap());
        was_closed.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let tmp = TempDir::new();
        // A socket file nobody listens on, as left behind by a crash.
        drop(UnixListener::bind(socket(&tmp)).unwrap());
        assert!(socket(&tmp).exists());
        assert!(granted(claim(Some(tmp.path()))).owned.is_some());

        tmp.write("plain/sengi-quick.sock", "not a socket");
        let plain = tmp.path().join("plain");
        assert!(granted(claim(Some(&plain))).owned.is_some());
    }

    #[test]
    fn a_panel_never_removes_a_socket_it_did_not_create() {
        let tmp = TempDir::new();
        let hung = granted(claim(Some(tmp.path())));
        // Another panel took this one for dead and put its own socket at
        // the path.
        std::fs::remove_file(socket(&tmp)).unwrap();
        let successor = UnixListener::bind(socket(&tmp)).unwrap();
        let theirs = identity(&socket(&tmp));
        assert_ne!(theirs, hung.owned.as_ref().map(|o| o.identity));

        drop(hung);
        assert_eq!(identity(&socket(&tmp)), theirs);
        drop(successor);
    }

    #[test]
    fn two_panels_starting_together_do_not_both_open() {
        let tmp = TempDir::new();
        for round in 0..40 {
            let barrier = Arc::new(Barrier::new(2));
            let threads: Vec<_> = (0..2)
                .map(|_| {
                    let (barrier, dir) = (barrier.clone(), tmp.path().to_path_buf());
                    std::thread::spawn(move || {
                        barrier.wait();
                        claim(Some(&dir))
                    })
                })
                .collect();
            let claims: Vec<Claim> = threads.into_iter().map(|t| t.join().unwrap()).collect();
            let opened = claims
                .iter()
                .filter(|c| matches!(c, Claim::Granted(_)))
                .count();
            assert_eq!(opened, 1, "round {round}");
            drop(claims);
            assert!(!socket(&tmp).exists(), "round {round}");
        }
    }

    #[test]
    fn strangers_on_the_socket_do_not_close_the_panel() {
        let tmp = TempDir::new();
        let (closed, was_closed) = mpsc::channel();
        let instance = granted(claim(Some(tmp.path())));
        instance.on_close_request(move || closed.send(()).unwrap());
        drop(UnixStream::connect(socket(&tmp)).unwrap());
        UnixStream::connect(socket(&tmp))
            .unwrap()
            .write_all(b"x")
            .unwrap();
        assert!(was_closed.recv_timeout(Duration::from_millis(300)).is_err());
        assert!(!instance.close_requested());
        assert!(matches!(claim(Some(tmp.path())), Claim::Toggled));
        was_closed.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn without_a_runtime_directory_everyone_is_granted() {
        let first = granted(claim(None));
        let second = granted(claim(None));
        first.on_close_request(|| {});
        assert!(!second.close_requested());

        let tmp = TempDir::new();
        let missing = tmp.path().join("missing");
        assert!(granted(claim(Some(&missing))).owned.is_none());
    }
}
