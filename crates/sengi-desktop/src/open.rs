//! Starting other programs: whatever opens a folder or a file, the
//! terminal and Settings.
//!
//! The image has no file manager and no `xdg-open`. Opening finds what is
//! installed and says so when there is nothing, rather than guessing.

use std::path::Path;

use sengi_apps::{exec, Env};

pub const NO_FILE_MANAGER: &str = "No file manager installed";
pub const NO_OPENER: &str = "Nothing installed can open this file";

/// Tried in this order to open a folder when `xdg-open` is not there.
const FILE_MANAGERS: &[&str] = &[
    "sengi-files",
    "thunar",
    "pcmanfm",
    "pcmanfm-qt",
    "nautilus",
    "nemo",
    "caja",
    "dolphin",
];

/// The command that opens `path`, or what to tell the user.
pub fn open_command(env: &Env, path: &Path, folder: bool) -> Result<Vec<String>, &'static str> {
    let target = path.to_string_lossy().into_owned();
    if env.find_program("xdg-open").is_some() {
        return Ok(vec!["xdg-open".into(), target]);
    }
    if folder {
        let manager = FILE_MANAGERS
            .iter()
            .find(|name| env.find_program(name).is_some())
            .ok_or(NO_FILE_MANAGER)?;
        return Ok(vec![(*manager).into(), target]);
    }
    Err(NO_OPENER)
}

/// Starts it in its own scope under the app slice when `sengi-run` is there,
/// like every app started from the desktop.
fn start(env: &Env, mut argv: Vec<String>, dir: Option<&Path>) -> Result<(), String> {
    if env.find_program(exec::RUNNER).is_some() {
        argv.insert(0, exec::RUNNER.into());
    }
    exec::spawn(&argv, dir).map_err(|error| format!("Cannot start {}: {error}", argv[0]))
}

pub fn open(env: &Env, path: &Path, folder: bool) -> Result<(), String> {
    let argv = open_command(env, path, folder).map_err(str::to_string)?;
    start(env, argv, None)
}

/// A terminal whose working folder is `dir`.
pub fn terminal(env: &Env, dir: &Path) -> Result<(), String> {
    if env.find_program("foot").is_none() {
        return Err("No terminal installed".into());
    }
    start(env, vec!["foot".into()], Some(dir))
}

pub fn settings(env: &Env) -> Result<(), String> {
    if env.find_program("sengi-settings").is_none() {
        return Err("Settings is not installed".into());
    }
    start(env, vec!["sengi-settings".into()], None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::temp_dir;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// An environment whose PATH holds these programs and nothing else.
    fn env_with(tag: &str, programs: &[&str]) -> Env {
        let dir = temp_dir(tag);
        for program in programs {
            let path = dir.join(program);
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = OsString::from(dir);
        Env::from_vars(move |name| (name == "PATH").then(|| path.clone()))
    }

    #[test]
    fn without_a_file_manager_a_folder_says_so() {
        let env = env_with("open-none", &["foot"]);
        assert_eq!(
            open_command(&env, Path::new("/h/Desktop/a"), true),
            Err(NO_FILE_MANAGER)
        );
        assert_eq!(
            open_command(&env, Path::new("/h/Desktop/a.txt"), false),
            Err(NO_OPENER)
        );
    }

    #[test]
    fn a_file_manager_opens_a_folder_but_not_a_file() {
        let env = env_with("open-fm", &["pcmanfm"]);
        assert_eq!(
            open_command(&env, Path::new("/h/Desktop/a"), true),
            Ok(vec!["pcmanfm".to_string(), "/h/Desktop/a".to_string()])
        );
        assert_eq!(
            open_command(&env, Path::new("/h/Desktop/a.txt"), false),
            Err(NO_OPENER)
        );
    }

    #[test]
    fn files_is_the_first_file_manager_tried() {
        let env = env_with("open-files", &["thunar", "sengi-files"]);
        assert_eq!(
            open_command(&env, Path::new("/h/Desktop/a"), true),
            Ok(vec!["sengi-files".to_string(), "/h/Desktop/a".to_string()])
        );
    }

    #[test]
    fn xdg_open_is_asked_first_for_both() {
        let env = env_with("open-xdg", &["xdg-open", "thunar"]);
        for folder in [true, false] {
            assert_eq!(
                open_command(&env, Path::new("/h/Desktop/x"), folder),
                Ok(vec!["xdg-open".to_string(), "/h/Desktop/x".to_string()])
            );
        }
    }

    #[test]
    fn missing_terminal_and_settings_are_said() {
        let env = env_with("open-missing", &[]);
        assert!(terminal(&env, Path::new("/")).is_err());
        assert!(settings(&env).is_err());
    }
}
