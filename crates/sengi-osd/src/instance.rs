//! One indicator at a time. The one that is open listens on a datagram
//! socket in `$XDG_RUNTIME_DIR`; a later run that finds it open hands its
//! level over and leaves, so the open panel changes and nothing stacks up.
//!
//! The path is settled under a lock file. A run takes the lock, tries to
//! hand its level to the open indicator, and only if nobody takes it
//! becomes the open one. An indicator that has run out of time ends under
//! the same lock, after looking once more for something that arrived, so a
//! key pressed just as it ends is not lost.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::level::Level;

const SOCKET: &str = "sengi-osd.sock";
const LOCK: &str = "sengi-osd.lock";
/// Longer than any level is when written out.
const MESSAGE_MAX: usize = 64;

/// What a run is to do after asking.
pub enum Claim {
    /// The open indicator has the level; there is nothing more to do.
    Delivered,
    /// This run is the open indicator.
    Open(Open),
}

/// The open indicator's end of the socket.
pub struct Open {
    socket: Option<UnixDatagram>,
    paths: Option<(PathBuf, PathBuf)>,
}

fn lock(path: &Path) -> Option<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .ok()?;
    file.lock().ok()?;
    Some(file)
}

/// Hands `level` to the indicator listening at `socket`, if there is one.
fn hand_over(socket: &Path, level: &Level) -> bool {
    let Ok(sender) = UnixDatagram::unbound() else {
        return false;
    };
    // A full queue means the indicator is busy with earlier presses; this
    // one is dropped, the next one shows the newer level.
    let _ = sender.set_nonblocking(true);
    match sender.send_to(level.encode().as_bytes(), socket) {
        Ok(_) => true,
        Err(error) => error.kind() == ErrorKind::WouldBlock,
    }
}

/// Becomes the open indicator, or hands `level` to the one that is.
/// Without a usable runtime directory there is nowhere to meet and every
/// run opens its own.
pub fn claim(runtime_dir: Option<&Path>, level: &Level) -> Claim {
    let Some(dir) = runtime_dir.filter(|dir| dir.is_absolute()) else {
        return Claim::Open(Open {
            socket: None,
            paths: None,
        });
    };
    let (socket_path, lock_path) = (dir.join(SOCKET), dir.join(LOCK));
    let Some(_guard) = lock(&lock_path) else {
        return Claim::Open(Open {
            socket: None,
            paths: None,
        });
    };
    if hand_over(&socket_path, level) {
        return Claim::Delivered;
    }
    // Whatever is at the path is a socket nobody listens on.
    let _ = std::fs::remove_file(&socket_path);
    let socket = UnixDatagram::bind(&socket_path).ok();
    Claim::Open(Open {
        socket,
        paths: Some((socket_path, lock_path)),
    })
}

/// What happened while the indicator waited.
#[derive(Debug, PartialEq, Eq)]
pub enum Wait {
    /// Another run handed over a level.
    Level(Level),
    /// Nothing came for the whole time, and the socket is gone.
    Over,
}

impl Open {
    /// Waits up to `time` for a level from a later run. When it runs out,
    /// looks once more under the lock and then gives the socket up.
    pub fn wait(&self, time: Duration) -> Wait {
        let Some(socket) = &self.socket else {
            std::thread::sleep(time);
            return Wait::Over;
        };
        let end = Instant::now() + time;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            // A zero timeout means "forever" to the kernel.
            let _ = socket.set_nonblocking(false);
            let _ = socket.set_read_timeout(Some(left.max(Duration::from_millis(1))));
            if let Some(level) = receive(socket) {
                return Wait::Level(level);
            }
        }
        // Closing is one step as far as a later run can tell.
        let _guard = self.paths.as_ref().and_then(|(_, lock_path)| lock(lock_path));
        let _ = socket.set_nonblocking(true);
        if let Some(level) = receive(socket) {
            return Wait::Level(level);
        }
        self.remove();
        Wait::Over
    }

    /// Takes the socket away, for a run that is leaving without waiting.
    pub fn give_up(&self) {
        let _guard = self.paths.as_ref().and_then(|(_, lock_path)| lock(lock_path));
        self.remove();
    }

    fn remove(&self) {
        if let Some((socket_path, _)) = &self.paths {
            let _ = std::fs::remove_file(socket_path);
        }
    }
}

fn receive(socket: &UnixDatagram) -> Option<Level> {
    let mut buffer = [0u8; MESSAGE_MAX];
    let length = socket.recv(&mut buffer).ok()?;
    Level::decode(std::str::from_utf8(&buffer[..length]).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::Kind;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sengi-osd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn level(percent: u32) -> Level {
        Level::new(Kind::Volume, percent, false)
    }

    #[test]
    fn the_first_run_opens_and_the_second_hands_over() {
        let dir = dir("handover");
        let Claim::Open(open) = claim(Some(&dir), &level(10)) else {
            panic!("the first run should open");
        };
        assert!(matches!(claim(Some(&dir), &level(20)), Claim::Delivered));
        assert!(matches!(claim(Some(&dir), &level(30)), Claim::Delivered));
        assert_eq!(open.wait(Duration::from_secs(1)), Wait::Level(level(20)));
        assert_eq!(open.wait(Duration::from_secs(1)), Wait::Level(level(30)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_indicator_that_ran_out_leaves_the_path_free() {
        let dir = dir("over");
        let Claim::Open(open) = claim(Some(&dir), &level(10)) else {
            panic!("the first run should open");
        };
        assert_eq!(open.wait(Duration::from_millis(50)), Wait::Over);
        assert!(!dir.join(SOCKET).exists());
        // The next run opens again, and this time hands over to nobody.
        assert!(matches!(claim(Some(&dir), &level(20)), Claim::Open(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_socket_nobody_listens_on_is_replaced() {
        let dir = dir("stale");
        drop(UnixDatagram::bind(dir.join(SOCKET)).unwrap());
        assert!(dir.join(SOCKET).exists());
        assert!(matches!(claim(Some(&dir), &level(10)), Claim::Open(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_runtime_directory_every_run_opens() {
        assert!(matches!(claim(None, &level(1)), Claim::Open(_)));
        assert!(matches!(
            claim(Some(Path::new("relative")), &level(1)),
            Claim::Open(_)
        ));
    }

    #[test]
    fn junk_on_the_socket_is_ignored() {
        let dir = dir("junk");
        let Claim::Open(open) = claim(Some(&dir), &level(10)) else {
            panic!("the first run should open");
        };
        let sender = UnixDatagram::unbound().unwrap();
        sender.send_to(b"hello", dir.join(SOCKET)).unwrap();
        assert_eq!(open.wait(Duration::from_millis(100)), Wait::Over);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
