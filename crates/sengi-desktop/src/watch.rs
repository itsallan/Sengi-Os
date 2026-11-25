//! Tells the desktop when something is made, removed or renamed in the
//! Desktop folder. One thread sleeps in `read` on an inotify descriptor, so
//! nothing runs between changes.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

/// Changes that come in a burst, such as copying a folder in, are told as
/// one: the thread lets this long pass before it reads the rest.
const SETTLE: Duration = Duration::from_millis(60);

const MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF;

fn watch(dir: &Path) -> io::Result<OwnedFd> {
    // SAFETY: plain system calls; the descriptor is owned right away.
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let path = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path holds a NUL"))?;
    // SAFETY: `path` is a valid C string for the duration of the call.
    let id = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), MASK) };
    if id < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Waits until at least one event is read. True if the folder itself went.
fn wait(fd: &OwnedFd) -> io::Result<bool> {
    let mut buffer = [0u8; 4096];
    loop {
        // SAFETY: reads into a buffer of the length given.
        let n = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if n < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let mut gone = false;
        let mut at = 0usize;
        let n = n as usize;
        while at + 16 <= n {
            let mask = u32::from_ne_bytes(buffer[at + 4..at + 8].try_into().unwrap());
            let len = u32::from_ne_bytes(buffer[at + 12..at + 16].try_into().unwrap()) as usize;
            if mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_IGNORED) != 0 {
                gone = true;
            }
            at += 16 + len;
        }
        return Ok(gone);
    }
}

/// Starts watching `dir`. `changed` is called from the watcher's thread
/// after each burst of changes and when the folder itself was removed and
/// made again.
pub fn spawn(dir: PathBuf, changed: impl Fn() + Send + 'static) -> io::Result<()> {
    let mut fd = watch(&dir)?;
    thread::Builder::new()
        .name("watch".into())
        .stack_size(64 * 1024)
        .spawn(move || loop {
            match wait(&fd) {
                Ok(false) => {}
                Ok(true) => {
                    // The folder was removed or moved away: a Desktop is
                    // always there.
                    let again = std::fs::create_dir_all(&dir).and_then(|()| watch(&dir));
                    match again {
                        Ok(new) => fd = new,
                        Err(error) => {
                            eprintln!("sengi-desktop: not watching {}: {error}", dir.display());
                            return;
                        }
                    }
                }
                Err(error) => {
                    eprintln!("sengi-desktop: not watching {}: {error}", dir.display());
                    return;
                }
            }
            thread::sleep(SETTLE);
            changed();
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::temp_dir;
    use std::fs;
    use std::sync::mpsc;

    const LONG: Duration = Duration::from_secs(5);

    #[test]
    fn a_new_file_is_told() {
        let dir = temp_dir("watch");
        let (tx, rx) = mpsc::channel();
        spawn(dir.clone(), move || {
            let _ = tx.send(());
        })
        .unwrap();
        fs::write(dir.join("new"), "").unwrap();
        rx.recv_timeout(LONG).expect("no word of the new file");
        fs::remove_file(dir.join("new")).unwrap();
        rx.recv_timeout(LONG).expect("no word of the removal");
        fs::create_dir(dir.join("sub")).unwrap();
        rx.recv_timeout(LONG).expect("no word of the folder");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_removed_folder_is_made_again_and_watched() {
        let dir = temp_dir("watch-gone");
        let (tx, rx) = mpsc::channel();
        spawn(dir.clone(), move || {
            let _ = tx.send(());
        })
        .unwrap();
        fs::remove_dir(&dir).unwrap();
        rx.recv_timeout(LONG).expect("no word of the removal");
        assert!(dir.is_dir());
        fs::write(dir.join("again"), "").unwrap();
        rx.recv_timeout(LONG).expect("not watching again");
        fs::remove_dir_all(dir).unwrap();
    }
}
