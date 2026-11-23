//! Passing a request to lock on to sengi-compositor.
//!
//! sengi-compositor shows the session lock only to a locker it started
//! itself. It listens for requests to lock on a socket beside its Wayland
//! socket, named after it with `.control` added. That socket being there
//! is what says the session runs on sengi-compositor; a variable left in
//! the environment by an earlier session says nothing. The request is one
//! word and a newline, and so is the answer: `locked` once every screen
//! is covered, `failed` if the locker did not get there.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// Where the compositor of this session would listen, given the values
/// of `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY`. `None` when there is no
/// socket there.
pub fn control_socket(runtime_dir: Option<&OsStr>, display: Option<&OsStr>) -> Option<PathBuf> {
    let display = display.filter(|name| !name.is_empty())?;
    let mut name = display.to_os_string();
    name.push(".control");
    let name = PathBuf::from(name);
    let path = if name.is_absolute() {
        name
    } else {
        PathBuf::from(runtime_dir.filter(|dir| !dir.is_empty())?).join(name)
    };
    path.exists().then_some(path)
}

/// Asks the compositor listening at `socket` to lock the screen and
/// waits for its answer: whether the screen is locked. `None` when
/// nobody could be asked, in which case nothing has been started and the
/// caller is free to lock by itself.
pub fn ask(socket: &Path) -> Option<bool> {
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.write_all(b"lock\n").ok()?;
    let mut answer = String::new();
    // From here on the compositor has the request. No answer is a no.
    let _ = BufReader::new(stream).read_line(&mut answer);
    Some(answer.trim() == "locked")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sengi-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Something that answers one request as a compositor would.
    fn answering(socket: &Path, answer: &'static str) -> std::thread::JoinHandle<String> {
        let listener = UnixListener::bind(socket).unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut word = [0u8; 5];
            stream.read_exact(&mut word).unwrap();
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8_lossy(&word).into_owned()
        })
    }

    #[test]
    fn the_socket_is_looked_for_beside_the_wayland_socket() {
        let dir = scratch("beside");
        let at = |display: &str| control_socket(Some(dir.as_os_str()), Some(OsStr::new(display)));
        // labwc has no such socket, whatever the environment says.
        assert_eq!(at("wayland-0"), None);
        let _listener = UnixListener::bind(dir.join("wayland-1.control")).unwrap();
        assert_eq!(at("wayland-1"), Some(dir.join("wayland-1.control")));
        let absolute = dir.join("wayland-1");
        assert_eq!(
            control_socket(None, Some(absolute.as_os_str())),
            Some(dir.join("wayland-1.control"))
        );
        assert_eq!(control_socket(Some(dir.as_os_str()), None), None);
        assert_eq!(control_socket(None, Some(OsStr::new("wayland-1"))), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_answer_says_whether_the_screen_is_locked() {
        let dir = scratch("answer");
        let socket = dir.join("wayland-1.control");
        let asked = answering(&socket, "locked\n");
        assert_eq!(ask(&socket), Some(true));
        assert_eq!(asked.join().unwrap(), "lock\n");
        std::fs::remove_file(&socket).unwrap();

        let asked = answering(&socket, "failed\n");
        assert_eq!(ask(&socket), Some(false));
        asked.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_socket_nobody_listens_on_leaves_the_locking_to_us() {
        let dir = scratch("stale");
        let socket = dir.join("wayland-1.control");
        // Left behind by a compositor that was killed.
        drop(UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());
        assert_eq!(ask(&socket), None);
        assert_eq!(ask(&dir.join("none.control")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
