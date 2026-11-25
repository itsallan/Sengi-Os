//! Running the tools: each in a process group of its own, so that what it
//! starts goes with it, with its output read on threads of ours so that a
//! full pipe never stops it, and with a way to stop it that is looked at
//! about ten times a second.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::apt::{self, Status};
use crate::tools::Tools;

/// How often a running tool is looked at for a request to stop.
const LOOK: Duration = Duration::from_millis(100);
/// After a polite stop, how long before it is made.
const PATIENCE: Duration = Duration::from_secs(8);
/// How much of a tool's text is kept, for the words of an error.
const KEEP: usize = 256 * 1024;

/// Starts `command`. A script that was written a moment ago may still be
/// open for writing in another thread's fork (`ETXTBSY`), so that is
/// tried again.
pub fn spawn_retry(command: &mut Command) -> io::Result<Child> {
    let mut tries = 0;
    loop {
        match command.spawn() {
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) && tries < 40 => {
                tries += 1;
                thread::sleep(Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

/// Starts `command` in a process group of its own, so that what it
/// starts goes with it.
pub fn spawn_group(command: &mut Command) -> io::Result<Child> {
    command.process_group(0);
    spawn_retry(command)
}

/// Sends `signal` to the group `pid` leads.
pub fn signal_group(pid: u32, signal: libc::c_int) {
    // SAFETY: killpg only sends a signal.
    unsafe { libc::killpg(pid as libc::pid_t, signal) };
}

/// Reads a stream to its end on a thread, keeping the first `KEEP` bytes.
pub fn collect(mut stream: impl Read + Send + 'static) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if kept.len() < KEEP {
                        let room = (KEEP - kept.len()).min(n);
                        kept.extend_from_slice(&buffer[..room]);
                    }
                }
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    })
}

/// Reads lines (ended by a line feed) on a thread and sends them. The
/// channel ends when the stream does.
pub fn lines(stream: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stream.take(64 * 1024 * 1024));
        let mut line = Vec::new();
        loop {
            line.clear();
            // A line longer than any we understand is cut, not buffered
            // without end.
            match reader
                .by_ref()
                .take(crate::json::MAX_LINE as u64)
                .read_until(b'\n', &mut line)
            {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line).trim_end().to_string();
                    if tx.send(text).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// A pipe: (read end, write end), neither inherited by what we start.
fn pipe() -> io::Result<(File, File)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe2 fills the two integers it is given.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors are new and nobody else owns them.
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}

/// What a finished apt-get left.
#[derive(Debug, Clone, Default)]
pub struct Ran {
    /// Its exit status, when it exited rather than being killed.
    pub code: Option<i32>,
    /// It was stopped because a stop was asked for.
    pub stopped: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.stopped
    }

    /// What to say went wrong: apt's own last word.
    pub fn problem(&self) -> String {
        let from_stderr = crate::last_line(&self.stderr);
        if !from_stderr.is_empty() {
            from_stderr
        } else {
            let from_stdout = crate::last_line(&self.stdout);
            if from_stdout.is_empty() {
                match self.code {
                    Some(code) => format!("apt-get stopped with status {code}"),
                    None => "apt-get was stopped".to_string(),
                }
            } else {
                from_stdout
            }
        }
    }
}

/// An apt-get command with an environment of our own making: no
/// variable of whoever started us can change what it does. It sees
/// English, and is never asked a question.
pub fn apt_command(tools: &Tools, args: &[String]) -> Command {
    let mut command = Command::new(&tools.apt_get);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TERM", "dumb")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("APT_LISTCHANGES_FRONTEND", "none");
    // Someone who is not root and sits behind a proxy keeps it. Root
    // takes nothing from its caller.
    // SAFETY: geteuid has no arguments and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        for key in [
            "http_proxy",
            "https_proxy",
            "no_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "NO_PROXY",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
    }
    command
        .args(["-q", "-o", "APT::Color=0"])
        .args(args)
        .stdin(Stdio::null());
    command
}

/// Runs apt-get to the end without watching it. For a simulation.
pub fn apt_capture(tools: &Tools, args: &[String]) -> io::Result<Ran> {
    let mut command = apt_command(tools, args);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn_group(&mut command)?;
    let out = child.stdout.take().map(collect);
    let err = child.stderr.take().map(collect);
    let status = child.wait()?;
    signal_group(child.id(), libc::SIGKILL);
    Ok(Ran {
        code: status.code(),
        stopped: false,
        stdout: out.and_then(|t| t.join().ok()).unwrap_or_default(),
        stderr: err.and_then(|t| t.join().ok()).unwrap_or_default(),
    })
}

/// Runs apt-get and tells `on_status` what its status descriptor says.
///
/// While `stop` is set, the group gets SIGTERM, and SIGKILL if that is
/// not enough after a while. A caller that must not be stopped passes a
/// flag nobody sets.
pub fn apt_watch(
    tools: &Tools,
    args: &[String],
    stop: &AtomicBool,
    mut on_status: impl FnMut(Status),
) -> io::Result<Ran> {
    let (status_read, status_write) = pipe()?;
    let write_fd = status_write.as_raw_fd();
    let mut command = apt_command(tools, args);
    command
        .args(["-o", "APT::Status-Fd=3"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: between fork and exec only dup2 and fcntl are called, which
    // are safe there, and the descriptor numbers are plain integers.
    unsafe {
        command.pre_exec(move || {
            if write_fd == 3 {
                let flags = libc::fcntl(3, libc::F_GETFD);
                if flags < 0 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
            } else if libc::dup2(write_fd, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = spawn_group(&mut command)?;
    // The child has its own copy; ours must go, or the end never comes.
    drop(status_write);
    let out = child.stdout.take().map(collect);
    let err = child.stderr.take().map(collect);
    let incoming = lines(status_read);

    let pid = child.id();
    let mut stopped = false;
    let mut stopped_at = Instant::now();
    let mut killed = false;
    let mut exited: Option<std::process::ExitStatus> = None;
    let mut exited_at = Instant::now();
    loop {
        match incoming.recv_timeout(LOOK) {
            Ok(line) => {
                if let Some(status) = apt::parse_status(&line) {
                    on_status(status);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if stop.load(Ordering::SeqCst) && !stopped {
            stopped = true;
            stopped_at = Instant::now();
            signal_group(pid, libc::SIGTERM);
        }
        if stopped && !killed && stopped_at.elapsed() > PATIENCE {
            killed = true;
            signal_group(pid, libc::SIGKILL);
        }
        if exited.is_none() {
            if let Ok(Some(status)) = child.try_wait() {
                exited = Some(status);
                exited_at = Instant::now();
            }
        }
        // Something it started may hold the descriptor open after it has
        // gone. That does not keep us waiting for long.
        if exited.is_some() && exited_at.elapsed() > Duration::from_secs(2) {
            break;
        }
    }
    let status = match exited {
        Some(status) => status,
        None => child.wait()?,
    };
    signal_group(pid, libc::SIGKILL);
    Ok(Ran {
        code: status.code(),
        stopped,
        stdout: out.and_then(|t| t.join().ok()).unwrap_or_default(),
        stderr: err.and_then(|t| t.join().ok()).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{scratch, script};
    use std::sync::Arc;

    fn tools_with(dir: &std::path::Path, body: &str) -> Tools {
        Tools {
            apt_get: script(dir, "apt-get", body),
            ..Tools::system()
        }
    }

    #[test]
    fn status_lines_come_from_descriptor_three() {
        let dir = scratch("watch");
        let tools = tools_with(
            &dir,
            "echo 'Get:1 http://example' ; \
             echo 'dlstatus:1:10.0000:Retrieving file 1 of 2' >&3; \
             echo 'dlstatus:2:100.0000:Retrieving file 2 of 2' >&3; \
             echo done",
        );
        let stop = AtomicBool::new(false);
        let mut seen = Vec::new();
        let ran = apt_watch(&tools, &[], &stop, |s| seen.push(s)).unwrap();
        assert!(ran.success());
        assert!(ran.stdout.contains("Get:1"));
        // Only the status lines were taken for status.
        assert_eq!(seen.len(), 2);
        assert!(matches!(seen[1], Status::Download { percent, .. } if percent == 100.0));
    }

    #[test]
    fn its_environment_is_ours_not_the_callers() {
        let dir = scratch("env");
        let tools = tools_with(
            &dir,
            "echo \"$DEBIAN_FRONTEND $LC_ALL ${APT_CONFIG:-none}\"; echo \"$@\"",
        );
        // A variable nobody else in this test binary reads.
        std::env::set_var("APT_CONFIG", "/tmp/evil.conf");
        let ran = apt_capture(&tools, &["upgrade".to_string()]).unwrap();
        let first = ran.stdout.lines().next().unwrap();
        assert_eq!(first, "noninteractive C none");
        assert!(ran.stdout.contains("-q -o APT::Color=0 upgrade"));
    }

    #[test]
    fn a_stop_ends_the_tool_and_what_it_started() {
        let dir = scratch("stop");
        let marker = dir.join("child-alive");
        let tools = tools_with(
            &dir,
            &format!(
                "(sleep 30; touch '{}') &\necho 'dlstatus:1:1.0000:Retrieving file 1 of 1' >&3\nsleep 30",
                marker.display()
            ),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let setter = {
            let stop = stop.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(300));
                stop.store(true, Ordering::SeqCst);
            })
        };
        let started = Instant::now();
        let ran = apt_watch(&tools, &[], &stop, |_| {}).unwrap();
        setter.join().unwrap();
        assert!(ran.stopped);
        assert!(!ran.success());
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!marker.exists());
    }

    #[test]
    fn a_failure_is_said_in_apts_own_words() {
        let dir = scratch("fail");
        let tools = tools_with(&dir, "echo 'E: Unable to lock the directory' >&2; exit 100");
        let ran = apt_capture(&tools, &[]).unwrap();
        assert_eq!(ran.code, Some(100));
        assert!(!ran.success());
        assert_eq!(ran.problem(), "E: Unable to lock the directory");
        let none = Ran::default();
        assert_eq!(none.problem(), "apt-get was stopped");
    }

    #[test]
    fn long_lines_and_many_of_them_do_not_stall_it() {
        let dir = scratch("flood");
        let tools = tools_with(
            &dir,
            "i=0; while [ $i -lt 3000 ]; do echo 'dlstatus:1:5.0000:Retrieving file 1 of 1' >&3; i=$((i+1)); done; \
             head -c 2000000 /dev/zero | tr '\\0' x; exit 0",
        );
        let stop = AtomicBool::new(false);
        let mut count = 0;
        let ran = apt_watch(&tools, &[], &stop, |_| count += 1).unwrap();
        assert!(ran.success());
        assert_eq!(count, 3000);
        assert!(ran.stdout.len() <= KEEP);
    }
}
