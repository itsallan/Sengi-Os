//! One panel at a time. The open launcher listens on a socket in
//! `$XDG_RUNTIME_DIR`; a second invocation asks it to close instead of
//! stacking another panel on top.
//!
//! Asking for the same kind of panel that is already open is a toggle: the
//! open one closes and the new one does not appear, which is what a key
//! binding wants. Asking for the other kind (the session menu while the
//! application search is open) replaces it.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cli::Mode;

const SOCKET_NAME: &str = "sengi-launcher.sock";
const REPLY_TIMEOUT: Duration = Duration::from_millis(500);
/// How long to wait for the panel being replaced to go away.
const HANDOVER_TIMEOUT: Duration = Duration::from_secs(2);

pub enum Claim {
    /// This process may open the panel.
    Granted(Instance),
    /// The same panel was open and has been told to close.
    Toggled,
}

/// Held by the process that owns the panel.
pub struct Instance {
    listener: Option<(UnixListener, PathBuf)>,
    mode: Mode,
}

fn mode_byte(mode: Mode) -> u8 {
    match mode {
        Mode::Apps => b'a',
        Mode::Menu => b'm',
    }
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
/// runtime directory there is nowhere to meet, and every launcher is granted.
pub fn claim(runtime_dir: Option<&Path>, mode: Mode) -> Claim {
    let Some(dir) = runtime_dir else {
        return Claim::Granted(Instance {
            listener: None,
            mode,
        });
    };
    let path = dir.join(SOCKET_NAME);

    if let Some(open) = ask_to_close(&path, mode) {
        if open == mode_byte(mode) {
            return Claim::Toggled;
        }
        // A different panel is closing: wait until it has let go.
        let deadline = Instant::now() + HANDOVER_TIMEOUT;
        while UnixStream::connect(&path).is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // Nobody answered, so whatever is at the path is left over from a crash.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).ok().map(|l| (l, path));
    Claim::Granted(Instance { listener, mode })
}

impl Instance {
    /// Call `close` (on another thread) when a later invocation asks this
    /// one to go away. Only the first such request is served.
    pub fn on_close_request(mut self, close: impl FnOnce() + Send + 'static) {
        let Some((listener, path)) = self.listener.take() else {
            return;
        };
        let reply = mode_byte(self.mode);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
                let _ = stream.set_write_timeout(Some(REPLY_TIMEOUT));
                let mut request = [0u8; 1];
                // Something that connects and says nothing is not a launcher.
                if stream.read_exact(&mut request).is_err() {
                    continue;
                }
                let _ = stream.write_all(&[reply]);
                break;
            }
            // The file goes first: once the listener is gone a replacement
            // may bind the same path, and that file is not ours to remove.
            let _ = std::fs::remove_file(&path);
            drop(listener);
            close();
        });
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Some((_, path)) = self.listener.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Remove the socket of a panel that closed by itself. The listening thread
/// cannot do it, as it is still waiting for a request that never came.
pub fn release(runtime_dir: Option<&Path>) {
    if let Some(dir) = runtime_dir {
        let _ = std::fs::remove_file(dir.join(SOCKET_NAME));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::sync::mpsc;

    fn granted(claim: Claim) -> Instance {
        match claim {
            Claim::Granted(instance) => instance,
            Claim::Toggled => panic!("expected to be granted"),
        }
    }

    #[test]
    fn the_first_launcher_is_granted_and_leaves_a_socket() {
        let tmp = TempDir::new();
        let instance = granted(claim(Some(tmp.path()), Mode::Apps));
        assert!(tmp.path().join(SOCKET_NAME).exists());
        drop(instance);
        assert!(!tmp.path().join(SOCKET_NAME).exists());
    }

    #[test]
    fn the_same_panel_again_is_a_toggle() {
        let tmp = TempDir::new();
        let (closed, was_closed) = mpsc::channel();
        granted(claim(Some(tmp.path()), Mode::Apps)).on_close_request(move || {
            closed.send(()).unwrap();
        });

        assert!(matches!(
            claim(Some(tmp.path()), Mode::Apps),
            Claim::Toggled
        ));
        was_closed.recv_timeout(Duration::from_secs(5)).unwrap();

        // The first one is gone now, so a third press opens a fresh panel.
        let again = granted(claim(Some(tmp.path()), Mode::Apps));
        drop(again);
    }

    #[test]
    fn a_different_panel_replaces_the_open_one() {
        let tmp = TempDir::new();
        let (closed, was_closed) = mpsc::channel();
        granted(claim(Some(tmp.path()), Mode::Apps)).on_close_request(move || {
            closed.send(()).unwrap();
        });

        let menu = granted(claim(Some(tmp.path()), Mode::Menu));
        was_closed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(tmp.path().join(SOCKET_NAME).exists());

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
        drop(UnixListener::bind(tmp.path().join(SOCKET_NAME)).unwrap());
        assert!(tmp.path().join(SOCKET_NAME).exists());
        let instance = granted(claim(Some(tmp.path()), Mode::Menu));
        assert!(instance.listener.is_some());

        tmp.write("plain/sengi-launcher.sock", "not a socket");
        let plain = tmp.path().join("plain");
        assert!(granted(claim(Some(&plain), Mode::Apps)).listener.is_some());
    }

    #[test]
    fn silent_connections_do_not_close_the_panel() {
        let tmp = TempDir::new();
        let (closed, was_closed) = mpsc::channel();
        granted(claim(Some(tmp.path()), Mode::Apps)).on_close_request(move || {
            closed.send(()).unwrap();
        });
        drop(UnixStream::connect(tmp.path().join(SOCKET_NAME)).unwrap());
        assert!(was_closed.recv_timeout(Duration::from_millis(300)).is_err());
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
        drop(second);
        release(None);

        let tmp = TempDir::new();
        let missing = tmp.path().join("missing");
        assert!(granted(claim(Some(&missing), Mode::Apps))
            .listener
            .is_none());
    }
}
