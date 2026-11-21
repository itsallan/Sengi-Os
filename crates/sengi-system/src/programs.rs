//! The services reached by running a program: PipeWire through `wpctl`
//! and do-not-disturb through the notification service's own command.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::commands;
use crate::services::{AudioService, DndService, Refusal};
use crate::volume::{self, Volume};

/// How long a command may take. These answer in a few milliseconds; one
/// that has not after this long is stuck, and the worker that ran it must
/// not be stuck with it.
const DEADLINE: Duration = Duration::from_secs(5);
/// How much of a command's output is taken. `wpctl status` on a machine
/// with a great many devices is some tens of kilobytes.
const MAX_OUTPUT: usize = 256 * 1024;

/// Run `program`, found on `PATH` and started by its full path, to the
/// end and return what it printed, if it succeeded. One that takes too
/// long or prints too much is killed and counts as failed.
pub fn output(program: &str, args: &[&str]) -> Option<String> {
    let path = std::env::var("PATH").unwrap_or_default();
    run(&find_program(program, &path)?, args, DEADLINE, MAX_OUTPUT)
}

/// Kill a child that is not going to be waited out, and reap it.
fn end(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn run(program: &Path, args: &[&str], limit: Duration, cap: usize) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + limit;
    let read = child
        .stdout
        .take()
        .and_then(|stdout| read_until(stdout, deadline, cap));
    let Some(printed) = read else {
        end(&mut child);
        return None;
    };
    // It has closed its output, which nearly always means it has exited.
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return status
                    .success()
                    .then(|| String::from_utf8_lossy(&printed).into_owned())
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            _ => {
                end(&mut child);
                return None;
            }
        }
    }
}

/// Everything `stdout` gives up to its end. `None` if that is more than
/// `cap` bytes or has not ended by `deadline`.
fn read_until(mut stdout: ChildStdout, deadline: Instant, cap: usize) -> Option<Vec<u8>> {
    let mut printed = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        let mut waiting = libc::pollfd {
            fd: stdout.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout = i32::try_from(left.as_millis()).unwrap_or(i32::MAX).max(1);
        // SAFETY: poll is given one descriptor, which `stdout` keeps open
        // for as long as this runs, and writes only to `waiting`.
        let ready = unsafe { libc::poll(&mut waiting, 1, timeout) };
        if ready == 0 {
            continue;
        }
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return None;
        }
        match stdout.read(&mut buffer) {
            Ok(0) => return Some(printed),
            Ok(count) if printed.len() + count > cap => return None,
            Ok(count) => printed.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

/// The default output's volume, through `wpctl`.
pub struct Wpctl;

impl AudioService for Wpctl {
    fn read(&self) -> Option<Volume> {
        volume::parse(&output(commands::WPCTL, &["get-volume", commands::SINK])?)
    }

    fn set_volume(&self, percent: u32) {
        let _ = self.try_set_volume(percent);
    }

    fn set_muted(&self, muted: bool) {
        let _ = self.try_set_muted(muted);
    }

    fn try_set_volume(&self, percent: u32) -> Result<(), Refusal> {
        let args = volume::set_volume_args(percent);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        done(output(commands::WPCTL, &args))
    }

    fn try_set_muted(&self, muted: bool) -> Result<(), Refusal> {
        let args = volume::set_mute_args(muted);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        done(output(commands::WPCTL, &args))
    }
}

/// A command run for what it does, not for what it prints.
fn done(output: Option<String>) -> Result<(), Refusal> {
    output.map(|_| ()).ok_or(Refusal::Failed)
}

/// Where `program` is on a `PATH`-style list of directories.
pub fn find_program(program: &str, path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| {
            candidate
                .metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// What `sengi-notify --dnd status` printed, if it was an answer.
pub fn parse_dnd(output: &str) -> Option<bool> {
    match output.trim() {
        commands::DND_ON => Some(true),
        commands::DND_OFF => Some(false),
        _ => None,
    }
}

/// Do-not-disturb, through the `sengi-notify` command.
pub struct Notify {
    installed: bool,
}

impl Notify {
    /// Looks for `sengi-notify` in `PATH` once, now.
    pub fn new() -> Notify {
        let path = std::env::var("PATH").unwrap_or_default();
        Notify {
            installed: find_program(commands::NOTIFY, &path).is_some(),
        }
    }
}

impl Default for Notify {
    fn default() -> Notify {
        Notify::new()
    }
}

impl DndService for Notify {
    fn available(&self) -> bool {
        self.installed
    }

    fn read(&self) -> Option<bool> {
        let args = [commands::DND_FLAG, commands::DND_STATUS];
        parse_dnd(&output(commands::NOTIFY, &args)?)
    }

    fn set(&self, on: bool) {
        let _ = self.try_set(on);
    }

    fn try_set(&self, on: bool) -> Result<(), Refusal> {
        let state = if on {
            commands::DND_ON
        } else {
            commands::DND_OFF
        };
        done(output(commands::NOTIFY, &[commands::DND_FLAG, state]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn programs_are_found_on_the_path() {
        let tmp = TempDir::new();
        tmp.write("bin/sengi-notify", "#!/bin/sh\n");
        tmp.write("bin/readme", "not a program\n");
        tmp.write("other/placeholder", "");
        let bin = tmp.path().join("bin");
        let program = bin.join("sengi-notify");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(bin.join("readme"), std::fs::Permissions::from_mode(0o644))
            .unwrap();

        let path = format!(
            "{}::{}:/nonexistent",
            tmp.path().join("other").display(),
            bin.display()
        );
        assert_eq!(find_program("sengi-notify", &path), Some(program));
        // Present but not executable, a directory, and not there at all.
        assert_eq!(find_program("readme", &path), None);
        assert_eq!(find_program("bin", &tmp.path().display().to_string()), None);
        assert_eq!(find_program("sengi-notify", ""), None);
    }

    fn sh(script: &str, limit: Duration, cap: usize) -> Option<String> {
        run(Path::new("/bin/sh"), &["-c", script], limit, cap)
    }

    const LONG: Duration = Duration::from_secs(20);

    #[test]
    fn a_command_is_run_for_what_it_prints_if_it_succeeds() {
        assert_eq!(
            sh("echo one; echo two", LONG, 1024).as_deref(),
            Some("one\ntwo\n")
        );
        assert_eq!(sh("true", LONG, 1024).as_deref(), Some(""));
        assert_eq!(sh("echo said; exit 3", LONG, 1024), None);
        assert_eq!(
            run(Path::new("/nonexistent/program"), &[], LONG, 1024),
            None
        );
    }

    #[test]
    fn a_command_that_hangs_is_killed_at_the_deadline() {
        let short = Duration::from_millis(200);
        for script in [
            "sleep 30",
            "echo started; sleep 30",
            // Closes its output and stays.
            "exec >&-; sleep 30",
        ] {
            let started = Instant::now();
            assert_eq!(sh(script, short, 1024), None, "{script}");
            let took = started.elapsed();
            assert!(
                took >= short && took < Duration::from_secs(10),
                "{script}: {took:?}"
            );
        }
    }

    #[test]
    fn a_command_that_prints_without_end_is_cut_off() {
        let started = Instant::now();
        assert_eq!(
            sh("while :; do echo xxxxxxxxxxxxxxxx; done", LONG, 64 * 1024),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        // Up to the cap is fine.
        assert_eq!(sh("printf 12345678", LONG, 8).as_deref(), Some("12345678"));
        assert_eq!(sh("printf 123456789", LONG, 8), None);
    }

    #[test]
    fn a_program_is_looked_for_on_the_path_and_nowhere_else() {
        // Not in the working directory, whatever is there.
        assert_eq!(output("no-such-program-anywhere", &[]), None);
    }

    #[test]
    fn dnd_answers() {
        assert_eq!(parse_dnd("on\n"), Some(true));
        assert_eq!(parse_dnd("off"), Some(false));
        // An older service that does not know the question.
        assert_eq!(parse_dnd(""), None);
        assert_eq!(
            parse_dnd("--dnd takes on, off or toggle, not 'status'"),
            None
        );
    }
}
