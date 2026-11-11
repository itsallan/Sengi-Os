//! Finding and reading desktop entries (the `.desktop` files applications
//! install), following the freedesktop Desktop Entry specification.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::env::Env;
use crate::files::read_limited;

/// An installed application, as its desktop file describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    /// Desktop file ID, e.g. `org.mozilla.firefox.desktop`.
    pub id: String,
    /// The desktop file this was read from.
    pub path: PathBuf,
    /// `Name`, translated, made safe to show and cut to 256 characters.
    pub name: String,
    /// `GenericName`, treated like the name. Empty if there is none.
    pub generic_name: String,
    /// `Comment`, treated like the name. Empty if there is none.
    pub comment: String,
    /// `Keywords`, at most 64, each treated like the name.
    pub keywords: Vec<String>,
    /// The Exec value with the general string escapes already resolved.
    pub exec: String,
    /// `Icon`: an icon name or an absolute path. Empty if there is none.
    pub icon: String,
    /// `Terminal=true`: the program wants to run inside a terminal.
    pub terminal: bool,
    /// `Path`, when it is absolute: the directory to start the program in.
    pub working_dir: Option<PathBuf>,
    /// `StartupWMClass`: the window class or app_id the program's windows
    /// carry when that is not the desktop file ID.
    pub wm_class: Option<String>,
    /// `NoDisplay=true`: installed, but not for menus. Only [`scan_all`]
    /// returns such entries.
    pub no_display: bool,
}

impl DesktopEntry {
    /// An entry made by hand rather than read from a file, for previews and
    /// tests. `app_id` is the desktop file ID without `.desktop`.
    pub fn named(app_id: &str, name: &str, exec: &str) -> Self {
        DesktopEntry {
            id: format!("{app_id}.desktop"),
            path: PathBuf::from(format!("/usr/share/applications/{app_id}.desktop")),
            name: name.into(),
            generic_name: String::new(),
            comment: String::new(),
            keywords: Vec::new(),
            exec: exec.into(),
            icon: String::new(),
            terminal: false,
            working_dir: None,
            wm_class: None,
            no_display: false,
        }
    }

    /// The desktop file ID without its `.desktop` suffix, which is what a
    /// window's app_id is supposed to be.
    pub fn app_id(&self) -> &str {
        strip_suffix(&self.id)
    }
}

/// Removes `.desktop` from a desktop file ID if it is there.
pub fn strip_suffix(id: &str) -> &str {
    id.strip_suffix(".desktop").unwrap_or(id)
}

/// Which entries a scan keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// What a menu shows.
    Menu,
    /// Every installed application, shown in menus or not.
    Installed,
}

/// What one desktop file turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Not a desktop file at all: there is no `[Desktop Entry]` group. It
    /// does not hide a file of the same ID in a later directory.
    Invalid,
    /// A real entry that must not be shown, and that hides later ones.
    Hidden,
    /// An application to show.
    Visible(Box<DesktopEntry>),
}

// Deep enough for vendor subdirectories.
const MAX_DEPTH: usize = 6;
/// Directory entries looked at in one scan. A large system has a few
/// thousand desktop files at the very most.
const MAX_VISITED: usize = 10_000;
/// Desktop files are a few kilobytes; translations push some to 100 kB.
const MAX_FILE_BYTES: u64 = 1 << 20;
/// Characters kept of a name, generic name, comment or keyword.
const MAX_TEXT_CHARS: usize = 256;
const MAX_KEYWORDS: usize = 64;

/// Every application to show, from all application directories. A file in an
/// earlier directory replaces files with the same ID in later ones.
pub fn load(env: &Env) -> Vec<DesktopEntry> {
    scan(&env.application_dirs(), env)
}

/// [`load`], from the given `applications` directories, most important
/// first.
pub fn scan(dirs: &[PathBuf], env: &Env) -> Vec<DesktopEntry> {
    scan_limited(dirs, env, MAX_VISITED, Scope::Menu)
}

/// Every installed application in the given `applications` directories,
/// most important first, including the ones menus leave out: `NoDisplay`
/// entries, entries for other desktops, entries whose `TryExec` is missing
/// and entries with nothing to run. This is the list to match windows
/// against. `Hidden=true` still removes an application, here and in later
/// directories.
pub fn scan_all(dirs: &[PathBuf], env: &Env) -> Vec<DesktopEntry> {
    scan_limited(dirs, env, MAX_VISITED, Scope::Installed)
}

/// `scan`, looking at no more than `max_visited` directory entries in all.
fn scan_limited(
    dirs: &[PathBuf],
    env: &Env,
    max_visited: usize,
    scope: Scope,
) -> Vec<DesktopEntry> {
    let locales = locales(env);
    let mut seen = HashSet::new();
    let mut walk = Walk {
        visited: HashSet::new(),
        budget: max_visited,
        files: Vec::new(),
    };
    let mut entries = Vec::new();
    for dir in dirs {
        walk.files.clear();
        walk.directory(dir, "", 0);
        for (id, path) in &walk.files {
            if seen.contains(id) {
                continue;
            }
            let Some(bytes) = read_limited(path, MAX_FILE_BYTES) else {
                continue;
            };
            match parse_scoped(id, path, &bytes, &locales, env, scope) {
                Parsed::Invalid => {}
                Parsed::Hidden => {
                    seen.insert(id.clone());
                }
                Parsed::Visible(entry) => {
                    seen.insert(id.clone());
                    entries.push(*entry);
                }
            }
        }
    }
    entries
}

struct Walk {
    /// Real paths of the directories already read.
    visited: HashSet<PathBuf>,
    /// Directory entries that may still be looked at.
    budget: usize,
    /// Desktop file IDs and paths found in the directory being walked.
    files: Vec<(String, PathBuf)>,
}

impl Walk {
    fn directory(&mut self, dir: &Path, prefix: &str, depth: usize) {
        // Each real directory is read once, so a symlink back up the tree or
        // the same directory listed twice cannot produce duplicates.
        let Ok(real) = std::fs::canonicalize(dir) else {
            return;
        };
        if !self.visited.insert(real) {
            return;
        }
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        // The budget keeps a stray link to a huge tree from being walked on
        // every start.
        let mut paths: Vec<PathBuf> = read.flatten().take(self.budget).map(|e| e.path()).collect();
        self.budget -= paths.len();
        paths.sort();
        for path in paths {
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            // metadata() follows symlinks, which is how Flatpak exports its
            // files.
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    self.directory(&path, &format!("{prefix}{name}-"), depth + 1);
                }
            } else if meta.is_file() && name.ends_with(".desktop") {
                self.files.push((format!("{prefix}{name}"), path));
            }
        }
    }
}

/// The locale names to look for in `Key[locale]` for this environment, best
/// match first: the languages in `LANGUAGE`, then the message locale. As in
/// gettext, `LANGUAGE` counts only when the locale is not plain C.
pub fn locales(env: &Env) -> Vec<String> {
    let own = locale_variants(env.locale.as_deref());
    if own.is_empty() {
        return own;
    }
    let mut all: Vec<String> = Vec::new();
    let preferred = env.languages.iter().flat_map(|l| locale_variants(Some(l)));
    for locale in preferred.chain(own) {
        if !all.contains(&locale) {
            all.push(locale);
        }
    }
    all
}

/// The names one locale can be written as in `Key[locale]`, most specific
/// first.
pub fn locale_variants(locale: Option<&str>) -> Vec<String> {
    let Some(locale) = locale else {
        return Vec::new();
    };
    let (rest, modifier) = match locale.split_once('@') {
        Some((rest, modifier)) => (rest, Some(modifier)),
        None => (locale, None),
    };
    let rest = rest.split('.').next().unwrap_or("");
    let (lang, country) = match rest.split_once('_') {
        Some((lang, country)) => (lang, Some(country)),
        None => (rest, None),
    };
    if lang.is_empty() || lang == "C" || lang == "POSIX" {
        return Vec::new();
    }
    let mut variants = Vec::new();
    if let (Some(country), Some(modifier)) = (country, modifier) {
        variants.push(format!("{lang}_{country}@{modifier}"));
    }
    if let Some(country) = country {
        variants.push(format!("{lang}_{country}"));
    }
    if let Some(modifier) = modifier {
        variants.push(format!("{lang}@{modifier}"));
    }
    variants.push(lang.to_owned());
    variants
}

/// Read the `[Desktop Entry]` group. For keys with translations the best one
/// for `locales` is kept. Returns `None` if the group is missing.
fn read_group(text: &str, locales: &[String]) -> Option<HashMap<String, String>> {
    let mut found = false;
    let mut in_group = false;
    // Value and how good its locale is (lower is better).
    let mut fields: HashMap<String, (usize, String)> = HashMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            // Only the first such group counts; a repeat would be malformed.
            in_group = line == "[Desktop Entry]" && !found;
            found |= in_group;
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim_end(), value.trim_start());
        let (key, rank) = match key.split_once('[') {
            None => (key, locales.len()),
            Some((base, locale)) => {
                let Some(locale) = locale.strip_suffix(']') else {
                    continue;
                };
                match locales.iter().position(|l| l == locale) {
                    Some(rank) => (base, rank),
                    None => continue,
                }
            }
        };
        match fields.get(key) {
            Some((existing, _)) if *existing <= rank => {}
            _ => {
                fields.insert(key.to_owned(), (rank, value.to_owned()));
            }
        }
    }

    found.then(|| fields.into_iter().map(|(k, (_, v))| (k, v)).collect())
}

/// Resolve the escapes allowed in string values: `\s`, `\n`, `\t`, `\r`, `\\`.
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

/// Make a value safe to show: control characters could drive a terminal
/// through `--list`, and bidirectional overrides could reorder what the
/// panel shows. Line breaks and tabs become spaces, the rest is dropped.
fn display_text(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            '\u{200e}' | '\u{200f}' | '\u{061c}' => None,
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => None,
            c => Some(c),
        })
        .take(MAX_TEXT_CHARS)
        .collect();
    cleaned.trim().to_owned()
}

/// Split a `;` separated list. `\;` is a literal semicolon.
fn split_list(value: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&';') => {
                chars.next();
                current.push(';');
            }
            // Keep other escapes intact for unescape() below.
            '\\' => {
                current.push('\\');
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ';' => items.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    items.push(current);
    items
        .iter()
        .map(|item| display_text(&unescape(item)))
        .filter(|item| !item.is_empty())
        .take(MAX_KEYWORDS)
        .collect()
}

/// Read one desktop file. `locales` is what [`locales`] returned; `env`
/// decides the `OnlyShowIn`, `NotShowIn` and `TryExec` rules.
pub fn parse(id: &str, path: &Path, bytes: &[u8], locales: &[String], env: &Env) -> Parsed {
    parse_scoped(id, path, bytes, locales, env, Scope::Menu)
}

fn parse_scoped(
    id: &str,
    path: &Path,
    bytes: &[u8],
    locales: &[String],
    env: &Env,
    scope: Scope,
) -> Parsed {
    let text = String::from_utf8_lossy(bytes);
    let Some(fields) = read_group(&text, locales) else {
        return Parsed::Invalid;
    };
    let get = |key: &str| fields.get(key).map(String::as_str).unwrap_or("");
    let flag = |key: &str| get(key) == "true";
    let string = |key: &str| unescape(get(key)).trim().to_owned();
    let text = |key: &str| display_text(&unescape(get(key)));

    // A file placed to hide a system entry often carries nothing but
    // Hidden=true or NoDisplay=true, so anything with the group takes the ID,
    // and what cannot be shown is hidden rather than ignored.
    let name = text("Name");
    if flag("Hidden") || get("Type") != "Application" || name.is_empty() {
        return Parsed::Hidden;
    }
    let no_display = flag("NoDisplay");
    let exec = string("Exec");

    // The rest of the rules decide what a menu shows. An application left
    // out of menus is still installed, and its windows still need a name
    // and an icon.
    if scope == Scope::Menu {
        if no_display {
            return Parsed::Hidden;
        }

        let only = split_list(get("OnlyShowIn"));
        let not = split_list(get("NotShowIn"));
        let ours = |list: &[String]| list.iter().any(|d| env.desktops.contains(d));
        if (fields.contains_key("OnlyShowIn") && !ours(&only)) || ours(&not) {
            return Parsed::Hidden;
        }

        let try_exec = string("TryExec");
        if !try_exec.is_empty() && env.find_program(&try_exec).is_none() {
            return Parsed::Hidden;
        }

        if exec.is_empty() {
            // D-Bus activation is the only other way to start it, and we do
            // not speak that.
            return Parsed::Hidden;
        }
    }

    let working_dir = Some(string("Path"))
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());

    Parsed::Visible(Box::new(DesktopEntry {
        id: id.to_owned(),
        path: path.to_owned(),
        name,
        generic_name: text("GenericName"),
        comment: text("Comment"),
        keywords: split_list(get("Keywords")),
        exec,
        icon: string("Icon"),
        terminal: flag("Terminal"),
        working_dir,
        wm_class: Some(string("StartupWMClass")).filter(|class| !class.is_empty()),
        no_display,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::ffi::OsString;

    fn env_with(vars: &[(&str, String)]) -> Env {
        let vars: Vec<(String, OsString)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        Env::from_vars(move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
    }

    fn plain_env() -> Env {
        env_with(&[
            ("HOME", "/nonexistent".into()),
            ("XDG_CURRENT_DESKTOP", "sengi:wlroots".into()),
        ])
    }

    fn parse_str(text: &str, env: &Env) -> Parsed {
        let locales = locales(env);
        parse(
            "test.desktop",
            Path::new("/x/test.desktop"),
            text.as_bytes(),
            &locales,
            env,
        )
    }

    fn visible(text: &str, env: &Env) -> DesktopEntry {
        match parse_str(text, env) {
            Parsed::Visible(entry) => *entry,
            other => panic!("expected a visible entry, got {other:?}"),
        }
    }

    const BASIC: &str = "[Desktop Entry]\nType=Application\nName=Files\nExec=files %U\n";

    #[test]
    fn reads_the_usual_keys() {
        let entry = visible(
            "# a comment\n\
             [Desktop Entry]\n\
             Version=1.0\n\
             Type=Application\n\
             Name = Web Browser \n\
             GenericName=Browser\n\
             Comment=Browse the\\sweb\n\
             Keywords=internet;www;semi\\;colon;;\n\
             Exec=browser --new %u\n\
             Icon=browser\n\
             Terminal=true\n\
             Path=/srv/browser\n\
             \n\
             [Desktop Action private]\n\
             Name=Private Window\n\
             Exec=browser --private\n",
            &plain_env(),
        );
        assert_eq!(entry.id, "test.desktop");
        assert_eq!(entry.name, "Web Browser");
        assert_eq!(entry.generic_name, "Browser");
        assert_eq!(entry.comment, "Browse the web");
        assert_eq!(entry.keywords, vec!["internet", "www", "semi;colon"]);
        assert_eq!(entry.exec, "browser --new %u");
        assert_eq!(entry.icon, "browser");
        assert!(entry.terminal);
        assert_eq!(entry.working_dir, Some(PathBuf::from("/srv/browser")));
    }

    #[test]
    fn keys_outside_the_main_group_are_ignored() {
        let entry = visible(
            "[Other]\nName=Wrong\n[Desktop Entry]\nType=Application\nName=Right\nExec=right\n\
             [Desktop Entry]\nName=Repeated\n",
            &plain_env(),
        );
        assert_eq!(entry.name, "Right");
    }

    #[test]
    fn exec_keeps_its_own_quoting_after_string_unescaping() {
        let entry = visible(
            "[Desktop Entry]\nType=Application\nName=A\nExec=sh -c \"echo \\\\$HOME\"\n",
            &plain_env(),
        );
        assert_eq!(entry.exec, "sh -c \"echo \\$HOME\"");
    }

    #[test]
    fn hidden_nodisplay_and_other_types_are_not_shown() {
        let env = plain_env();
        for extra in ["Hidden=true", "NoDisplay=true"] {
            assert_eq!(
                parse_str(&format!("{BASIC}{extra}\n"), &env),
                Parsed::Hidden
            );
        }
        assert_eq!(
            parse_str(
                "[Desktop Entry]\nType=Link\nName=Site\nURL=https://example.org\n",
                &env
            ),
            Parsed::Hidden
        );
        assert_eq!(
            parse_str("[Desktop Entry]\nHidden=true\n", &env),
            Parsed::Hidden
        );
        assert_eq!(
            parse_str("[Desktop Entry]\nNoDisplay=true\n", &env),
            Parsed::Hidden
        );
        assert_eq!(
            parse_str("[Desktop Entry]\nType=Application\nName=No exec\n", &env),
            Parsed::Hidden
        );
        assert!(matches!(
            parse_str(&format!("{BASIC}Hidden=false\nNoDisplay=false\n"), &env),
            Parsed::Visible(_)
        ));
    }

    #[test]
    fn malformed_files_are_invalid_not_fatal() {
        let env = plain_env();
        assert_eq!(parse_str("", &env), Parsed::Invalid);
        assert_eq!(parse_str("just some text\n=\n[[[\n", &env), Parsed::Invalid);
        // With the group it is a desktop file, just not one to show.
        assert_eq!(
            parse_str("[Desktop Entry]\nName=No type\nExec=x\n", &env),
            Parsed::Hidden
        );
        assert_eq!(
            parse_str("[Desktop Entry]\nType=Application\nExec=x\n", &env),
            Parsed::Hidden
        );
        assert_eq!(
            parse_str("Name=Before any group\nType=Application\n", &env),
            Parsed::Invalid
        );

        let locales = Vec::new();
        let binary = [0xff, 0xfe, 0x00, b'[', 0x80, b'\n', 0xc3, 0x28];
        assert_eq!(
            parse(
                "b.desktop",
                Path::new("/b.desktop"),
                &binary,
                &locales,
                &env
            ),
            Parsed::Invalid
        );
        // Latin-1 bytes in a value are replaced, the entry still loads.
        let latin1 = b"[Desktop Entry]\nType=Application\nName=Caf\xe9\nExec=cafe\n";
        match parse("c.desktop", Path::new("/c.desktop"), latin1, &locales, &env) {
            Parsed::Visible(entry) => assert_eq!(entry.name, "Caf\u{fffd}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn show_in_lists_use_the_current_desktop() {
        let env = plain_env();
        let with = |line: &str| parse_str(&format!("{BASIC}{line}\n"), &env);
        assert_eq!(with("OnlyShowIn=GNOME;KDE;"), Parsed::Hidden);
        assert_eq!(with("OnlyShowIn="), Parsed::Hidden);
        assert!(matches!(
            with("OnlyShowIn=GNOME;sengi;"),
            Parsed::Visible(_)
        ));
        assert!(matches!(with("OnlyShowIn=wlroots"), Parsed::Visible(_)));
        assert_eq!(with("NotShowIn=sengi;"), Parsed::Hidden);
        assert!(matches!(with("NotShowIn=GNOME;"), Parsed::Visible(_)));
    }

    #[test]
    fn try_exec_must_resolve() {
        let tmp = TempDir::new();
        tmp.write("bin/present", "");
        tmp.make_executable("bin/present");
        let env = env_with(&[("PATH", tmp.path().join("bin").display().to_string())]);
        let with = |line: &str| parse_str(&format!("{BASIC}{line}\n"), &env);
        assert!(matches!(with("TryExec=present"), Parsed::Visible(_)));
        assert_eq!(with("TryExec=absent"), Parsed::Hidden);
        assert_eq!(with("TryExec=/no/such/program"), Parsed::Hidden);
        let absolute = format!("TryExec={}", tmp.path().join("bin/present").display());
        assert!(matches!(with(&absolute), Parsed::Visible(_)));
    }

    #[test]
    fn locale_variants_in_spec_order() {
        assert_eq!(
            locale_variants(Some("sr_RS.UTF-8@latin")),
            vec!["sr_RS@latin", "sr_RS", "sr@latin", "sr"]
        );
        assert_eq!(locale_variants(Some("de_DE.UTF-8")), vec!["de_DE", "de"]);
        assert_eq!(locale_variants(Some("fr")), vec!["fr"]);
        assert!(locale_variants(Some("C.UTF-8")).is_empty());
        assert!(locale_variants(Some("POSIX")).is_empty());
        assert!(locale_variants(None).is_empty());
    }

    #[test]
    fn localised_keys_win_when_the_locale_asks_for_them() {
        let text = "[Desktop Entry]\nType=Application\nExec=files\n\
                    Name[de]=Dateien\nName=Files\nName[de_AT]=Dateien AT\nName[fr]=Fichiers\n\
                    Comment=Browse files\nComment[fr]=Parcourir\nName[broken=Oops\n";
        let name_in = |lang: &str| {
            let mut vars = vec![("HOME", "/nonexistent".to_string())];
            if !lang.is_empty() {
                vars.push(("LANG", lang.to_string()));
            }
            let entry = visible(text, &env_with(&vars));
            (entry.name, entry.comment)
        };
        assert_eq!(name_in(""), ("Files".into(), "Browse files".into()));
        assert_eq!(name_in("C"), ("Files".into(), "Browse files".into()));
        assert_eq!(
            name_in("de_DE.UTF-8"),
            ("Dateien".into(), "Browse files".into())
        );
        assert_eq!(
            name_in("de_AT.UTF-8"),
            ("Dateien AT".into(), "Browse files".into())
        );
        assert_eq!(name_in("fr_FR"), ("Fichiers".into(), "Parcourir".into()));
        assert_eq!(
            name_in("ja_JP.UTF-8"),
            ("Files".into(), "Browse files".into())
        );
    }

    fn entry(name: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec=run\n")
    }

    #[test]
    fn earlier_directories_override_later_ones_by_id() {
        let tmp = TempDir::new();
        tmp.write("user/applications/editor.desktop", entry("My Editor"));
        tmp.write(
            "user/applications/gone.desktop",
            "[Desktop Entry]\nHidden=true\n",
        );
        tmp.write(
            "user/applications/broken.desktop",
            "this is not a desktop file",
        );
        tmp.write(
            "user/applications/quiet.desktop",
            "[Desktop Entry]\nNoDisplay=true\n",
        );
        tmp.write("system/applications/quiet.desktop", entry("Quiet"));
        tmp.write("user/applications/notes.txt", entry("Not a desktop file"));
        tmp.write("system/applications/editor.desktop", entry("Editor"));
        tmp.write("system/applications/gone.desktop", entry("Gone"));
        tmp.write("system/applications/broken.desktop", entry("Still here"));
        tmp.write("system/applications/kde/calc.desktop", entry("Calculator"));
        tmp.write(
            "system/applications/kde-calc.desktop",
            entry("Other Calculator"),
        );

        let env = env_with(&[
            ("HOME", "/nonexistent".into()),
            (
                "XDG_DATA_HOME",
                tmp.path().join("user").display().to_string(),
            ),
            (
                "XDG_DATA_DIRS",
                tmp.path().join("system").display().to_string(),
            ),
        ]);
        let mut found: Vec<(String, String)> =
            load(&env).into_iter().map(|e| (e.id, e.name)).collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                ("broken.desktop".to_string(), "Still here".to_string()),
                ("editor.desktop".to_string(), "My Editor".to_string()),
                // The subdirectory file sorts first and takes the ID.
                ("kde-calc.desktop".to_string(), "Calculator".to_string()),
            ]
        );
    }

    #[test]
    fn missing_directories_and_odd_files_are_skipped() {
        let tmp = TempDir::new();
        tmp.write("apps/ok.desktop", entry("OK"));
        tmp.write("apps/binary.desktop", [0u8, 159, 146, 150, 255, 10, 91]);
        tmp.write("apps/empty.desktop", "");
        std::fs::create_dir_all(tmp.path().join("apps/dir.desktop")).unwrap();
        std::os::unix::fs::symlink("/no/such/target", tmp.path().join("apps/dangling.desktop"))
            .unwrap();
        std::os::unix::fs::symlink(tmp.path().join("apps"), tmp.path().join("apps/loop")).unwrap();
        std::os::unix::fs::symlink(
            tmp.path().join("apps/ok.desktop"),
            tmp.path().join("apps/link.desktop"),
        )
        .unwrap();

        let dirs = vec![tmp.path().join("missing"), tmp.path().join("apps")];
        let found = scan(&dirs, &plain_env());
        let ids: Vec<&str> = found.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["link.desktop", "ok.desktop"]);
    }

    #[test]
    fn control_and_bidi_characters_are_stripped_from_shown_text() {
        let entry = visible(
            "[Desktop Entry]\nType=Application\nExec=app \"a\\tb\"\n\
             Name=\x1b[31mRed\x07 \u{202e}evil\u{202c}\u{9b}2J\x7f\n\
             GenericName=Two\\nLines\\tTabbed\n\
             Comment=\u{2066}iso\u{2069}late \u{200f}mark\n\
             Keywords=k\x1b]0;title\x07;\u{202d}two;\n",
            &plain_env(),
        );
        assert_eq!(entry.name, "[31mRed evil2J");
        assert_eq!(entry.generic_name, "Two Lines Tabbed");
        assert_eq!(entry.comment, "isolate mark");
        assert_eq!(entry.keywords, vec!["k]0", "title", "two"]);
        // The command is never shown, and is passed on as written.
        assert_eq!(entry.exec, "app \"a\tb\"");
    }

    #[test]
    fn a_name_of_only_control_characters_is_no_name() {
        assert_eq!(
            parse_str(
                "[Desktop Entry]\nType=Application\nName=\x1b\x07\nExec=x\n",
                &plain_env()
            ),
            Parsed::Hidden
        );
    }

    #[test]
    fn oversized_files_are_skipped_and_long_values_cut() {
        let tmp = TempDir::new();
        let long = "n".repeat(5000);
        tmp.write(
            "apps/long.desktop",
            format!(
                "[Desktop Entry]\nType=Application\nName={long}\nGenericName={long}\n\
                 Comment={long}\nKeywords={}\nExec=x\n",
                format!("{long};").repeat(100)
            ),
        );
        let mut huge = entry("Huge").into_bytes();
        huge.resize(MAX_FILE_BYTES as usize + 1, b'\n');
        tmp.write("apps/huge.desktop", huge);
        // Sparse: enormous on paper, nothing on disk.
        let sparse = std::fs::File::create(tmp.path().join("apps/sparse.desktop")).unwrap();
        sparse.set_len(16 << 30).unwrap();
        // The too-large file in front does not hide a sane one behind it.
        tmp.write("later/huge.desktop", entry("Sane"));

        let dirs = vec![tmp.path().join("apps"), tmp.path().join("later")];
        let found = scan(&dirs, &plain_env());
        let names: Vec<usize> = found.iter().map(|e| e.name.chars().count()).collect();
        assert_eq!(names, vec![MAX_TEXT_CHARS, 4]);
        let long = &found[0];
        assert_eq!(long.generic_name.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(long.comment.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(long.keywords.len(), MAX_KEYWORDS);
        assert!(long
            .keywords
            .iter()
            .all(|k| k.chars().count() == MAX_TEXT_CHARS));
    }

    #[test]
    fn a_scan_looks_at_a_bounded_number_of_entries() {
        let tmp = TempDir::new();
        for i in 0..40 {
            tmp.write(
                &format!("apps/sub{}/app{i:02}.desktop", i % 4),
                entry("App"),
            );
        }
        let dirs = vec![tmp.path().join("apps")];
        assert_eq!(scan(&dirs, &plain_env()).len(), 40);
        // Four directory entries at the top, then six more below them.
        assert_eq!(scan_limited(&dirs, &plain_env(), 10, Scope::Menu).len(), 6);
        assert!(scan_limited(&dirs, &plain_env(), 0, Scope::Menu).is_empty());
    }

    #[test]
    fn language_list_is_preferred_over_the_locale() {
        let text = "[Desktop Entry]\nType=Application\nExec=files\n\
                    Name=Files\nName[de]=Dateien\nName[fr]=Fichiers\nName[pt_BR]=Arquivos\n";
        let name_with = |lang: &str, language: &str| {
            let mut vars = vec![("HOME", "/nonexistent".to_string())];
            for (key, value) in [("LANG", lang), ("LANGUAGE", language)] {
                if !value.is_empty() {
                    vars.push((key, value.to_string()));
                }
            }
            visible(text, &env_with(&vars)).name
        };
        assert_eq!(name_with("de_DE.UTF-8", "fr"), "Fichiers");
        assert_eq!(name_with("de_DE.UTF-8", "ja:pt_BR:fr"), "Arquivos");
        assert_eq!(name_with("de_DE.UTF-8", "ja"), "Dateien");
        assert_eq!(name_with("en_GB.UTF-8", "fr:de"), "Fichiers");
        // No effect in the C locale or without one.
        assert_eq!(name_with("C", "fr"), "Files");
        assert_eq!(name_with("", "fr"), "Files");
        assert_eq!(
            locales(&env_with(&[
                ("LANG", "de_AT.UTF-8".into()),
                ("LANGUAGE", "de:en_GB".into())
            ])),
            vec!["de", "en_GB", "en", "de_AT"]
        );
    }

    // The tests below came with the dock's parser, which kept every
    // installed application.

    fn installed(text: &str) -> Parsed {
        let env = plain_env();
        parse_scoped(
            "test.desktop",
            Path::new("/x/test.desktop"),
            text.as_bytes(),
            &locales(&env),
            &env,
            Scope::Installed,
        )
    }

    const FOOT: &str = "[Desktop Entry]\nType=Application\nExec=foot\nIcon=foot\n\
        Terminal=false\nName=Foot\nName[de]=Fuss\nGenericName=Terminal\n";

    #[test]
    fn an_application_is_parsed() {
        let Parsed::Visible(entry) = installed(FOOT) else {
            panic!("not an app");
        };
        assert_eq!(entry.name, "Foot");
        assert_eq!(entry.icon, "foot");
        assert_eq!(entry.exec, "foot");
        assert_eq!(entry.app_id(), "test");
        assert!(!entry.terminal);
        assert!(!entry.no_display);
    }

    #[test]
    fn only_the_main_group_is_read() {
        let text = "[Desktop Entry]\nType=Application\nName=Web\nExec=web %u\n\
            [Desktop Action private]\nName=Private window\nExec=web --private\n";
        let Parsed::Visible(entry) = installed(text) else {
            panic!("not an app");
        };
        assert_eq!(entry.name, "Web");
        assert_eq!(entry.exec, "web %u");
    }

    #[test]
    fn comments_spaces_and_flags_are_handled() {
        let text = "# made by hand\n\n[Desktop Entry]\nType = Application\nName = Mail \n\
            Terminal=true\nNoDisplay=true\nStartupWMClass=mail-client\n";
        let Parsed::Visible(entry) = installed(text) else {
            panic!("not an app");
        };
        assert_eq!(entry.name, "Mail");
        assert!(entry.terminal);
        assert!(entry.no_display);
        assert_eq!(entry.wm_class.as_deref(), Some("mail-client"));
        assert_eq!(entry.exec, "");
        // A menu shows none of it.
        assert_eq!(parse_str(text, &plain_env()), Parsed::Hidden);
    }

    #[test]
    fn hidden_wins_over_everything_else() {
        assert_eq!(
            installed("[Desktop Entry]\nType=Application\nName=Gone\nHidden=true\n"),
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
}
