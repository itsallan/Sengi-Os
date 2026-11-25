//! Tells Files when the folder it shows changes under it. One thread sleeps
//! in `poll` on an inotify descriptor, so nothing runs between changes. A
//! burst of changes, such as a folder being copied in, is told as one: the
//! thread waits until nothing has happened for a moment.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// How long it must be quiet before the change is told.
pub const SETTLE: Duration = Duration::from_millis(150);

/// A steady stream of changes is told at least this often.
const LONGEST: Duration = Duration::from_secs(2);

const MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_CLOSE_WRITE
    | libc::IN_ATTRIB
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF;

/// Watches one folder at a time, and changes which.
pub struct Watcher {
    fd: Arc<OwnedFd>,
    /// The watch that counts, or -1. Events of an older one are dropped.
    current: Arc<AtomicI32>,
}

impl Watcher {
    /// Starts the thread. `changed` is called from it after each burst.
    pub fn spawn(changed: impl Fn() + Send + 'static) -> io::Result<Watcher> {
        // SAFETY: a plain system call; the descriptor is owned right away.
        let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
        let current = Arc::new(AtomicI32::new(-1));
        let (thread_fd, thread_current) = (fd.clone(), current.clone());
        thread::Builder::new()
            .name("watch".into())
            .stack_size(64 * 1024)
            .spawn(move || run(&thread_fd, &thread_current, &changed))?;
        Ok(Watcher { fd, current })
    }

    /// Watches `dir` and nothing else, or nothing at all with None.
    pub fn watch(&self, dir: Option<&Path>) -> io::Result<()> {
        let old = self.current.swap(-1, Ordering::SeqCst);
        if old >= 0 {
            // SAFETY: removes a watch of this descriptor.
            unsafe { libc::inotify_rm_watch(self.fd.as_raw_fd(), old) };
        }
        let Some(dir) = dir else { return Ok(()) };
        let path = CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path holds a NUL"))?;
        // SAFETY: `path` is a valid C string for the duration of the call.
        let id = unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), path.as_ptr(), MASK) };
        if id < 0 {
            return Err(io::Error::last_os_error());
        }
        self.current.store(id, Ordering::SeqCst);
        Ok(())
    }
}

/// Reads what is waiting. True if any of it was about the watch that counts.
fn drain(fd: &OwnedFd, current: &AtomicI32) -> io::Result<bool> {
    let mut buffer = [0u8; 4096];
    // SAFETY: reads into a buffer of the length given.
    let n = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
    if n < 0 {
        let error = io::Error::last_os_error();
        return if matches!(
            error.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) {
            Ok(false)
        } else {
            Err(error)
        };
    }
    let n = n as usize;
    let want = current.load(Ordering::SeqCst);
    let mut seen = false;
    let mut at = 0usize;
    while at + 16 <= n {
        let id = i32::from_ne_bytes(buffer[at..at + 4].try_into().unwrap());
        let mask = u32::from_ne_bytes(buffer[at + 4..at + 8].try_into().unwrap());
        let len = u32::from_ne_bytes(buffer[at + 12..at + 16].try_into().unwrap()) as usize;
        // Letting go of a watch is not a change in the folder.
        if id == want && mask & !libc::IN_IGNORED != 0 {
            seen = true;
        }
        at += 16 + len;
    }
    Ok(seen)
}

/// Waits up to `wait` for the descriptor to have something to read.
fn ready(fd: &OwnedFd, wait: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = wait.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: one valid pollfd.
    let n = unsafe { libc::poll(&mut poll, 1, millis) };
    n > 0
}

fn run(fd: &OwnedFd, current: &AtomicI32, changed: &dyn Fn()) {
    loop {
        // Sleep until something happens.
        if !ready(fd, Duration::from_secs(3600)) {
            continue;
        }
        match drain(fd, current) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(error) => {
                eprintln!("sengi-files: not watching the folder: {error}");
                return;
            }
        }
        // Then wait for quiet.
        let started = Instant::now();
        while started.elapsed() < LONGEST && ready(fd, SETTLE) {
            if drain(fd, current).is_err() {
                return;
            }
        }
        changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::mpsc;

    const LONG: Duration = Duration::from_secs(5);

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sengi-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn watcher() -> (Watcher, mpsc::Receiver<Instant>) {
        let (tx, rx) = mpsc::channel();
        let w = Watcher::spawn(move || {
            let _ = tx.send(Instant::now());
        })
        .unwrap();
        (w, rx)
    }

    #[test]
    fn a_new_file_a_removal_and_a_rename_are_told() {
        let dir = temp("watch");
        let (w, rx) = watcher();
        w.watch(Some(&dir)).unwrap();
        fs::write(dir.join("new"), "").unwrap();
        rx.recv_timeout(LONG).expect("no word of the new file");
        fs::rename(dir.join("new"), dir.join("renamed")).unwrap();
        rx.recv_timeout(LONG).expect("no word of the rename");
        fs::remove_file(dir.join("renamed")).unwrap();
        rx.recv_timeout(LONG).expect("no word of the removal");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_burst_is_told_once_and_after_it_is_over() {
        let dir = temp("watch-burst");
        let (w, rx) = watcher();
        w.watch(Some(&dir)).unwrap();
        let started = Instant::now();
        for n in 0..20 {
            fs::write(dir.join(format!("f{n}")), "").unwrap();
            thread::sleep(Duration::from_millis(10));
        }
        let told = rx.recv_timeout(LONG).expect("no word of the burst");
        assert!(told.duration_since(started) >= Duration::from_millis(200));
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_the_folder_now_watched_is_told() {
        let (one, two) = (temp("watch-one"), temp("watch-two"));
        let (w, rx) = watcher();
        w.watch(Some(&one)).unwrap();
        w.watch(Some(&two)).unwrap();
        fs::write(one.join("x"), "").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(600)).is_err());
        fs::write(two.join("x"), "").unwrap();
        rx.recv_timeout(LONG).expect("no word of the new folder");
        w.watch(None).unwrap();
        fs::write(two.join("y"), "").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(600)).is_err());
        fs::remove_dir_all(one).unwrap();
        fs::remove_dir_all(two).unwrap();
    }
}
