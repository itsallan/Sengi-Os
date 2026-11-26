//! Running another helper as root through pkexec, with the password
//! asked in Settings' own dialog. This is the same sign-in as the one the
//! system update uses (see `client`): a pseudo-terminal is pkexec's
//! controlling terminal, and when it prints "Password:" there the caller
//! is told, and the answer goes back to the terminal once. The helper's
//! output comes on a pipe, so it cannot be taken for a prompt.
//!
//! The helper is told to stop by closing its standard input, which is
//! all a program that runs as root can be asked for by one that does not.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use crate::client::{open_pty, send_password, wait_reply, watch_prompts, Control, Item, Reply};
use crate::run;

/// How many of the helper's last lines are kept.
const MAX_LINES: usize = 400;

/// What the helper printed and how it ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    pub lines: Vec<String>,
    pub code: Option<i32>,
    /// What it wrote to its error output, for words when it failed.
    pub errors: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elevation {
    /// The helper ran, with permission. Look at its code.
    Ran(Ran),
    /// polkit did not take the password.
    WrongPassword,
    /// The person backed out of the password question.
    NoPassword,
    /// Stopped by the caller before it ended.
    Stopped,
    /// Never started.
    Failed(String),
}

/// Runs `helper` with `args` as root through `pkexec`. `need_password`
/// is called when pkexec asks, and the answer is read from `control`.
/// pkexec exits with 126 or 127 when the person is not allowed, and that
/// comes back as a [`Ran`] with that code, for the caller to word.
pub fn run(
    pkexec: &OsStr,
    helper: &Path,
    args: &[&str],
    control: &Control,
    limit: Duration,
    need_password: &mut dyn FnMut(),
) -> Elevation {
    let (mut master, slave) = match open_pty() {
        Ok(pair) => pair,
        Err(e) => return Elevation::Failed(format!("Could not set up the sign-in: {e}")),
    };
    let slave_fd = slave.as_raw_fd();
    let mut command = Command::new(pkexec);
    command
        .arg(helper)
        .args(args)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: between fork and exec only setsid and ioctl are called,
    // with a descriptor that is open in the child.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match run::spawn_retry(&mut command) {
        Ok(child) => child,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Elevation::Failed("pkexec is not installed.".to_string())
        }
        Err(e) => return Elevation::Failed(format!("Could not ask for permission: {e}")),
    };

    let (tx, rx) = mpsc::channel::<Item>();
    match master.try_clone() {
        Ok(reader) => watch_prompts(reader, tx.clone()),
        Err(e) => return Elevation::Failed(format!("Could not set up the sign-in: {e}")),
    }
    if let Some(stdout) = child.stdout.take() {
        let lines = run::lines(stdout);
        let tx = tx.clone();
        thread::spawn(move || {
            for line in lines {
                if tx.send(Item::Line(line)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Item::Closed);
        });
    }
    drop(tx);
    let errors = child.stderr.take().map(run::collect);
    let mut stdin = child.stdin.take();

    let started = Instant::now();
    let mut sent_password = false;
    let mut wrong = false;
    let mut dismissed = false;
    let mut stopped = false;
    let mut lines = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Item::Line(line)) => {
                // The last lines are the ones that say how it ended.
                if lines.len() >= MAX_LINES {
                    lines.remove(0);
                }
                lines.push(line);
            }
            Ok(Item::Prompt) => {
                if sent_password {
                    wrong = true;
                    break;
                }
                need_password();
                match wait_reply(control) {
                    Reply::Password(pass) => {
                        if send_password(&mut master, &pass).is_err() {
                            dismissed = true;
                            break;
                        }
                        sent_password = true;
                    }
                    Reply::Cancel => {
                        dismissed = true;
                        break;
                    }
                }
            }
            Ok(Item::Closed) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if control.stop.load(Ordering::SeqCst) || started.elapsed() > limit {
            stopped = true;
            break;
        }
    }

    if wrong || dismissed || stopped {
        // Closing the terminal ends pkexec's question; closing the input
        // tells a helper that is already running to stop.
        drop(master);
        drop(stdin.take());
        let _ = child.kill();
        let waited = Instant::now();
        while waited.elapsed() < Duration::from_secs(5) {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        drop(slave);
        return if wrong {
            Elevation::WrongPassword
        } else if dismissed {
            Elevation::NoPassword
        } else {
            Elevation::Stopped
        };
    }

    // Nothing more is ever written to it; a helper that waits for input
    // sees the end.
    if let Some(pipe) = stdin.as_mut() {
        let _ = pipe.flush();
    }
    drop(stdin);
    let status = child.wait().ok();
    let errors = errors.and_then(|t| t.join().ok()).unwrap_or_default();
    drop(master);
    drop(slave);
    Elevation::Ran(Ran {
        lines,
        code: status.and_then(|s| s.code()),
        errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{scratch, script};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn control() -> (Control, mpsc::Sender<Reply>) {
        let (tx, rx) = mpsc::channel();
        (
            Control {
                stop: Arc::new(AtomicBool::new(false)),
                replies: rx,
            },
            tx,
        )
    }

    #[test]
    fn a_helper_that_asks_for_nothing_just_runs() {
        let dir = scratch("elevate-plain");
        let pkexec = script(
            &dir,
            "pkexec",
            "shift; echo \"args: $*\"; echo more; exit 0",
        );
        let (control, _keep) = control();
        let got = run(
            pkexec.as_os_str(),
            Path::new("/usr/bin/sengi-print"),
            &["add", "Foo"],
            &control,
            Duration::from_secs(10),
            &mut || panic!("no password was asked for"),
        );
        let Elevation::Ran(ran) = got else {
            panic!("{got:?}")
        };
        assert_eq!(ran.code, Some(0));
        assert_eq!(ran.lines, vec!["args: add Foo", "more"]);
    }

    #[test]
    fn a_long_output_keeps_its_last_lines() {
        let dir = scratch("elevate-long");
        let pkexec = script(
            &dir,
            "pkexec",
            "i=0; while [ $i -lt 1000 ]; do echo \"line $i\"; i=$((i+1)); done",
        );
        let (control, _keep) = control();
        let got = run(
            pkexec.as_os_str(),
            Path::new("/x"),
            &[],
            &control,
            Duration::from_secs(20),
            &mut || {},
        );
        let Elevation::Ran(ran) = got else {
            panic!("{got:?}")
        };
        assert_eq!(ran.lines.len(), MAX_LINES);
        assert_eq!(ran.lines.last().map(String::as_str), Some("line 999"));
    }

    #[test]
    fn a_helper_that_fails_reports_its_code_and_words() {
        let dir = scratch("elevate-fail");
        let pkexec = script(&dir, "pkexec", "echo broke >&2; exit 3");
        let (control, _keep) = control();
        let got = run(
            pkexec.as_os_str(),
            Path::new("/x"),
            &[],
            &control,
            Duration::from_secs(10),
            &mut || {},
        );
        let Elevation::Ran(ran) = got else {
            panic!("{got:?}")
        };
        assert_eq!(ran.code, Some(3));
        assert_eq!(ran.errors.trim(), "broke");
    }

    #[test]
    fn a_missing_pkexec_is_a_failure_in_words() {
        let (control, _keep) = control();
        let got = run(
            OsStr::new("/nonexistent/pkexec"),
            Path::new("/x"),
            &[],
            &control,
            Duration::from_secs(10),
            &mut || {},
        );
        assert!(matches!(got, Elevation::Failed(_)), "{got:?}");
    }

    #[test]
    fn a_stop_ends_the_wait() {
        let dir = scratch("elevate-stop");
        let pkexec = script(&dir, "pkexec", "sleep 30");
        let (control, _keep) = control();
        control.stop.store(true, Ordering::SeqCst);
        let began = Instant::now();
        let got = run(
            pkexec.as_os_str(),
            Path::new("/x"),
            &[],
            &control,
            Duration::from_secs(60),
            &mut || {},
        );
        assert_eq!(got, Elevation::Stopped);
        assert!(began.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_time_limit_ends_the_wait() {
        let dir = scratch("elevate-limit");
        let pkexec = script(&dir, "pkexec", "sleep 30");
        let (control, _keep) = control();
        let began = Instant::now();
        let got = run(
            pkexec.as_os_str(),
            Path::new("/x"),
            &[],
            &control,
            Duration::from_millis(400),
            &mut || {},
        );
        assert_eq!(got, Elevation::Stopped);
        assert!(began.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_password_goes_to_the_terminal_once() {
        let dir = scratch("elevate-pass");
        // Stands in for pkexec's own agent: asks on its terminal and
        // reads the answer there.
        let pkexec = script(
            &dir,
            "pkexec",
            "printf 'Password: ' >/dev/tty; read -r answer </dev/tty; \
             if [ \"$answer\" = secret ]; then echo granted; exit 0; fi; exit 127",
        );
        let (control, tx) = control();
        tx.send(Reply::Password(crate::client::Pass::new(
            b"secret".to_vec(),
        )))
        .unwrap();
        let mut asked = 0;
        let got = run(
            pkexec.as_os_str(),
            Path::new("/x"),
            &[],
            &control,
            Duration::from_secs(20),
            &mut || asked += 1,
        );
        assert_eq!(asked, 1);
        let Elevation::Ran(ran) = got else {
            panic!("{got:?}")
        };
        assert_eq!(ran.lines, vec!["granted"]);
    }

    #[test]
    fn a_second_prompt_means_the_password_was_wrong() {
        let dir = scratch("elevate-wrong");
        let pkexec = script(
            &dir,
            "pkexec",
            "printf 'Password: ' >/dev/tty; read -r a </dev/tty; \
             printf 'Sorry.\\nPassword: ' >/dev/tty; read -r a </dev/tty; exit 127",
        );
        let (control, tx) = control();
        tx.send(Reply::Password(crate::client::Pass::new(b"nope".to_vec())))
            .unwrap();
        let got = run(
            pkexec.as_os_str(),
            Path::new("/x"),
            &[],
            &control,
            Duration::from_secs(20),
            &mut || {},
        );
        assert_eq!(got, Elevation::WrongPassword);
    }

    #[test]
    fn backing_out_of_the_question_is_not_a_failure() {
        let dir = scratch("elevate-cancel");
        let pkexec = script(&dir, "pkexec", "printf 'Password: ' >/dev/tty; sleep 20");
        let (control, tx) = control();
        tx.send(Reply::Cancel).unwrap();
        let got = run(
            pkexec.as_os_str(),
            Path::new("/x"),
            &[],
            &control,
            Duration::from_secs(20),
            &mut || {},
        );
        assert_eq!(got, Elevation::NoPassword);
    }
}
