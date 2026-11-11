//! Starting programs from the dock. Building the command and letting go of
//! the program are sengi-apps' business; this is the dock's wording for
//! what can go wrong, and the search button.

use std::fmt;
use std::io;

use sengi_apps::exec::{self, ExecError};
use sengi_apps::{DesktopEntry, Env};

/// What the search button runs.
pub const LAUNCHER_COMMAND: &[&str] = &["sengi-launcher"];

#[derive(Debug)]
pub enum LaunchError {
    Exec(ExecError),
    /// The program the entry names is not installed.
    Missing(String),
    Spawn(String, io::Error),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exec(e) => write!(f, "{e}"),
            Self::Missing(program) => write!(f, "cannot start {program}: it is not installed"),
            Self::Spawn(program, e) => write!(f, "cannot start {program}: {e}"),
        }
    }
}

impl std::error::Error for LaunchError {}

impl From<ExecError> for LaunchError {
    fn from(error: ExecError) -> Self {
        match error {
            ExecError::NotFound(program) => Self::Missing(program),
            other => Self::Exec(other),
        }
    }
}

fn spawn(args: &[String], entry: Option<&DesktopEntry>) -> Result<(), LaunchError> {
    let dir = entry.and_then(|entry| entry.working_dir.as_deref());
    exec::spawn(args, dir)
        .map_err(|e| LaunchError::Spawn(args.first().cloned().unwrap_or_default(), e))
}

/// The full command for an entry, checked and with the runner in front.
///
/// The check matters: the dock starts the runner, and the runner succeeds
/// whether or not the app it is given exists.
pub fn prepare(entry: &DesktopEntry, env: &Env) -> Result<Vec<String>, LaunchError> {
    Ok(exec::launchable(entry, env)?)
}

/// Starts an app from its desktop entry, through the runner if there is one.
pub fn launch(entry: &DesktopEntry) -> Result<(), LaunchError> {
    let env = Env::from_process();
    spawn(&prepare(entry, &env)?, Some(entry))
}

/// Opens the search launcher.
pub fn open_launcher() -> Result<(), LaunchError> {
    let args: Vec<String> = LAUNCHER_COMMAND.iter().map(|s| s.to_string()).collect();
    if Env::from_process().find_program(&args[0]).is_none() {
        return Err(LaunchError::Missing(args[0].clone()));
    }
    spawn(&args, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{entry, temp_dir};
    use sengi_apps::exec::RUNNER;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn env_with_path(path: &str) -> Env {
        let path = OsString::from(path);
        Env::from_vars(move |name| (name == "PATH").then(|| path.clone()))
    }

    #[test]
    fn a_missing_program_is_an_error() {
        let result = spawn(&["/nonexistent/program".to_string()], None);
        assert!(
            matches!(result, Err(LaunchError::Spawn(program, _)) if program == "/nonexistent/program")
        );
        assert!(matches!(spawn(&[], None), Err(LaunchError::Spawn(_, _))));
    }

    #[test]
    fn a_missing_app_is_caught_before_the_runner_hides_it() {
        let dir = temp_dir("missing");
        let env = env_with_path(&dir.to_string_lossy());
        executable(&dir.join(RUNNER), "#!/bin/sh\nexec \"$@\"\n");
        executable(&dir.join("present"), "#!/bin/sh\n");
        let runner = dir.join(RUNNER).to_string_lossy().into_owned();

        let found = prepare(&entry("present", "Present", "present --flag %U"), &env).unwrap();
        assert_eq!(found, [runner.as_str(), "present", "--flag"]);

        let gone = prepare(&entry("gone", "Gone", "gone-app %U"), &env);
        assert!(matches!(gone, Err(LaunchError::Missing(program)) if program == "gone-app"));
        assert_eq!(
            LaunchError::Missing("gone-app".into()).to_string(),
            "cannot start gone-app: it is not installed"
        );

        // A full path is checked where it points, not on PATH.
        let absolute = dir.join("present").to_string_lossy().into_owned();
        assert!(prepare(&entry("p", "P", &absolute), &env_with_path("")).is_ok());
        assert!(prepare(&entry("p", "P", "/nonexistent/app"), &env).is_err());

        // A terminal app needs its terminal too.
        let mut tui = entry("present", "Present", "present");
        tui.terminal = true;
        assert!(matches!(
            prepare(&tui, &env),
            Err(LaunchError::Missing(program)) if program == "foot"
        ));
    }

    #[test]
    fn an_entry_without_a_command_cannot_be_launched() {
        let broken = entry("x", "X", "");
        assert!(matches!(
            launch(&broken),
            Err(LaunchError::Exec(ExecError::Empty))
        ));
    }
}
