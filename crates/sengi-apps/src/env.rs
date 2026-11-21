//! The parts of the process environment that finding and starting
//! applications depends on, gathered once so the rest can be tested without
//! touching real variables.

use std::ffi::OsString;
use std::path::PathBuf;

/// The environment, read once. Relative directories are ignored, as the
/// base directory specification asks.
#[derive(Debug, Clone)]
pub struct Env {
    /// `HOME`, or `/` without one.
    pub home: PathBuf,
    /// `XDG_DATA_HOME`, or `~/.local/share`.
    pub data_home: PathBuf,
    /// `XDG_DATA_DIRS`, or `/usr/local/share` and `/usr/share`.
    pub data_dirs: Vec<PathBuf>,
    /// `XDG_STATE_HOME`, or `~/.local/state`.
    pub state_home: PathBuf,
    /// `XDG_RUNTIME_DIR`, if it is set.
    pub runtime_dir: Option<PathBuf>,
    /// The directories of `PATH`.
    pub path: Vec<PathBuf>,
    /// The message locale, e.g. `de_DE.UTF-8`, if one is set.
    pub locale: Option<String>,
    /// Preferred languages from `LANGUAGE`, best first.
    pub languages: Vec<String>,
    /// Desktop names from `XDG_CURRENT_DESKTOP`.
    pub desktops: Vec<String>,
}

impl Env {
    /// The environment of this process.
    pub fn from_process() -> Self {
        Self::from_vars(|name| std::env::var_os(name))
    }

    /// An environment from a lookup function, for tests.
    pub fn from_vars(get: impl Fn(&str) -> Option<OsString>) -> Self {
        let set = |name: &str| get(name).filter(|v| !v.is_empty());
        // The base directory spec says relative paths are to be ignored.
        let dir = |name: &str| set(name).map(PathBuf::from).filter(|p| p.is_absolute());

        let home = dir("HOME").unwrap_or_else(|| PathBuf::from("/"));
        let data_home = dir("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
        let state_home = dir("XDG_STATE_HOME").unwrap_or_else(|| home.join(".local/state"));

        let mut data_dirs: Vec<PathBuf> = set("XDG_DATA_DIRS")
            .map(|v| {
                std::env::split_paths(&v)
                    .filter(|p| p.is_absolute())
                    .collect()
            })
            .unwrap_or_default();
        if data_dirs.is_empty() {
            data_dirs = vec!["/usr/local/share".into(), "/usr/share".into()];
        }

        let path = set("PATH")
            .map(|v| {
                std::env::split_paths(&v)
                    .filter(|p| !p.as_os_str().is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|name| set(name))
            .map(|v| v.to_string_lossy().into_owned());

        let languages = set("LANGUAGE")
            .map(|v| {
                v.to_string_lossy()
                    .split(':')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let desktops = match set("XDG_CURRENT_DESKTOP") {
            Some(v) => v
                .to_string_lossy()
                .split(':')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            None => vec!["sengi".to_owned(), "wlroots".to_owned()],
        };

        Env {
            home,
            data_home,
            data_dirs,
            state_home,
            runtime_dir: dir("XDG_RUNTIME_DIR"),
            path,
            locale,
            languages,
            desktops,
        }
    }

    /// The data directories applications install into, most important
    /// first: the user's, the system's, then Flatpak's exports.
    pub fn data_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.data_home.clone()];
        roots.extend(self.data_dirs.iter().cloned());
        // Flatpak normally adds these to XDG_DATA_DIRS itself, but only for
        // sessions started after it was installed.
        roots.push(self.home.join(".local/share/flatpak/exports/share"));
        roots.push("/var/lib/flatpak/exports/share".into());

        let mut unique: Vec<PathBuf> = Vec::new();
        for root in roots {
            if !unique.contains(&root) {
                unique.push(root);
            }
        }
        unique
    }

    /// Where desktop files live, most important first.
    pub fn application_dirs(&self) -> Vec<PathBuf> {
        self.data_roots()
            .into_iter()
            .map(|root| root.join("applications"))
            .collect()
    }

    /// Find a program given as an absolute path or as a bare name to look up
    /// in PATH. The Desktop Entry spec allows nothing else, and a relative
    /// path would depend on where the caller happened to be started.
    pub fn find_program(&self, name: &str) -> Option<PathBuf> {
        if name.is_empty() {
            return None;
        }
        if name.contains('/') {
            let path = PathBuf::from(name);
            return (path.is_absolute() && is_executable(&path)).then_some(path);
        }
        self.path
            .iter()
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    }
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn env(vars: &[(&str, &str)]) -> Env {
        let vars: Vec<(String, OsString)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        Env::from_vars(move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
    }

    #[test]
    fn defaults_follow_the_base_directory_spec() {
        let e = env(&[("HOME", "/home/a")]);
        assert_eq!(e.data_home, PathBuf::from("/home/a/.local/share"));
        assert_eq!(e.state_home, PathBuf::from("/home/a/.local/state"));
        assert_eq!(
            e.data_dirs,
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share")
            ]
        );
        assert_eq!(e.desktops, vec!["sengi", "wlroots"]);
        assert_eq!(e.locale, None);
        assert_eq!(e.runtime_dir, None);
    }

    #[test]
    fn empty_and_relative_values_fall_back() {
        let e = env(&[
            ("HOME", "/home/a"),
            ("XDG_DATA_HOME", ""),
            ("XDG_STATE_HOME", "relative/state"),
            ("XDG_DATA_DIRS", "relative:/opt/share"),
        ]);
        assert_eq!(e.data_home, PathBuf::from("/home/a/.local/share"));
        assert_eq!(e.state_home, PathBuf::from("/home/a/.local/state"));
        assert_eq!(e.data_dirs, vec![PathBuf::from("/opt/share")]);
    }

    #[test]
    fn application_dirs_are_ordered_and_include_flatpak_once() {
        let e = env(&[
            ("HOME", "/home/a"),
            (
                "XDG_DATA_DIRS",
                "/var/lib/flatpak/exports/share:/usr/share:/usr/share",
            ),
        ]);
        assert_eq!(
            e.application_dirs(),
            vec![
                PathBuf::from("/home/a/.local/share/applications"),
                PathBuf::from("/var/lib/flatpak/exports/share/applications"),
                PathBuf::from("/usr/share/applications"),
                PathBuf::from("/home/a/.local/share/flatpak/exports/share/applications"),
            ]
        );
    }

    #[test]
    fn locale_precedence_and_desktop_list() {
        let e = env(&[
            ("LANG", "en_GB.UTF-8"),
            ("LC_MESSAGES", "de_DE.UTF-8"),
            ("XDG_CURRENT_DESKTOP", "sengi:wlroots"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]);
        assert_eq!(e.runtime_dir, Some(PathBuf::from("/run/user/1000")));
        assert_eq!(e.locale.as_deref(), Some("de_DE.UTF-8"));
        assert_eq!(e.desktops, vec!["sengi", "wlroots"]);
    }

    #[test]
    fn find_program_checks_path_and_the_executable_bit() {
        let tmp = TempDir::new();
        tmp.write("bin/tool", "#!/bin/sh\n");
        tmp.write("bin/data", "not a program");
        tmp.make_executable("bin/tool");
        let bin = tmp.path().join("bin");
        let e = env(&[("PATH", bin.to_str().unwrap())]);

        assert_eq!(e.find_program("tool"), Some(bin.join("tool")));
        assert_eq!(e.find_program("data"), None);
        assert_eq!(e.find_program("missing"), None);
        assert_eq!(e.find_program(""), None);
        let absolute = bin.join("tool");
        assert_eq!(
            e.find_program(absolute.to_str().unwrap()),
            Some(absolute.clone())
        );
        assert_eq!(e.find_program(bin.to_str().unwrap()), None);
    }

    #[test]
    fn find_program_refuses_relative_paths() {
        let tmp = TempDir::new();
        tmp.write("bin/tool", "");
        tmp.make_executable("bin/tool");
        let e = env(&[("PATH", tmp.path().to_str().unwrap())]);
        // Even where it would resolve against PATH or the working directory.
        assert_eq!(e.find_program("bin/tool"), None);
        assert_eq!(e.find_program("./tool"), None);
        assert_eq!(e.find_program("../bin/tool"), None);
    }
}
