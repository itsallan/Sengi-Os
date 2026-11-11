//! Finding and reading desktop entries (the `.desktop` files applications
//! install), following the freedesktop Desktop Entry specification.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::env::Env;

/// An application the launcher can show and start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    /// Desktop file ID, e.g. `org.mozilla.firefox.desktop`.
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub generic_name: String,
    pub comment: String,
    pub keywords: Vec<String>,
    /// The Exec value with the general string escapes already resolved.
    pub exec: String,
    pub icon: String,
    pub terminal: bool,
    pub working_dir: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Not a usable desktop file. It does not hide a file of the same ID in
    /// a later directory.
    Invalid,
    /// A real entry that must not be shown, and that hides later ones.
    Hidden,
    Visible(Box<DesktopEntry>),
}

// Deep enough for vendor subdirectories.
const MAX_DEPTH: usize = 6;

/// Every application to show, from all application directories. A file in an
/// earlier directory replaces files with the same ID in later ones.
pub fn load(env: &Env) -> Vec<DesktopEntry> {
    scan(&env.application_dirs(), env)
}

pub fn scan(dirs: &[PathBuf], env: &Env) -> Vec<DesktopEntry> {
    let locales = locale_variants(env.locale.as_deref());
    let mut seen = HashSet::new();
    let mut visited = HashSet::new();
    let mut entries = Vec::new();
    for dir in dirs {
        let mut files = Vec::new();
        walk(dir, "", 0, &mut visited, &mut files);
        for (id, path) in files {
            if seen.contains(&id) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            match parse(&id, &path, &bytes, &locales, env) {
                Parsed::Invalid => {}
                Parsed::Hidden => {
                    seen.insert(id);
                }
                Parsed::Visible(entry) => {
                    seen.insert(id);
                    entries.push(*entry);
                }
            }
        }
    }
    entries
}

fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<(String, PathBuf)>,
) {
    // Each real directory is read once, so a symlink back up the tree or the
    // same directory listed twice cannot produce duplicates.
    let Ok(real) = std::fs::canonicalize(dir) else {
        return;
    };
    if !visited.insert(real) {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        // metadata() follows symlinks, which is how Flatpak exports its files.
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if depth < MAX_DEPTH {
                walk(&path, &format!("{prefix}{name}-"), depth + 1, visited, out);
            }
        } else if meta.is_file() && name.ends_with(".desktop") {
            out.push((format!("{prefix}{name}"), path));
        }
    }
}

/// The locale names to look for in `Key[locale]`, best match first.
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
        .map(|item| unescape(item).trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

pub fn parse(id: &str, path: &Path, bytes: &[u8], locales: &[String], env: &Env) -> Parsed {
    let text = String::from_utf8_lossy(bytes);
    let Some(fields) = read_group(&text, locales) else {
        return Parsed::Invalid;
    };
    let get = |key: &str| fields.get(key).map(String::as_str).unwrap_or("");
    let flag = |key: &str| get(key) == "true";
    let string = |key: &str| unescape(get(key)).trim().to_owned();

    // Checked first: a file whose only job is to hide a system entry often
    // carries nothing but Hidden=true.
    if flag("Hidden") {
        return Parsed::Hidden;
    }
    let name = string("Name");
    if get("Type").is_empty() || name.is_empty() {
        return Parsed::Invalid;
    }
    if get("Type") != "Application" || flag("NoDisplay") {
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

    let exec = string("Exec");
    if exec.is_empty() {
        // D-Bus activation is the only other way to start it, and we do not
        // speak that.
        return Parsed::Hidden;
    }

    let working_dir = Some(string("Path"))
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());

    Parsed::Visible(Box::new(DesktopEntry {
        id: id.to_owned(),
        path: path.to_owned(),
        name,
        generic_name: string("GenericName"),
        comment: string("Comment"),
        keywords: split_list(get("Keywords")),
        exec,
        icon: string("Icon"),
        terminal: flag("Terminal"),
        working_dir,
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
        let locales = locale_variants(env.locale.as_deref());
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
        assert_eq!(
            parse_str("[Desktop Entry]\nName=No type\nExec=x\n", &env),
            Parsed::Invalid
        );
        assert_eq!(
            parse_str("[Desktop Entry]\nType=Application\nExec=x\n", &env),
            Parsed::Invalid
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
}
