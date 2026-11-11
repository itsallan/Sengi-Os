//! The parts of the process environment the launcher reads, gathered once so
//! the rest of the crate can be tested without touching real variables.

use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub data_home: PathBuf,
    pub data_dirs: Vec<PathBuf>,
    pub state_home: PathBuf,
    pub path: Vec<PathBuf>,
    /// The message locale, e.g. `de_DE.UTF-8`, if one is set.
    pub locale: Option<String>,
    /// Desktop names from `XDG_CURRENT_DESKTOP`.
    pub desktops: Vec<String>,
}

impl Env {
    pub fn from_process() -> Self {
        Self::from_vars(|name| std::env::var_os(name))
    }

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
            path,
            locale,
            desktops,
        }
    }

    /// Where desktop files live, most important first.
    pub fn application_dirs(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.data_home.clone()];
        roots.extend(self.data_dirs.iter().cloned());
        // Flatpak normally adds these to XDG_DATA_DIRS itself, but only for
        // sessions started after it was installed.
        roots.push(self.home.join(".local/share/flatpak/exports/share"));
        roots.push("/var/lib/flatpak/exports/share".into());

        let mut dirs: Vec<PathBuf> = Vec::new();
        for root in roots {
            let dir = root.join("applications");
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    pub fn usage_file(&self) -> PathBuf {
        self.state_home.join("sengi-launcher/usage")
    }

    /// Find a program the way a shell would, without running one.
    pub fn find_program(&self, name: &str) -> Option<PathBuf> {
        if name.is_empty() {
            return None;
        }
        if name.contains('/') {
            let path = PathBuf::from(name);
            return is_executable(&path).then_some(path);
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
        assert_eq!(
            e.usage_file(),
            PathBuf::from("/home/a/.local/state/sengi-launcher/usage")
        );
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
        ]);
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
}
