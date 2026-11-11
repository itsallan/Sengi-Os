//! Desktop entries: just enough of the Desktop Entry spec for the dock.
//!
//! sengi-launcher has a fuller parser of its own. The two should move into
//! one shared crate once both have settled.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One installed application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Desktop-file ID without the `.desktop` suffix, e.g. `org.mozilla.firefox`.
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub icon: String,
    pub exec: Option<String>,
    pub wm_class: Option<String>,
    pub terminal: bool,
    pub no_display: bool,
}

/// What a desktop file says, before it has an ID.
#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    App(Box<Entry>),
    /// `Hidden=true`: the app was removed, and this file masks any copy in a
    /// directory of lower precedence.
    Hidden,
    /// Not an application, or not a usable file.
    Other,
}

/// Undoes the escapes the spec defines for string values.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn parse(text: &str) -> Parsed {
    let mut in_main_group = false;
    let mut kind = None;
    let mut name = None;
    let mut icon = String::new();
    let mut exec = None;
    let mut wm_class = None;
    let mut terminal = false;
    let mut no_display = false;
    let mut hidden = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_main_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_main_group {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        // Translated keys such as Name[de] are skipped.
        match key.trim() {
            "Type" => kind = Some(value.to_string()),
            "Name" => name = Some(unescape(value)),
            "Icon" => icon = unescape(value),
            "Exec" => exec = Some(unescape(value)),
            "StartupWMClass" => wm_class = Some(unescape(value)),
            "Terminal" => terminal = value == "true",
            "NoDisplay" => no_display = value == "true",
            "Hidden" => hidden = value == "true",
            _ => {}
        }
    }

    if hidden {
        return Parsed::Hidden;
    }
    match (kind.as_deref(), name) {
        (Some("Application"), Some(name)) => Parsed::App(Box::new(Entry {
            id: String::new(),
            path: PathBuf::new(),
            name,
            icon,
            exec,
            wm_class,
            terminal,
            no_display,
        })),
        _ => Parsed::Other,
    }
}

/// The XDG data directories, most important first.
pub fn data_dirs_from(
    home: Option<&str>,
    data_home: Option<&str>,
    data_dirs: Option<&str>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match (data_home.filter(|s| !s.is_empty()), home) {
        (Some(dir), _) => dirs.push(PathBuf::from(dir)),
        (None, Some(home)) => dirs.push(Path::new(home).join(".local/share")),
        (None, None) => {}
    }
    let system = data_dirs
        .filter(|s| !s.is_empty())
        .unwrap_or("/usr/local/share:/usr/share");
    for dir in system.split(':').filter(|s| !s.is_empty()) {
        let dir = PathBuf::from(dir);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

pub fn data_dirs() -> Vec<PathBuf> {
    let var = |name| std::env::var(name).ok();
    data_dirs_from(
        var("HOME").as_deref(),
        var("XDG_DATA_HOME").as_deref(),
        var("XDG_DATA_DIRS").as_deref(),
    )
}

/// Removes `.desktop` from a desktop-file ID if it is there.
pub fn strip_suffix(id: &str) -> &str {
    id.strip_suffix(".desktop").unwrap_or(id)
}

/// Every installed application, with the override order already applied.
#[derive(Clone, Debug, Default)]
pub struct Apps {
    entries: Vec<Entry>,
    by_id: HashMap<String, usize>,
}

impl Apps {
    /// Reads `applications/` under each data directory. A file in an earlier
    /// directory replaces one with the same ID in a later directory.
    pub fn load(data_dirs: &[PathBuf]) -> Self {
        let mut found: HashMap<String, Option<Entry>> = HashMap::new();
        for dir in data_dirs {
            let root = dir.join("applications");
            let mut files = Vec::new();
            collect(&root, 0, &mut files);
            for path in files {
                let Ok(relative) = path.strip_prefix(&root) else {
                    continue;
                };
                // A file in a subdirectory gets its directories joined with dashes.
                let id = relative.to_string_lossy().replace('/', "-");
                let id = strip_suffix(&id).to_string();
                let key = id.to_lowercase();
                if found.contains_key(&key) {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                match parse(&text) {
                    Parsed::App(mut entry) => {
                        entry.id = id;
                        entry.path = path;
                        found.insert(key, Some(*entry));
                    }
                    Parsed::Hidden => {
                        found.insert(key, None);
                    }
                    Parsed::Other => {}
                }
            }
        }
        let mut entries: Vec<Entry> = found.into_values().flatten().collect();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Self::from_entries(entries)
    }

    pub fn from_entries(entries: Vec<Entry>) -> Self {
        let by_id = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.to_lowercase(), index))
            .collect();
        Self { entries, by_id }
    }

    /// Looks an entry up by desktop-file ID, with or without `.desktop`,
    /// ignoring case.
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.by_id
            .get(&strip_suffix(id).to_lowercase())
            .map(|&index| &self.entries[index])
    }

    /// All entries, sorted by ID.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

fn collect(dir: &Path, depth: u32, files: &mut Vec<PathBuf>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            if depth < 3 {
                collect(&path, depth + 1, files);
            }
        } else if path.extension().is_some_and(|e| e == "desktop") {
            files.push(path);
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A fresh empty directory under the system temp directory.
    pub fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sengi-dock-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    pub fn entry(id: &str, name: &str, exec: &str) -> Entry {
        Entry {
            id: id.into(),
            path: PathBuf::from(format!("/usr/share/applications/{id}.desktop")),
            name: name.into(),
            icon: id.into(),
            exec: Some(exec.into()),
            wm_class: None,
            terminal: false,
            no_display: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{temp_dir, write};
    use super::*;

    const FOOT: &str = "[Desktop Entry]\nType=Application\nExec=foot\nIcon=foot\n\
        Terminal=false\nName=Foot\nName[de]=Fuss\nGenericName=Terminal\n";

    #[test]
    fn an_application_is_parsed() {
        let Parsed::App(entry) = parse(FOOT) else {
            panic!("not an app");
        };
        assert_eq!(entry.name, "Foot");
        assert_eq!(entry.icon, "foot");
        assert_eq!(entry.exec.as_deref(), Some("foot"));
        assert!(!entry.terminal);
        assert!(!entry.no_display);
    }

    #[test]
    fn only_the_main_group_is_read() {
        let text = "[Desktop Entry]\nType=Application\nName=Web\nExec=web %u\n\
            [Desktop Action private]\nName=Private window\nExec=web --private\n";
        let Parsed::App(entry) = parse(text) else {
            panic!("not an app");
        };
        assert_eq!(entry.name, "Web");
        assert_eq!(entry.exec.as_deref(), Some("web %u"));
    }

    #[test]
    fn comments_spaces_and_flags_are_handled() {
        let text = "# made by hand\n\n[Desktop Entry]\nType = Application\nName = Mail \n\
            Terminal=true\nNoDisplay=true\nStartupWMClass=mail-client\n";
        let Parsed::App(entry) = parse(text) else {
            panic!("not an app");
        };
        assert_eq!(entry.name, "Mail");
        assert!(entry.terminal);
        assert!(entry.no_display);
        assert_eq!(entry.wm_class.as_deref(), Some("mail-client"));
        assert_eq!(entry.exec, None);
    }

    #[test]
    fn links_and_nameless_files_are_not_apps() {
        assert_eq!(
            parse("[Desktop Entry]\nType=Link\nName=Site\nURL=x\n"),
            Parsed::Other
        );
        assert_eq!(parse("[Desktop Entry]\nType=Application\n"), Parsed::Other);
        assert_eq!(parse(""), Parsed::Other);
    }

    #[test]
    fn hidden_wins_over_everything_else() {
        assert_eq!(
            parse("[Desktop Entry]\nType=Application\nName=Gone\nHidden=true\n"),
            Parsed::Hidden
        );
    }

    #[test]
    fn string_escapes_are_undone() {
        assert_eq!(unescape(r"a\sb\\c\td"), "a b\\c\td");
        // Unknown escapes are left for the Exec parser.
        assert_eq!(unescape(r#"sh -c "echo \"hi\"""#), r#"sh -c "echo \"hi\"""#);
        assert_eq!(unescape("end\\"), "end\\");
    }

    #[test]
    fn data_dirs_follow_the_environment() {
        assert_eq!(
            data_dirs_from(Some("/home/a"), None, None),
            [
                PathBuf::from("/home/a/.local/share"),
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        );
        assert_eq!(
            data_dirs_from(Some("/home/a"), Some("/data"), Some("/x:/y::/x")),
            [
                PathBuf::from("/data"),
                PathBuf::from("/x"),
                PathBuf::from("/y")
            ]
        );
        assert_eq!(
            data_dirs_from(None, Some(""), Some("")),
            [
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share")
            ]
        );
    }

    #[test]
    fn the_user_directory_overrides_the_system_one() {
        let user = temp_dir("user");
        let system = temp_dir("system");
        write(
            &user.join("applications/foot.desktop"),
            "[Desktop Entry]\nType=Application\nName=My terminal\nExec=foot -f mono\n",
        );
        write(&system.join("applications/foot.desktop"), FOOT);
        write(
            &system.join("applications/web.desktop"),
            "[Desktop Entry]\nType=Application\nName=Web\nExec=web\n",
        );

        let apps = Apps::load(&[user.clone(), system.clone()]);
        assert_eq!(apps.entries().len(), 2);
        let foot = apps.get("foot").unwrap();
        assert_eq!(foot.name, "My terminal");
        assert_eq!(foot.path, user.join("applications/foot.desktop"));
        assert_eq!(apps.get("web.desktop").unwrap().name, "Web");
    }

    #[test]
    fn a_hidden_user_file_removes_the_system_app() {
        let user = temp_dir("user");
        let system = temp_dir("system");
        write(
            &user.join("applications/foot.desktop"),
            "[Desktop Entry]\nHidden=true\n",
        );
        write(&system.join("applications/foot.desktop"), FOOT);
        let apps = Apps::load(&[user, system]);
        assert!(apps.get("foot").is_none());
    }

    #[test]
    fn ids_ignore_case_and_flatten_subdirectories() {
        let dir = temp_dir("ids");
        write(
            &dir.join("applications/kde/Okular.desktop"),
            "[Desktop Entry]\nType=Application\nName=Okular\nExec=okular\n",
        );
        write(&dir.join("applications/notes.txt"), "not a desktop file");
        let apps = Apps::load(&[dir, PathBuf::from("/nonexistent")]);
        assert_eq!(apps.entries().len(), 1);
        assert_eq!(apps.get("KDE-okular").unwrap().id, "kde-Okular");
    }
}
