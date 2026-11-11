//! One Settings window at a time. The open window listens on a socket in
//! `$XDG_RUNTIME_DIR`; a second invocation hands it the page it was asked
//! for and exits, so the quick settings panel and the bar can send the user
//! to a page whether or not Settings is already open.
//!
//! The scheme is sengi-launcher's and sengi-quick's, with one difference:
//! there the newcomer takes over and the old panel closes, here the old
//! window stays and the newcomer leaves. The same care applies. Who owns
//! the socket path is settled under a lock file, so two starting together
//! cannot both take it; a window only ever removes the socket it created;
//! and a request is only acknowledged while the window can still act on
//! it, so none is lost to a window that was closing as it arrived.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::cli::Page;

const SOCKET_NAME: &str = "sengi-settings.sock";
const LOCK_NAME: &str = "sengi-settings.lock";
/// The window answers from a thread that does nothing else, so one that
/// stays silent this long is hung or gone.
const REPLY_TIMEOUT: Duration = Duration::from_millis(500);
/// A request is a page name and a newline, or just the newline.
const MAX_REQUEST: usize = 32;
const TAKEN: u8 = b'k';

pub enum Claim {
    /// This process is the Settings window.
    Granted(Instance),
    /// A window was open and has been given the page.
    Forwarded,
}

/// Held by the process that owns the window, for as long as it is open.
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

/// A later invocation's request: bring the window forward, at a page if
/// it named one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request(pub Option<Page>);

type Wake = Box<dyn Fn() + Send>;

#[derive(Default)]
struct Slot {
    /// A request the window has not acted on yet. A newer one replaces it.
    pending: Option<Request>,
    /// The window has closed for good. Nothing more is acknowledged.
    retired: bool,
    /// Tells the window a request is waiting.
    wake: Option<Wake>,
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

/// Hand `page` to the open window, if there is one that takes it.
fn forward(path: &Path, page: Option<Page>) -> bool {
    let ask = || {
        let mut stream = UnixStream::connect(path).ok()?;
        stream.set_read_timeout(Some(REPLY_TIMEOUT)).ok()?;
        stream.set_write_timeout(Some(REPLY_TIMEOUT)).ok()?;
        let line = format!("{}\n", page.map_or("", Page::name));
        stream.write_all(line.as_bytes()).ok()?;
        let mut reply = [0u8; 1];
        stream.read_exact(&mut reply).ok()?;
        Some(reply[0] == TAKEN)
    };
    ask().unwrap_or(false)
}

/// Become the one window, or pass `page` to the one that is open. Without
/// a usable runtime directory there is nowhere to meet, and every
/// invocation is granted.
pub fn claim(runtime_dir: Option<&Path>, page: Option<Page>) -> Claim {
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
    // step as far as other invocations can tell.
    let Some(_guard) = lock(&lock_path) else {
        return unowned();
    };

    if forward(&socket, page) {
        return Claim::Forwarded;
    }
    // Nothing took the request: the file at the path, if any, was left by
    // a crash, or belongs to a window that has retired or hung.
    let _ = std::fs::remove_file(&socket);

    let Ok(listener) = UnixListener::bind(&socket) else {
        return unowned();
    };
    let Some(identity) = identity(&socket) else {
        return unowned();
    };
    let slot: Arc<Mutex<Slot>> = Arc::default();
    // Listening starts now, not when the window is up: one that is slow
    // to start must still answer, or it would be taken for dead.
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

/// One request: a line holding a page name or nothing. Anything else, or
/// silence, is not a second sengi-settings.
fn read_request(stream: &mut UnixStream) -> Option<Request> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).ok()?;
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() > MAX_REQUEST {
            return None;
        }
    }
    if line.is_empty() {
        return Some(Request(None));
    }
    let name = std::str::from_utf8(&line).ok()?;
    Page::ALL
        .into_iter()
        .find(|page| page.name() == name)
        .map(|page| Request(Some(page)))
}

fn serve(listener: UnixListener, shared: &Mutex<Slot>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
        let _ = stream.set_write_timeout(Some(REPLY_TIMEOUT));
        let Some(request) = read_request(&mut stream) else {
            continue;
        };
        let mut slot = slot(shared);
        if slot.retired {
            // No reply: the asker opens a window of its own.
            break;
        }
        // Recorded before the reply goes out. Once the other side hears
        // from us it exits, and the request is ours to honour.
        slot.pending = Some(request);
        if let Some(wake) = &slot.wake {
            wake();
        }
        drop(slot);
        let _ = stream.write_all(&[TAKEN]);
    }
}

impl Instance {
    /// Call `wake` whenever a request arrives. It runs on another thread,
    /// with the request left for [`Instance::take`]; at once, if one has
    /// come already.
    pub fn on_request(&self, wake: impl Fn() + Send + 'static) {
        let mut shared = slot(&self.slot);
        if shared.pending.is_some() {
            wake();
        }
        shared.wake = Some(Box::new(wake));
    }

    /// The request waiting to be acted on, if any.
    pub fn take(&self) -> Option<Request> {
        slot(&self.slot).pending.take()
    }

    /// The window has closed. If a request slipped in while it was
    /// closing, that request is returned and the window must come back
    /// for it; otherwise this instance stops taking requests and the next
    /// invocation opens its own window.
    pub fn retire(&self) -> Option<Request> {
        let mut shared = slot(&self.slot);
        let late = shared.pending.take();
        shared.retired = late.is_none();
        late
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        slot(&self.slot).retired = true;
        let Some(owned) = self.owned.take() else {
            return;
        };
        let Some(_guard) = lock(&owned.lock) else {
            return;
        };
        // Checked under the lock. If we were taken for dead the path holds
        // someone else's socket.
        if identity(&owned.socket) == Some(owned.identity) {
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
            Claim::Forwarded => panic!("expected to be granted"),
        }
    }

    fn socket(tmp: &TempDir) -> PathBuf {
        tmp.path().join(SOCKET_NAME)
    }

    #[test]
    fn the_first_window_is_granted_and_cleans_up_after_itself() {
        let tmp = TempDir::new();
        let instance = granted(claim(Some(tmp.path()), None));
        assert!(instance.owned.is_some());
        assert!(socket(&tmp).exists());
        assert_eq!(instance.take(), None);
        drop(instance);
        assert!(!socket(&tmp).exists());
    }

    #[test]
    fn a_second_invocation_hands_over_its_page_and_does_not_open() {
        let tmp = TempDir::new();
        let (woken, was_woken) = mpsc::channel();
        let first = granted(claim(Some(tmp.path()), None));
        first.on_request(move || woken.send(()).unwrap());

        assert!(matches!(
            claim(Some(tmp.path()), Some(Page::Sound)),
            Claim::Forwarded
        ));
        was_woken.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first.take(), Some(Request(Some(Page::Sound))));
        assert_eq!(first.take(), None);

        // And again, any number of times; with no page it only raises.
        assert!(matches!(claim(Some(tmp.path()), None), Claim::Forwarded));
        was_woken.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first.take(), Some(Request(None)));
        assert!(socket(&tmp).exists());
    }

    #[test]
    fn a_request_that_came_before_the_window_was_up_is_kept() {
        let tmp = TempDir::new();
        let slow = granted(claim(Some(tmp.path()), None));
        assert!(matches!(
            claim(Some(tmp.path()), Some(Page::Display)),
            Claim::Forwarded
        ));
        assert!(matches!(
            claim(Some(tmp.path()), Some(Page::About)),
            Claim::Forwarded
        ));
        let (woken, was_woken) = mpsc::channel();
        slow.on_request(move || woken.send(()).unwrap());
        was_woken.recv_timeout(Duration::from_secs(5)).unwrap();
        // The newest is the one that counts.
        assert_eq!(slow.take(), Some(Request(Some(Page::About))));
    }

    #[test]
    fn a_request_that_arrives_as_the_window_closes_brings_it_back() {
        let tmp = TempDir::new();
        let first = granted(claim(Some(tmp.path()), None));
        assert!(matches!(
            claim(Some(tmp.path()), Some(Page::Power)),
            Claim::Forwarded
        ));
        // The window closed without having looked at it.
        assert_eq!(first.retire(), Some(Request(Some(Page::Power))));
        // It came back, so it still takes requests.
        assert!(matches!(claim(Some(tmp.path()), None), Claim::Forwarded));
        assert_eq!(first.retire(), Some(Request(None)));
    }

    #[test]
    fn a_retired_window_takes_nothing_and_the_next_invocation_opens() {
        let tmp = TempDir::new();
        let first = granted(claim(Some(tmp.path()), None));
        assert_eq!(first.retire(), None);
        // Still alive, socket still there, but closed for business.
        let second = granted(claim(Some(tmp.path()), Some(Page::Sound)));
        assert!(second.owned.is_some());
        assert_eq!(first.take(), None);
        // The first going away does not disturb the second's socket.
        let theirs = identity(&socket(&tmp));
        drop(first);
        assert_eq!(identity(&socket(&tmp)), theirs);
        assert!(matches!(claim(Some(tmp.path()), None), Claim::Forwarded));
        drop(second);
        assert!(!socket(&tmp).exists());
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let tmp = TempDir::new();
        // A socket file nobody listens on, as left behind by a crash.
        drop(UnixListener::bind(socket(&tmp)).unwrap());
        assert!(socket(&tmp).exists());
        assert!(granted(claim(Some(tmp.path()), None)).owned.is_some());

        tmp.write("plain/sengi-settings.sock", "not a socket");
        let plain = tmp.path().join("plain");
        assert!(granted(claim(Some(&plain), None)).owned.is_some());
    }

    #[test]
    fn a_window_never_removes_a_socket_it_did_not_create() {
        let tmp = TempDir::new();
        let hung = granted(claim(Some(tmp.path()), None));
        // Another invocation took this one for dead and put its own socket
        // at the path.
        std::fs::remove_file(socket(&tmp)).unwrap();
        let successor = UnixListener::bind(socket(&tmp)).unwrap();
        let theirs = identity(&socket(&tmp));
        assert_ne!(theirs, hung.owned.as_ref().map(|o| o.identity));

        drop(hung);
        assert_eq!(identity(&socket(&tmp)), theirs);
        drop(successor);
    }

    #[test]
    fn two_starting_together_open_one_window() {
        let tmp = TempDir::new();
        for round in 0..40 {
            let barrier = Arc::new(Barrier::new(2));
            let threads: Vec<_> = (0..2)
                .map(|_| {
                    let (barrier, dir) = (barrier.clone(), tmp.path().to_path_buf());
                    std::thread::spawn(move || {
                        barrier.wait();
                        claim(Some(&dir), Some(Page::Network))
                    })
                })
                .collect();
            let claims: Vec<Claim> = threads.into_iter().map(|t| t.join().unwrap()).collect();
            let opened: Vec<&Instance> = claims
                .iter()
                .filter_map(|claim| match claim {
                    Claim::Granted(instance) => Some(instance),
                    Claim::Forwarded => None,
                })
                .collect();
            assert_eq!(opened.len(), 1, "round {round}");
            // And the loser's page reached the winner.
            assert_eq!(opened[0].take(), Some(Request(Some(Page::Network))));
            drop(claims);
            assert!(!socket(&tmp).exists(), "round {round}");
        }
    }

    #[test]
    fn strangers_on_the_socket_are_ignored() {
        let tmp = TempDir::new();
        let instance = granted(claim(Some(tmp.path()), None));
        drop(UnixStream::connect(socket(&tmp)).unwrap());
        for junk in [
            &b"x"[..],
            b"nowhere\n",
            b"network",
            &[b'a'; 200],
            b"\xff\xfe\n",
        ] {
            let mut stream = UnixStream::connect(socket(&tmp)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let _ = stream.write_all(junk);
            let _ = stream.shutdown(std::net::Shutdown::Write);
            // No acknowledgement: the stream just ends.
            let mut reply = Vec::new();
            let _ = stream.read_to_end(&mut reply);
            assert!(reply.is_empty(), "{junk:?}");
        }
        assert_eq!(instance.take(), None);
        // A real request still gets through afterwards.
        assert!(matches!(
            claim(Some(tmp.path()), Some(Page::About)),
            Claim::Forwarded
        ));
        assert_eq!(instance.take(), Some(Request(Some(Page::About))));
    }

    #[test]
    fn without_a_runtime_directory_everyone_is_granted() {
        let first = granted(claim(None, None));
        let second = granted(claim(None, Some(Page::Sound)));
        assert_eq!(first.take(), None);
        assert_eq!(second.retire(), None);

        let tmp = TempDir::new();
        let missing = tmp.path().join("missing");
        assert!(granted(claim(Some(&missing), None)).owned.is_none());
    }
}
