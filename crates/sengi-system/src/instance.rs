//! One panel at a time. The open panel listens on a socket in
//! `$XDG_RUNTIME_DIR`; a second invocation of the same program asks it to
//! close instead of stacking another panel on top.
//!
//! Every invocation is of some kind, a byte ([`Wire::answer`]). Asking
//! while the same kind is open is a toggle: the open one closes and the
//! new one does not appear, which is what a key binding wants. Asking
//! while another kind is open (the session menu while the application
//! search is open) replaces it.
//!
//! Who owns the socket path is settled under a lock file, so two panels
//! starting together cannot both take it, and a panel only ever removes
//! the socket it created.
//!
//! A program that keeps a window, not a panel, wants the opposite: the
//! window that is open stays, and a second invocation hands it a short
//! message, such as the page to show, and leaves. That is [`deliver`],
//! with the same care about the socket path.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// An open panel answers from a thread that does nothing else, so one that
/// stays silent this long is hung or gone.
const REPLY_TIMEOUT: Duration = Duration::from_millis(500);

/// How the invocations of one program find and talk to each other. Each
/// side sends a single byte.
#[derive(Debug, Clone, Copy)]
pub struct Wire<'a> {
    /// File name of the socket under the runtime directory.
    pub socket_name: &'a str,
    /// File name of the lock that guards the socket path.
    pub lock_name: &'a str,
    /// What this invocation sends to ask the open one to close.
    pub ask: u8,
    /// What this invocation answers once it is the open one. A later
    /// invocation that would answer the same takes the answer as a toggle.
    pub answer: u8,
    /// The requests taken as coming from another invocation of the same
    /// program. A connection that sends anything else is ignored.
    pub accepted: &'a [u8],
}

/// The outcome of [`claim`].
pub enum Claim {
    /// This process may open the panel.
    Granted(Instance),
    /// The same kind of panel was open and has been told to close. This
    /// process should exit without opening one.
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

/// Ask the open panel, if there is one, to close. Returns its answer.
fn ask_to_close(path: &Path, ask: u8) -> Option<u8> {
    let mut stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(REPLY_TIMEOUT)).ok()?;
    stream.write_all(&[ask]).ok()?;
    let mut reply = [0u8; 1];
    stream.read_exact(&mut reply).ok()?;
    Some(reply[0])
}

/// Become the one open panel, closing any that is already open. Without a
/// usable runtime directory there is nowhere to meet, and every invocation
/// is granted.
pub fn claim(runtime_dir: Option<&Path>, wire: &Wire) -> Claim {
    let unowned = || {
        Claim::Granted(Instance {
            owned: None,
            slot: Arc::default(),
        })
    };
    let Some(dir) = runtime_dir else {
        return unowned();
    };
    let socket = dir.join(wire.socket_name);
    let lock_path = dir.join(wire.lock_name);
    // Held until this function returns: asking, removing and binding are one
    // step as far as other invocations can tell.
    let Some(_guard) = lock(&lock_path) else {
        return unowned();
    };

    let open = ask_to_close(&socket, wire.ask);
    // Whether a panel just agreed to close or nothing answered at all, the
    // file at the path is finished with.
    let _ = std::fs::remove_file(&socket);
    if open == Some(wire.answer) {
        return Claim::Toggled;
    }

    let Ok(listener) = UnixListener::bind(&socket) else {
        return unowned();
    };
    let Some(identity) = identity(&socket) else {
        return unowned();
    };
    let slot: Arc<Mutex<Slot>> = Arc::default();
    // Listening starts now, not when the panel is ready: a program that is
    // slow to start must still answer, or it would be taken for dead.
    std::thread::spawn({
        let slot = slot.clone();
        let (answer, accepted) = (wire.answer, wire.accepted.to_vec());
        move || serve(listener, answer, &accepted, &slot)
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

/// Answer the first invocation that asks, then have this one close.
fn serve(listener: UnixListener, reply: u8, accepted: &[u8], shared: &Mutex<Slot>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
        let _ = stream.set_write_timeout(Some(REPLY_TIMEOUT));
        let mut request = [0u8; 1];
        // Something that connects and says nothing, or something else, is
        // not another invocation of this program.
        if stream.read_exact(&mut request).is_err() || !accepted.contains(&request[0]) {
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

/// How the invocations of a program that keeps one window find each
/// other. A message is a line: a few bytes and a newline.
#[derive(Debug, Clone, Copy)]
pub struct Post<'a> {
    /// File name of the socket under the runtime directory.
    pub socket_name: &'a str,
    /// File name of the lock that guards the socket path.
    pub lock_name: &'a str,
    /// The longest message taken, in bytes, without its newline.
    pub max_len: usize,
    /// Whether a message is one this program sends. The open window
    /// runs it on what arrived; a connection that sends anything else,
    /// or too much, or nothing, is ignored.
    pub valid: fn(&[u8]) -> bool,
}

/// What the open window answers once a message is its to act on.
const TAKEN: u8 = b'k';

/// The outcome of [`deliver`].
pub enum Delivery {
    /// This process is the window. Later messages arrive in the mailbox.
    Granted(Mailbox),
    /// A window was open and has the message. This process should exit.
    Forwarded,
}

/// Held by the process that owns the window, for as long as it lives.
pub struct Mailbox {
    owned: Option<Owned>,
    letters: Arc<Mutex<Letters>>,
}

type Wake = Box<dyn Fn() + Send>;

#[derive(Default)]
struct Letters {
    /// A message the window has not acted on yet. A newer one replaces it.
    pending: Option<Vec<u8>>,
    /// The window has closed for good. Nothing more is acknowledged.
    retired: bool,
    /// Tells the window a message is waiting.
    wake: Option<Wake>,
}

fn letters(letters: &Mutex<Letters>) -> MutexGuard<'_, Letters> {
    letters
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Hand `message` to the open window, if there is one that takes it.
fn forward(path: &Path, message: &[u8]) -> bool {
    let ask = || {
        let mut stream = UnixStream::connect(path).ok()?;
        stream.set_read_timeout(Some(REPLY_TIMEOUT)).ok()?;
        stream.set_write_timeout(Some(REPLY_TIMEOUT)).ok()?;
        let mut line = message.to_vec();
        line.push(b'\n');
        stream.write_all(&line).ok()?;
        let mut reply = [0u8; 1];
        stream.read_exact(&mut reply).ok()?;
        Some(reply[0] == TAKEN)
    };
    ask().unwrap_or(false)
}

/// Become the one window, or pass `message` to the one that is open.
/// Without a usable runtime directory there is nowhere to meet, and every
/// invocation is granted.
///
/// `message` has to be one `post.valid` accepts, without a newline and
/// no longer than `post.max_len`; one that is not is sent as the empty
/// message.
pub fn deliver(runtime_dir: Option<&Path>, post: &Post, message: &[u8]) -> Delivery {
    let unowned = || {
        Delivery::Granted(Mailbox {
            owned: None,
            letters: Arc::default(),
        })
    };
    let Some(dir) = runtime_dir else {
        return unowned();
    };
    let sendable =
        message.len() <= post.max_len && !message.contains(&b'\n') && (post.valid)(message);
    let message = if sendable { message } else { &[] };
    let socket = dir.join(post.socket_name);
    let lock_path = dir.join(post.lock_name);
    // Held until this function returns: asking, removing and binding are one
    // step as far as other invocations can tell.
    let Some(_guard) = lock(&lock_path) else {
        return unowned();
    };

    if forward(&socket, message) {
        return Delivery::Forwarded;
    }
    // Nothing took the message: the file at the path, if any, was left by
    // a crash, or belongs to a window that has retired or hung.
    let _ = std::fs::remove_file(&socket);

    let Ok(listener) = UnixListener::bind(&socket) else {
        return unowned();
    };
    let Some(identity) = identity(&socket) else {
        return unowned();
    };
    let letters: Arc<Mutex<Letters>> = Arc::default();
    // Listening starts now, not when the window is up: one that is slow
    // to start must still answer, or it would be taken for dead.
    std::thread::spawn({
        let letters = letters.clone();
        let (max_len, valid) = (post.max_len, post.valid);
        move || receive(listener, max_len, valid, &letters)
    });
    Delivery::Granted(Mailbox {
        owned: Some(Owned {
            socket,
            lock: lock_path,
            identity,
        }),
        letters,
    })
}

/// One message: a line of at most `max_len` bytes that `valid` accepts.
/// Anything else, or silence, is not another invocation of this program.
fn read_message(
    stream: &mut UnixStream,
    max_len: usize,
    valid: fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).ok()?;
        if byte[0] == b'\n' {
            break;
        }
        if line.len() >= max_len {
            return None;
        }
        line.push(byte[0]);
    }
    valid(&line).then_some(line)
}

fn receive(
    listener: UnixListener,
    max_len: usize,
    valid: fn(&[u8]) -> bool,
    shared: &Mutex<Letters>,
) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
        let _ = stream.set_write_timeout(Some(REPLY_TIMEOUT));
        let Some(message) = read_message(&mut stream, max_len, valid) else {
            continue;
        };
        let mut letters = letters(shared);
        if letters.retired {
            // No reply: the asker opens a window of its own.
            break;
        }
        // Recorded before the reply goes out. Once the other side hears
        // from us it exits, and the message is ours to honour.
        letters.pending = Some(message);
        if let Some(wake) = &letters.wake {
            wake();
        }
        drop(letters);
        let _ = stream.write_all(&[TAKEN]);
    }
}

impl Mailbox {
    /// Call `wake` whenever a message arrives. It runs on another thread,
    /// with the message left for [`Mailbox::take`]; at once, if one has
    /// come already.
    pub fn on_message(&self, wake: impl Fn() + Send + 'static) {
        let mut shared = letters(&self.letters);
        if shared.pending.is_some() {
            wake();
        }
        shared.wake = Some(Box::new(wake));
    }

    /// The message waiting to be acted on, if any.
    pub fn take(&self) -> Option<Vec<u8>> {
        letters(&self.letters).pending.take()
    }

    /// The window has closed. If a message slipped in while it was
    /// closing, that message is returned and the window must come back
    /// for it; otherwise this mailbox stops taking messages and the next
    /// invocation opens its own window.
    pub fn retire(&self) -> Option<Vec<u8>> {
        let mut shared = letters(&self.letters);
        let late = shared.pending.take();
        shared.retired = late.is_none();
        late
    }
}

impl Drop for Mailbox {
    fn drop(&mut self) {
        letters(&self.letters).retired = true;
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

/// The tests that came with the settings application, whose window stays
/// and is handed the page a later invocation asked for.
#[cfg(test)]
mod window_tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::sync::{mpsc, Barrier};

    const SOCKET_NAME: &str = "sengi-settings.sock";

    fn page(message: &[u8]) -> bool {
        [
            &b""[..],
            b"network",
            b"sound",
            b"display",
            b"power",
            b"about",
        ]
        .contains(&message)
    }

    fn deliver(runtime_dir: Option<&Path>, message: &str) -> Delivery {
        super::deliver(
            runtime_dir,
            &Post {
                socket_name: SOCKET_NAME,
                lock_name: "sengi-settings.lock",
                max_len: 32,
                valid: page,
            },
            message.as_bytes(),
        )
    }

    fn granted(delivery: Delivery) -> Mailbox {
        match delivery {
            Delivery::Granted(mailbox) => mailbox,
            Delivery::Forwarded => panic!("expected to be granted"),
        }
    }

    fn socket(tmp: &TempDir) -> PathBuf {
        tmp.path().join(SOCKET_NAME)
    }

    fn message(text: &str) -> Option<Vec<u8>> {
        Some(text.as_bytes().to_vec())
    }

    #[test]
    fn the_first_window_is_granted_and_cleans_up_after_itself() {
        let tmp = TempDir::new();
        let mailbox = granted(deliver(Some(tmp.path()), ""));
        assert!(mailbox.owned.is_some());
        assert!(socket(&tmp).exists());
        assert_eq!(mailbox.take(), None);
        drop(mailbox);
        assert!(!socket(&tmp).exists());
    }

    #[test]
    fn a_second_invocation_hands_over_its_page_and_does_not_open() {
        let tmp = TempDir::new();
        let (woken, was_woken) = mpsc::channel();
        let first = granted(deliver(Some(tmp.path()), ""));
        first.on_message(move || woken.send(()).unwrap());

        assert!(matches!(
            deliver(Some(tmp.path()), "sound"),
            Delivery::Forwarded
        ));
        was_woken.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first.take(), message("sound"));
        assert_eq!(first.take(), None);

        // And again, any number of times; with no page it only raises.
        assert!(matches!(deliver(Some(tmp.path()), ""), Delivery::Forwarded));
        was_woken.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first.take(), message(""));
        assert!(socket(&tmp).exists());
    }

    #[test]
    fn a_request_that_came_before_the_window_was_up_is_kept() {
        let tmp = TempDir::new();
        let slow = granted(deliver(Some(tmp.path()), ""));
        assert!(matches!(
            deliver(Some(tmp.path()), "display"),
            Delivery::Forwarded
        ));
        assert!(matches!(
            deliver(Some(tmp.path()), "about"),
            Delivery::Forwarded
        ));
        let (woken, was_woken) = mpsc::channel();
        slow.on_message(move || woken.send(()).unwrap());
        was_woken.recv_timeout(Duration::from_secs(5)).unwrap();
        // The newest is the one that counts.
        assert_eq!(slow.take(), message("about"));
    }

    #[test]
    fn a_request_that_arrives_as_the_window_closes_brings_it_back() {
        let tmp = TempDir::new();
        let first = granted(deliver(Some(tmp.path()), ""));
        assert!(matches!(
            deliver(Some(tmp.path()), "power"),
            Delivery::Forwarded
        ));
        // The window closed without having looked at it.
        assert_eq!(first.retire(), message("power"));
        // It came back, so it still takes requests.
        assert!(matches!(deliver(Some(tmp.path()), ""), Delivery::Forwarded));
        assert_eq!(first.retire(), message(""));
    }

    #[test]
    fn a_retired_window_takes_nothing_and_the_next_invocation_opens() {
        let tmp = TempDir::new();
        let first = granted(deliver(Some(tmp.path()), ""));
        assert_eq!(first.retire(), None);
        // Still alive, socket still there, but closed for business.
        let second = granted(deliver(Some(tmp.path()), "sound"));
        assert!(second.owned.is_some());
        assert_eq!(first.take(), None);
        // The first going away does not disturb the second's socket.
        let theirs = identity(&socket(&tmp));
        drop(first);
        assert_eq!(identity(&socket(&tmp)), theirs);
        assert!(matches!(deliver(Some(tmp.path()), ""), Delivery::Forwarded));
        drop(second);
        assert!(!socket(&tmp).exists());
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let tmp = TempDir::new();
        // A socket file nobody listens on, as left behind by a crash.
        drop(UnixListener::bind(socket(&tmp)).unwrap());
        assert!(socket(&tmp).exists());
        assert!(granted(deliver(Some(tmp.path()), "")).owned.is_some());

        tmp.write("plain/sengi-settings.sock", "not a socket");
        let plain = tmp.path().join("plain");
        assert!(granted(deliver(Some(&plain), "")).owned.is_some());
    }

    #[test]
    fn a_window_never_removes_a_socket_it_did_not_create() {
        let tmp = TempDir::new();
        let hung = granted(deliver(Some(tmp.path()), ""));
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
                        deliver(Some(&dir), "network")
                    })
                })
                .collect();
            let all: Vec<Delivery> = threads.into_iter().map(|t| t.join().unwrap()).collect();
            let opened: Vec<&Mailbox> = all
                .iter()
                .filter_map(|delivery| match delivery {
                    Delivery::Granted(mailbox) => Some(mailbox),
                    Delivery::Forwarded => None,
                })
                .collect();
            assert_eq!(opened.len(), 1, "round {round}");
            // And the loser's page reached the winner.
            assert_eq!(opened[0].take(), message("network"));
            drop(all);
            assert!(!socket(&tmp).exists(), "round {round}");
        }
    }

    #[test]
    fn strangers_on_the_socket_are_ignored() {
        let tmp = TempDir::new();
        let mailbox = granted(deliver(Some(tmp.path()), ""));
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
        assert_eq!(mailbox.take(), None);
        // A real request still gets through afterwards.
        assert!(matches!(
            deliver(Some(tmp.path()), "about"),
            Delivery::Forwarded
        ));
        assert_eq!(mailbox.take(), message("about"));
    }

    #[test]
    fn a_message_the_program_would_not_take_is_sent_as_none() {
        let tmp = TempDir::new();
        let mailbox = granted(deliver(Some(tmp.path()), ""));
        for odd in ["nowhere", "sound\nabout", &"s".repeat(64)] {
            assert!(matches!(
                deliver(Some(tmp.path()), odd),
                Delivery::Forwarded
            ));
            assert_eq!(mailbox.take(), message(""), "{odd:?}");
        }
    }

    #[test]
    fn without_a_runtime_directory_everyone_is_granted() {
        let first = granted(deliver(None, ""));
        let second = granted(deliver(None, "sound"));
        assert_eq!(first.take(), None);
        assert_eq!(second.retire(), None);

        let tmp = TempDir::new();
        let missing = tmp.path().join("missing");
        assert!(granted(deliver(Some(&missing), "")).owned.is_none());
    }
}

/// The tests that came with the launcher, which has two kinds of panel.
#[cfg(test)]
mod launcher_tests {
    use super::*;

    const SOCKET_NAME: &str = "sengi-launcher.sock";

    #[derive(Clone, Copy)]
    enum Mode {
        Apps,
        Menu,
    }

    fn claim(runtime_dir: Option<&Path>, mode: Mode) -> Claim {
        let byte = match mode {
            Mode::Apps => b'a',
            Mode::Menu => b'm',
        };
        super::claim(
            runtime_dir,
            &Wire {
                socket_name: SOCKET_NAME,
                lock_name: "sengi-launcher.lock",
                ask: byte,
                answer: byte,
                accepted: b"am",
            },
        )
    }
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

/// The tests that came with quick settings, which has one kind of panel
/// and answers with another byte than it asks with.
#[cfg(test)]
mod quick_tests {
    use super::*;

    const SOCKET_NAME: &str = "sengi-quick.sock";

    fn claim(runtime_dir: Option<&Path>) -> Claim {
        super::claim(
            runtime_dir,
            &Wire {
                socket_name: SOCKET_NAME,
                lock_name: "sengi-quick.lock",
                ask: b'c',
                answer: b'k',
                accepted: b"c",
            },
        )
    }
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
