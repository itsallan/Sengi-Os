//! Opening a file with whatever the system has for it, and what the first
//! argument of the program means.
//!
//! The image has no `xdg-open` of its own, so Files says so when it is not
//! there rather than doing nothing.

use std::path::{Path, PathBuf};

use sengi_apps::{exec, Env};

/// The command that opens `path`, or what to tell the user.
pub fn command(env: &Env, path: &Path) -> Result<Vec<String>, String> {
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    if env.find_program("xdg-open").is_none() {
        return Err(format!(
            "Cannot open \"{name}\": xdg-open is not installed, so Files does not know which app to use."
        ));
    }
    Ok(vec![
        "xdg-open".to_string(),
        path.to_string_lossy().into_owned(),
    ])
}

/// Starts the program that opens `path`, in its own scope under the app
/// slice when `sengi-run` is there, like every app started from the desktop.
pub fn open(env: &Env, path: &Path) -> Result<(), String> {
    let mut argv = command(env, path)?;
    if env.find_program(exec::RUNNER).is_some() {
        argv.insert(0, exec::RUNNER.to_string());
    }
    exec::spawn(&argv, None).map_err(|error| format!("Cannot start {}: {error}", argv[0]))
}

/// Where to start for the argument the program was given: a folder, a
/// `file://` address, or a file, which stands for the folder it is in. None
/// when there is no argument or the place does not exist.
pub fn start_place(arg: &str, cwd: &Path) -> Result<PathBuf, String> {
    let plain = match arg.strip_prefix("file://") {
        Some(rest) => {
            // file:///home/a, or file://localhost/home/a.
            let rest = rest.strip_prefix("localhost").unwrap_or(rest);
            sengi_system::trash::decode(rest)
        }
        None => PathBuf::from(arg),
    };
    let path = if plain.is_absolute() {
        plain
    } else {
        cwd.join(plain)
    };
    let resolved = std::fs::canonicalize(&path)
        .map_err(|_| format!("\"{}\" does not exist.", path.display()))?;
    if resolved.is_dir() {
        Ok(resolved)
    } else {
        resolved
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("\"{}\" cannot be shown.", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::tests::Temp;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn env_with(programs: &[&str]) -> (Temp, Env) {
        let t = Temp::new();
        for program in programs {
            let path = t.path().join(program);
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = OsString::from(t.path());
        let env = Env::from_vars(move |name| (name == "PATH").then(|| path.clone()));
        (t, env)
    }

    #[test]
    fn xdg_open_opens_a_file() {
        let (_t, env) = env_with(&["xdg-open"]);
        assert_eq!(
            command(&env, Path::new("/h/a b.txt")),
            Ok(vec!["xdg-open".to_string(), "/h/a b.txt".to_string()])
        );
    }

    #[test]
    fn without_xdg_open_the_notice_names_the_file_and_the_reason() {
        let (_t, env) = env_with(&["foot"]);
        let notice = command(&env, Path::new("/h/report.pdf")).unwrap_err();
        assert!(notice.contains("\"report.pdf\""), "{notice}");
        assert!(notice.contains("xdg-open is not installed"), "{notice}");
        assert!(open(&env, Path::new("/h/report.pdf")).is_err());
    }

    #[test]
    fn the_argument_may_be_a_folder_an_address_or_a_file() {
        let t = Temp::new();
        t.file("with space/a.txt", "");
        let dir = t.path().join("with space");
        assert_eq!(
            start_place(dir.to_str().unwrap(), Path::new("/")),
            Ok(dir.clone())
        );
        assert_eq!(
            start_place(
                &format!("file://{}/with%20space", t.path().display()),
                Path::new("/")
            ),
            Ok(dir.clone())
        );
        assert_eq!(
            start_place(
                &format!("file://localhost{}", dir.display()),
                Path::new("/")
            ),
            Ok(dir.clone())
        );
        assert_eq!(start_place("with space/a.txt", t.path()), Ok(dir.clone()));
        assert_eq!(start_place("with space/../with space", t.path()), Ok(dir));
        assert!(start_place("nowhere", t.path())
            .unwrap_err()
            .contains("does not exist"));
    }
}
