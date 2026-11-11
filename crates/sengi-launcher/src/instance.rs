//! One panel at a time. The open launcher listens on a socket in
//! `$XDG_RUNTIME_DIR`; a second invocation asks it to close instead of
//! stacking another panel on top.
//!
//! Asking for the same kind of panel that is already open is a toggle: the
//! open one closes and the new one does not appear, which is what a key
//! binding wants. Asking for the other kind (the session menu while the
//! application search is open) replaces it.
//!
//! Who owns the socket path is settled under a lock file, so two launchers
//! starting together cannot both take it, and a launcher only ever removes
//! the socket it created.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::cli::Mode;

const SOCKET_NAME: &str = "sengi-launcher.sock";
const LOCK_NAME: &str = "sengi-launcher.lock";
/// A launcher answers from a thread that does nothing else, so one that
/// stays silent this long is hung or gone.
const REPLY_TIMEOUT: Duration = Duration::from_millis(500);

pub enum Claim {
    /// This process may open the panel.
    Granted(Instance),
    /// The same panel was open and has been told to close.
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
    /// socket path belongs to whoever asked.
    requested: bool,
    close: Option<Box<dyn FnOnce() + Send>>,
}

fn mode_byte(mode: Mode) -> u8 {
    match mode {
        Mode::Apps => b'a',
        Mode::Menu => b'm',
    }
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

/// Ask the open panel, if there is one, to close. Returns its mode.
fn ask_to_close(path: &Path, mode: Mode) -> Option<u8> {
    let mut stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(REPLY_TIMEOUT)).ok()?;
    stream.write_all(&[mode_byte(mode)]).ok()?;
    let mut reply = [0u8; 1];
    stream.read_exact(&mut reply).ok()?;
    Some(reply[0])
}

/// Become the one launcher, closing any that is already open. Without a
/// usable runtime directory there is nowhere to meet, and every launcher is
/// granted.
pub fn claim(runtime_dir: Option<&Path>, mode: Mode) -> Claim {
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
    // step as far as other launchers can tell.
    let Some(_guard) = lock(&lock_path) else {
        return unowned();
    };

    let open = ask_to_close(&socket, mode);
    // Whether a panel just agreed to close or nothing answered at all, the
    // file at the path is finished with.
    let _ = std::fs::remove_file(&socket);
    if open == Some(mode_byte(mode)) {
        return Claim::Toggled;
    }

    let Ok(listener) = UnixListener::bind(&socket) else {
        return unowned();
    };
    let Some(identity) = identity(&socket) else {
        return unowned();
    };
    let slot: Arc<Mutex<Slot>> = Arc::default();
    // Listening starts now, not when the panel is ready: a launcher that is
    // slow to start must still answer, or it would be taken for dead.
    std::thread::spawn({
        let slot = slot.clone();
        move || serve(listener, mode_byte(mode), &slot)
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

/// Answer the first launcher that asks, then have this one close.
fn serve(listener: UnixListener, reply: u8, shared: &Mutex<Slot>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
        let _ = stream.set_write_timeout(Some(REPLY_TIMEOUT));
        let mut request = [0u8; 1];
        // Something that connects and says nothing is not a launcher.
        if stream.read_exact(&mut request).is_err() {
            continue;
        }
        // Marked before the reply goes out: once the other side hears from
        // us it treats the path as its own.
        slot(shared).requested = true;
        let _ = stream.write_all(&[reply]);
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
        // Checked under the lock. After a request the path is the asker's,
        // and if we were taken for dead it holds someone else's socket.
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
    use std::time::Instant;

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
    fn the_first_launcher_is_granted_and_leaves_a_socket() {
        let tmp = TempDir::new();
        let instance = granted(claim(Some(tmp.path()), Mode::Apps));
        assert!(instance.owned.is_some());
        assert!(socket(&tmp).exists());
        assert!(!instance.close_requested());
        drop(instance);
        assert!(!socket(&tmp).exists());
    }

    #[test]
    fn the_same_panel_again_is_a_toggle() {
        let tmp = TempDir::new();
        let (closed, was_closed) = mpsc::channel();
        let first = granted(claim(Some(tmp.path()), Mode::Apps));
        first.on_close_request(move || closed.send(()).unwrap());

        assert!(matches!(
            claim(Some(tmp.path()), Mode::Apps),
            Claim::Toggled
        ));
        was_closed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(first.close_requested());
        assert!(!socket(&tmp).exists());

        // A third press opens a fresh panel, even with the first not yet
        // gone, and the first going away does not disturb it.
        let third = granted(claim(Some(tmp.path()), Mode::Apps));
        drop(first);
        assert!(socket(&tmp).exists());
        drop(third);
        assert!(!socket(&tmp).exists());
    }

    #[test]
    fn a_launcher_still_starting_up_answers_and_learns_of_it_later() {
        let tmp = TempDir::new();
        // Granted, but busy reading its menu: no callback registered yet.
        let slow = granted(claim(Some(tmp.path()), Mode::Menu));

        let asked = Instant::now();
        assert!(matches!(
            claim(Some(tmp.path()), Mode::Menu),
            Claim::Toggled
        ));
        assert!(asked.elapsed() < REPLY_TIMEOUT, "it was taken for dead");

        // The reply is sent before the flag can be read from here.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !slow.close_requested() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(slow.close_requested());
        let (closed, was_closed) = mpsc::channel();
        slow.on_close_request(move || closed.send(()).unwrap());
        was_closed.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn a_different_panel_replaces_the_open_one() {
        let tmp = TempDir::new();
        let (closed, was_closed) = mpsc::channel();
        let apps = granted(claim(Some(tmp.path()), Mode::Apps));
        apps.on_close_request(move || closed.send(()).unwrap());

        let menu = granted(claim(Some(tmp.path()), Mode::Menu));
        was_closed.recv_timeout(Duration::from_secs(5)).unwrap();
        // The replaced launcher exits; the socket there now is the menu's.
        drop(apps);
        assert_eq!(
            identity(&socket(&tmp)),
            menu.owned.as_ref().map(|o| o.identity)
        );

        // And the menu in turn answers as a menu.
        let (closed, was_closed) = mpsc::channel();
        menu.on_close_request(move || closed.send(()).unwrap());
        assert!(matches!(
            claim(Some(tmp.path()), Mode::Menu),
            Claim::Toggled
        ));
        was_closed.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let tmp = TempDir::new();
        // A socket file nobody listens on, as left behind by a crash.
        drop(UnixListener::bind(socket(&tmp)).unwrap());
        assert!(socket(&tmp).exists());
        let instance = granted(claim(Some(tmp.path()), Mode::Menu));
        assert!(instance.owned.is_some());

        tmp.write("plain/sengi-launcher.sock", "not a socket");
        let plain = tmp.path().join("plain");
        assert!(granted(claim(Some(&plain), Mode::Apps)).owned.is_some());
    }

    #[test]
    fn a_launcher_never_removes_a_socket_it_did_not_create() {
        let tmp = TempDir::new();
        let hung = granted(claim(Some(tmp.path()), Mode::Apps));
        // Another launcher took this one for dead and put its own socket at
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
    fn two_launchers_starting_together_do_not_both_open() {
        let tmp = TempDir::new();
        for round in 0..40 {
            let barrier = Arc::new(Barrier::new(2));
            let threads: Vec<_> = (0..2)
                .map(|_| {
                    let (barrier, dir) = (barrier.clone(), tmp.path().to_path_buf());
                    std::thread::spawn(move || {
                        barrier.wait();
                        claim(Some(&dir), Mode::Apps)
                    })
                })
                .collect();
            let claims: Vec<Claim> = threads.into_iter().map(|t| t.join().unwrap()).collect();
            let opened = claims
                .iter()
                .filter(|c| matches!(c, Claim::Granted(_)))
                .count();
            assert_eq!(opened, 1, "round {round}");
            // The one that opened was toggled shut by the other, which left
            // the path free for the next press.
            drop(claims);
            assert!(!socket(&tmp).exists(), "round {round}");
        }
    }

    #[test]
    fn silent_connections_do_not_close_the_panel() {
        let tmp = TempDir::new();
        let (closed, was_closed) = mpsc::channel();
        let instance = granted(claim(Some(tmp.path()), Mode::Apps));
        instance.on_close_request(move || closed.send(()).unwrap());
        drop(UnixStream::connect(socket(&tmp)).unwrap());
        assert!(was_closed.recv_timeout(Duration::from_millis(300)).is_err());
        assert!(!instance.close_requested());
        assert!(matches!(
            claim(Some(tmp.path()), Mode::Apps),
            Claim::Toggled
        ));
        was_closed.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn without_a_runtime_directory_everyone_is_granted() {
        let first = granted(claim(None, Mode::Apps));
        let second = granted(claim(None, Mode::Apps));
        first.on_close_request(|| {});
        assert!(!second.close_requested());

        let tmp = TempDir::new();
        let missing = tmp.path().join("missing");
        assert!(granted(claim(Some(&missing), Mode::Apps)).owned.is_none());
    }
}
