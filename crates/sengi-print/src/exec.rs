//! Running a tool to its end, or until it is stopped or too slow. Each
//! runs in a process group of its own, so that what it starts goes with
//! it, and its output is read on threads so that a full pipe never stops
//! it.

use std::io;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use sengi_update::run;

/// What a finished tool left.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    /// Its exit status, when it exited rather than being killed.
    pub code: Option<i32>,
    /// It was stopped because a stop was asked for or the time was up.
    pub stopped: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.stopped
    }
}

/// Runs `command` with no input, to its end, or until `stop` is set or
/// `limit` has passed. It is asked to end, and killed if it does not.
pub fn capture(mut command: Command, stop: &AtomicBool, limit: Duration) -> io::Result<Ran> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = run::spawn_group(&mut command)?;
    let out = child.stdout.take().map(run::collect);
    let err = child.stderr.take().map(run::collect);
    let started = Instant::now();
    let mut asked_at: Option<Instant> = None;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if asked_at.is_none() && (stop.load(Ordering::SeqCst) || started.elapsed() > limit) {
            asked_at = Some(Instant::now());
            run::signal_group(child.id(), libc::SIGTERM);
        }
        if asked_at.is_some_and(|at| at.elapsed() > Duration::from_secs(2)) {
            run::signal_group(child.id(), libc::SIGKILL);
        }
        thread::sleep(Duration::from_millis(25));
    };
    run::signal_group(child.id(), libc::SIGKILL);
    Ok(Ran {
        code: status.code(),
        stopped: asked_at.is_some(),
        stdout: out.and_then(|t| t.join().ok()).unwrap_or_default(),
        stderr: err.and_then(|t| t.join().ok()).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{scratch, script};

    #[test]
    fn output_and_status_come_back() {
        let dir = scratch("exec-ok");
        let tool = script(&dir, "tool", "echo out; echo err >&2; exit 3");
        let ran = capture(
            Command::new(tool),
            &AtomicBool::new(false),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(ran.code, Some(3));
        assert_eq!(ran.stdout.trim(), "out");
        assert_eq!(ran.stderr.trim(), "err");
        assert!(!ran.stopped && !ran.success());
    }

    #[test]
    fn a_tool_that_takes_too_long_is_stopped() {
        let dir = scratch("exec-slow");
        // Ignores the polite request, so that the kill is what ends it.
        let tool = script(&dir, "tool", "trap '' TERM; sleep 30");
        let began = Instant::now();
        let ran = capture(
            Command::new(tool),
            &AtomicBool::new(false),
            Duration::from_millis(300),
        )
        .unwrap();
        assert!(ran.stopped);
        assert!(!ran.success());
        assert!(began.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_stop_ends_it_at_once() {
        let dir = scratch("exec-stop");
        let tool = script(&dir, "tool", "sleep 30");
        let stop = AtomicBool::new(true);
        let began = Instant::now();
        let ran = capture(Command::new(tool), &stop, Duration::from_secs(60)).unwrap();
        assert!(ran.stopped);
        assert!(began.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_missing_tool_is_not_found() {
        let error = capture(
            Command::new("/nonexistent/tool"),
            &AtomicBool::new(false),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
