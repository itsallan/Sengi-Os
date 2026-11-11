//! Starting programs from the dock.

use std::fmt;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::desktop::Entry;
use crate::exec::{self, ExecError};

/// What the search button runs. This becomes `sengi-launcher` when it exists.
pub const LAUNCHER_COMMAND: &[&str] = &["sengi-launcher"];

/// Puts an app in its own scope under app.slice. Used when it is installed.
pub const RUNNER: &str = "sengi-run";

/// Prefix for entries that say `Terminal=true`.
pub const TERMINAL_COMMAND: &[&str] = &["foot", "-e"];

#[derive(Debug)]
pub enum LaunchError {
    Exec(ExecError),
    Spawn(String, io::Error),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exec(e) => write!(f, "{e}"),
            Self::Spawn(program, e) => write!(f, "cannot start {program}: {e}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// The full argument vector for an entry, before the runner is added.
pub fn command_for(entry: &Entry) -> Result<Vec<String>, ExecError> {
    let mut args = exec::command(entry)?;
    if entry.terminal {
        let mut wrapped: Vec<String> = TERMINAL_COMMAND.iter().map(|s| s.to_string()).collect();
        wrapped.append(&mut args);
        args = wrapped;
    }
    Ok(args)
}

/// Looks a program up the way the shell would.
pub fn find_in_path(program: &str, path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| {
            candidate
                .metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Puts the runner in front of a command when the runner is installed.
pub fn with_runner(mut args: Vec<String>, path: &str) -> Vec<String> {
    if find_in_path(RUNNER, path).is_some() {
        args.insert(0, RUNNER.to_string());
    }
    args
}

/// Starts a program and lets go of it.
///
/// The program is forked twice, so it is never our child: when it exits,
/// init collects it and no zombie is left for the dock to reap. It also gets
/// a session of its own, so it outlives a dock restart.
pub fn spawn_detached(args: &[String]) -> Result<(), LaunchError> {
    let Some((program, rest)) = args.split_first() else {
        return Err(LaunchError::Exec(ExecError::Empty));
    };
    let mut command = Command::new(program);
    command.args(rest).stdin(Stdio::null());
    // SAFETY: the closure runs in the forked child before exec and only
    // makes async-signal-safe calls: fork, setsid and _exit.
    unsafe {
        command.pre_exec(|| match libc::fork() {
            -1 => Err(io::Error::last_os_error()),
            0 => {
                libc::setsid();
                Ok(())
            }
            _ => libc::_exit(0),
        });
    }
    // The child seen from here is the short-lived middle process. A failed
    // exec in the grandchild is still reported through spawn().
    let mut middle = command
        .spawn()
        .map_err(|e| LaunchError::Spawn(program.clone(), e))?;
    let _ = middle.wait();
    Ok(())
}

/// Starts an app from its desktop entry, through the runner if there is one.
pub fn launch(entry: &Entry) -> Result<(), LaunchError> {
    let args = command_for(entry).map_err(LaunchError::Exec)?;
    let path = std::env::var("PATH").unwrap_or_default();
    spawn_detached(&with_runner(args, &path))
}

/// Opens the search launcher.
pub fn open_launcher() -> Result<(), LaunchError> {
    let args: Vec<String> = LAUNCHER_COMMAND.iter().map(|s| s.to_string()).collect();
    spawn_detached(&args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::testing::{entry, temp_dir};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn terminal_apps_are_wrapped_in_the_terminal() {
        let mut vim = entry("vim", "Vim", "vim %F");
        assert_eq!(command_for(&vim).unwrap(), ["vim"]);
        vim.terminal = true;
        assert_eq!(command_for(&vim).unwrap(), ["foot", "-e", "vim"]);
    }

    #[test]
    fn the_runner_is_used_only_when_installed() {
        let dir = temp_dir("path");
        let path = format!("/nonexistent:{}", dir.display());
        assert_eq!(with_runner(strings(&["foot"]), &path), ["foot"]);

        // Present but not executable does not count.
        fs::write(dir.join(RUNNER), "").unwrap();
        assert_eq!(with_runner(strings(&["foot"]), &path), ["foot"]);

        executable(&dir.join(RUNNER), "#!/bin/sh\nexec \"$@\"\n");
        assert_eq!(
            with_runner(strings(&["foot", "-x"]), &path),
            ["sengi-run", "foot", "-x"]
        );
        assert_eq!(find_in_path(RUNNER, &path), Some(dir.join(RUNNER)));
        assert_eq!(find_in_path(RUNNER, ""), None);
    }

    #[test]
    fn a_started_program_runs_with_its_arguments_and_is_not_our_child() {
        let dir = temp_dir("spawn");
        let script = dir.join("report");
        let out = dir.join("out");
        executable(
            &script,
            "#!/bin/sh\nprintf '%s|%s|%s' \"$1\" \"$2\" \"$PPID\" > \"$3\"\n",
        );
        spawn_detached(&[
            script.to_string_lossy().into_owned(),
            "two words".into(),
            "$HOME".into(),
            out.to_string_lossy().into_owned(),
        ])
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let text = loop {
            match fs::read_to_string(&out) {
                Ok(text) if text.matches('|').count() == 2 => break text,
                _ if Instant::now() > deadline => panic!("the program did not run"),
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let fields: Vec<&str> = text.split('|').collect();
        // Arguments arrive untouched: no shell has split or expanded them.
        assert_eq!(fields[0], "two words");
        assert_eq!(fields[1], "$HOME");
        // Its parent is whoever adopts orphans, not this process.
        assert_ne!(fields[2], std::process::id().to_string());
    }

    #[test]
    fn a_missing_program_is_an_error() {
        let result = spawn_detached(&strings(&["/nonexistent/program"]));
        assert!(matches!(result, Err(LaunchError::Spawn(_, _))));
        assert!(matches!(
            spawn_detached(&[]),
            Err(LaunchError::Exec(ExecError::Empty))
        ));
    }

    #[test]
    fn an_entry_without_a_command_cannot_be_launched() {
        let mut broken = entry("x", "X", "");
        broken.exec = None;
        assert!(matches!(
            launch(&broken),
            Err(LaunchError::Exec(ExecError::Empty))
        ));
    }
}
