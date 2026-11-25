//! The dock's configuration: which apps are pinned.
//!
//! The file is `sengi-os/dock.toml` under the XDG config directories. It
//! holds one key, so it is read by hand rather than with a TOML library:
//!
//! ```toml
//! pinned = ["foot", "firefox-esr"]
//! ```

use std::fs;
use std::path::{Path, PathBuf};

/// Used when no config file exists or the one found cannot be read.
pub const DEFAULT_PINNED: &[&str] = &["foot"];

const FILE: &str = "sengi-os/dock.toml";

/// Removes a `#` comment, minding that `#` may appear inside a string.
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    for (index, c) in line.char_indices() {
        match c {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => {}
        }
    }
    line
}

/// Reads the pinned list: desktop-file IDs, in dock order.
pub fn parse(text: &str) -> Result<Vec<String>, String> {
    parse_with_warnings(text).map(|(pinned, _)| pinned)
}

/// As [`parse`], also returning what was skipped. A key this version does
/// not know is no reason to throw the whole file away: it may be meant for
/// a newer dock.
pub fn parse_with_warnings(text: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let body: String = text
        .lines()
        .map(strip_comment)
        .collect::<Vec<_>>()
        .join("\n");

    let mut rest = body.trim_start();
    let mut pinned = None;
    let mut warnings = Vec::new();
    while !rest.is_empty() {
        let (key, after) = rest
            .split_once('=')
            .ok_or_else(|| format!("expected `key = value` at `{}`", first_line(rest)))?;
        let key = key.trim();
        let after = after.trim_start_matches([' ', '\t']);
        if key != "pinned" {
            // Skip its value: a list up to the closing bracket, anything
            // else up to the end of the line.
            let tail = match after.strip_prefix('[') {
                Some(list) => list.split_once(']').map(|(_, tail)| tail),
                None => Some(after.split_once('\n').map_or("", |(_, tail)| tail)),
            };
            let tail = tail.ok_or_else(|| format!("the `{key}` list is not closed"))?;
            warnings.push(format!("unknown key `{key}` ignored"));
            rest = tail.trim_start();
            continue;
        }
        let after = after
            .strip_prefix('[')
            .ok_or("`pinned` must be a list: pinned = [\"foot\"]")?;
        let (list, tail) = after
            .split_once(']')
            .ok_or("the `pinned` list is not closed")?;
        pinned = Some(parse_list(list)?);
        rest = tail.trim_start();
    }
    let pinned = pinned.ok_or_else(|| "no `pinned` list".to_string())?;
    Ok((pinned, warnings))
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

fn parse_list(list: &str) -> Result<Vec<String>, String> {
    let mut ids: Vec<String> = Vec::new();
    for item in list.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let id = item
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .filter(|s| !s.contains('"'))
            .ok_or_else(|| format!("`{item}` should be a quoted desktop-file ID"))?;
        let id = id.trim();
        // Pinning an app twice would give it two icons.
        if !id.is_empty() && !ids.iter().any(|seen| seen.eq_ignore_ascii_case(id)) {
            ids.push(id.to_string());
        }
    }
    Ok(ids)
}

/// Where the config may live, most important first: the user's file, then
/// the system ones.
pub fn paths_from(
    home: Option<&str>,
    config_home: Option<&str>,
    config_dirs: Option<&str>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    match (config_home.filter(|s| !s.is_empty()), home) {
        (Some(dir), _) => paths.push(Path::new(dir).join(FILE)),
        (None, Some(home)) => paths.push(Path::new(home).join(".config").join(FILE)),
        (None, None) => {}
    }
    let system = config_dirs.filter(|s| !s.is_empty()).unwrap_or("/etc/xdg");
    for dir in system.split(':').filter(|s| !s.is_empty()) {
        paths.push(Path::new(dir).join(FILE));
    }
    paths
}

/// The user's own file, the only one the dock writes.
pub fn user_path_from(home: Option<&str>, config_home: Option<&str>) -> Option<PathBuf> {
    match (config_home.filter(|s| s.starts_with('/')), home) {
        (Some(dir), _) => Some(Path::new(dir).join(FILE)),
        (None, Some(home)) if home.starts_with('/') => {
            Some(Path::new(home).join(".config").join(FILE))
        }
        _ => None,
    }
}

pub fn user_path() -> Option<PathBuf> {
    let var = |name| std::env::var(name).ok();
    user_path_from(var("HOME").as_deref(), var("XDG_CONFIG_HOME").as_deref())
}

/// The text of the file with `pinned` set to `ids`. Everything else in
/// `existing` (comments, keys for a newer dock) is kept as it was; only
/// the `pinned` list is replaced.
pub fn render(existing: Option<&str>, ids: &[String]) -> String {
    let list = if ids.is_empty() {
        "pinned = []".to_string()
    } else {
        let items: Vec<String> = ids
            .iter()
            .filter(|id| !id.contains(['"', '\n', '\r']))
            .map(|id| format!("    \"{id}\","))
            .collect();
        format!("pinned = [\n{}\n]", items.join("\n"))
    };
    let Some(text) = existing else {
        return format!("# Apps pinned to the dock, in order. Written by sengi-dock.\n{list}\n");
    };
    match pinned_span(text) {
        Some((start, end)) => format!("{}{list}{}", &text[..start], &text[end..]),
        None => {
            let sep = if text.is_empty() || text.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            format!("{text}{sep}{list}\n")
        }
    }
}

/// Where the `pinned = [...]` entry starts and ends in the text.
fn pinned_span(text: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let code = strip_comment(line);
        let Some(rest) = code.trim_start().strip_prefix("pinned") else {
            continue;
        };
        if !rest.trim_start().starts_with('=') {
            continue;
        }
        let from = start + line.len() - line.trim_start().len();
        // The list may run over several lines. Look for its closing
        // bracket outside strings and comments.
        let mut in_string = false;
        let mut in_comment = false;
        for (index, c) in text[from..].char_indices() {
            match c {
                '\n' => in_comment = false,
                '#' if !in_string => in_comment = true,
                '"' if !in_comment => in_string = !in_string,
                ']' if !in_string && !in_comment => return Some((from, from + index + 1)),
                _ => {}
            }
        }
        return None;
    }
    None
}

/// Saves the pinned list to `path`, creating the directory if need be. The
/// file is written whole under another name and moved into place, so a
/// crash cannot leave half a list. A file that cannot be read as a dock
/// config is kept as `dock.toml.bak` first.
pub fn save(path: &Path, ids: &[String]) -> std::io::Result<()> {
    let existing = match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut text = render(existing.as_deref(), ids);
    if let (Some(old), true) = (&existing, parse(&text).is_err()) {
        // The old file has a mistake the edit cannot get around.
        let mut backup = path.as_os_str().to_owned();
        backup.push(".bak");
        fs::write(&backup, old)?;
        text = render(None, ids);
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".new");
    let temp = PathBuf::from(temp);
    fs::write(&temp, text)?;
    fs::rename(&temp, path)
}

pub fn paths() -> Vec<PathBuf> {
    let var = |name| std::env::var(name).ok();
    paths_from(
        var("HOME").as_deref(),
        var("XDG_CONFIG_HOME").as_deref(),
        var("XDG_CONFIG_DIRS").as_deref(),
    )
}

/// The pinned list from the first file that exists. The user's file replaces
/// the system list rather than adding to it, so apps can be unpinned. A file
/// with a mistake in it is reported and skipped.
pub fn pinned(paths: &[PathBuf]) -> Vec<String> {
    for path in paths {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                // Having no file is normal. Having one that cannot be read is not.
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("sengi-dock: cannot read {}: {error}", path.display());
                }
                continue;
            }
        };
        match parse_with_warnings(&text) {
            Ok((pinned, warnings)) => {
                for warning in warnings {
                    eprintln!("sengi-dock: {}: {warning}", path.display());
                }
                return pinned;
            }
            Err(error) => eprintln!("sengi-dock: {}: {error}", path.display()),
        }
    }
    DEFAULT_PINNED.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{temp_dir, write};

    #[test]
    fn a_one_line_list_is_read() {
        assert_eq!(
            parse("pinned = [\"foot\", \"firefox-esr\"]\n").unwrap(),
            ["foot", "firefox-esr"]
        );
    }

    #[test]
    fn lists_may_span_lines_and_carry_comments() {
        let text = "# Apps pinned to the dock.\n\npinned = [\n    \"foot\",  # terminal\n    \
            \"org.mozilla.firefox.desktop\",\n]\n";
        assert_eq!(
            parse(text).unwrap(),
            ["foot", "org.mozilla.firefox.desktop"]
        );
    }

    #[test]
    fn an_empty_list_means_nothing_is_pinned() {
        assert_eq!(parse("pinned = []").unwrap(), Vec::<String>::new());
        assert_eq!(parse("pinned=[ ]").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn duplicates_are_dropped() {
        assert_eq!(
            parse("pinned = [\"foot\", \"web\", \"Foot\"]").unwrap(),
            ["foot", "web"]
        );
    }

    #[test]
    fn a_hash_inside_a_name_is_not_a_comment() {
        assert_eq!(parse("pinned = [\"c#-ide\"] # note").unwrap(), ["c#-ide"]);
    }

    #[test]
    fn unknown_keys_are_skipped_with_a_warning() {
        let text = "size = 48\nlabel = \"a = b\"\npinned = [\"foot\"]\n\
            hidden = [\n  \"x\",\n  \"y\",\n]\nautohide = true\n";
        let (pinned, warnings) = parse_with_warnings(text).unwrap();
        assert_eq!(pinned, ["foot"]);
        assert_eq!(
            warnings,
            [
                "unknown key `size` ignored",
                "unknown key `label` ignored",
                "unknown key `hidden` ignored",
                "unknown key `autohide` ignored",
            ]
        );
        assert_eq!(parse(text).unwrap(), ["foot"]);
        assert!(parse_with_warnings("pinned = []").unwrap().1.is_empty());
        // Still an error: nothing says what is pinned.
        assert!(parse("size = 48\n").is_err());
        assert!(parse("hidden = [\"x\"\npinned = [\"foot\"").is_err());
    }

    #[test]
    fn a_user_file_with_an_unknown_key_is_still_used() {
        let dir = temp_dir("config");
        let user = dir.join("user/dock.toml");
        let system = dir.join("system/dock.toml");
        write(&system, "pinned = [\"foot\", \"web\"]\n");
        write(&user, "theme = \"dark\"\npinned = [\"mail\"]\n");
        assert_eq!(pinned(&[user, system]), ["mail"]);
    }

    #[test]
    fn an_unreadable_file_falls_through_to_the_next() {
        let dir = temp_dir("config");
        // A directory where the file should be cannot be read as one.
        let user = dir.join("user/dock.toml");
        std::fs::create_dir_all(&user).unwrap();
        let system = dir.join("system/dock.toml");
        write(&system, "pinned = [\"web\"]\n");
        assert_eq!(pinned(&[user, system]), ["web"]);
    }

    #[test]
    fn mistakes_are_errors() {
        assert!(parse("").is_err());
        assert!(parse("# only a comment\n").is_err());
        assert!(parse("pinned = \"foot\"").is_err());
        assert!(parse("pinned = [\"foot\"").is_err());
        assert!(parse("pinned = [foot]").is_err());
        assert!(parse("pinned = [\"a\" \"b\"]").is_err());
        assert!(parse("pins = [\"foot\"]").is_err());
        assert!(parse("pinned = [\"foot\"]\nleftover").is_err());
    }

    #[test]
    fn the_user_file_comes_before_the_system_ones() {
        assert_eq!(
            paths_from(Some("/home/a"), None, None),
            [
                PathBuf::from("/home/a/.config/sengi-os/dock.toml"),
                PathBuf::from("/etc/xdg/sengi-os/dock.toml"),
            ]
        );
        assert_eq!(
            paths_from(Some("/home/a"), Some("/cfg"), Some("/a:/b")),
            [
                PathBuf::from("/cfg/sengi-os/dock.toml"),
                PathBuf::from("/a/sengi-os/dock.toml"),
                PathBuf::from("/b/sengi-os/dock.toml"),
            ]
        );
    }

    #[test]
    fn the_first_readable_file_wins() {
        let dir = temp_dir("config");
        let user = dir.join("user/dock.toml");
        let system = dir.join("system/dock.toml");
        write(&system, "pinned = [\"foot\", \"web\"]\n");

        let paths = [user.clone(), system];
        assert_eq!(pinned(&paths), ["foot", "web"]);

        write(&user, "pinned = [\"mail\"]\n");
        assert_eq!(pinned(&paths), ["mail"]);

        // A broken user file falls through to the system one.
        write(&user, "pinned = oops\n");
        assert_eq!(pinned(&paths), ["foot", "web"]);
    }

    #[test]
    fn no_file_gives_the_built_in_default() {
        assert_eq!(
            pinned(&[PathBuf::from("/nonexistent/dock.toml")]),
            DEFAULT_PINNED
        );
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_saved_list_reads_back_in_order() {
        let dir = temp_dir("save");
        let path = dir.join("sengi-os/dock.toml");
        save(&path, &ids(&["web", "foot", "mail"])).unwrap();
        assert_eq!(pinned(&[path.clone()]), ["web", "foot", "mail"]);
        save(&path, &[]).unwrap();
        assert_eq!(pinned(&[path.clone()]), Vec::<String>::new());
        save(&path, &ids(&["foot"])).unwrap();
        assert_eq!(pinned(&[path]), ["foot"]);
    }

    #[test]
    fn saving_keeps_comments_and_other_keys() {
        let text = "# mine\nsize = 48\npinned = [\n  \"a\", # first\n  \"b\"\n]\nlater = true\n";
        let out = render(Some(text), &ids(&["b", "c"]));
        assert!(out.starts_with("# mine\nsize = 48\n"));
        assert!(out.ends_with("]\nlater = true\n"));
        assert_eq!(parse(&out).unwrap(), ["b", "c"]);
        // A key that only starts with the same letters is not the list.
        let out = render(Some("pinned-extra = 1\n"), &ids(&["a"]));
        assert_eq!(parse(&out).unwrap(), ["a"]);
        assert!(out.starts_with("pinned-extra = 1\n"));
    }

    #[test]
    fn saving_over_a_broken_file_keeps_a_copy() {
        let dir = temp_dir("broken");
        let path = dir.join("dock.toml");
        write(&path, "pinned = oops\n");
        save(&path, &ids(&["foot"])).unwrap();
        assert_eq!(pinned(&[path.clone()]), ["foot"]);
        let backup = std::fs::read_to_string(dir.join("dock.toml.bak")).unwrap();
        assert_eq!(backup, "pinned = oops\n");
    }

    #[test]
    fn the_user_file_is_where_xdg_says() {
        assert_eq!(
            user_path_from(Some("/home/a"), None),
            Some(PathBuf::from("/home/a/.config/sengi-os/dock.toml"))
        );
        assert_eq!(
            user_path_from(Some("/home/a"), Some("/cfg")),
            Some(PathBuf::from("/cfg/sengi-os/dock.toml"))
        );
        assert_eq!(user_path_from(None, None), None);
    }

    #[test]
    fn the_shipped_default_parses() {
        let text = include_str!("../dock.toml");
        assert_eq!(parse(text).unwrap(), DEFAULT_PINNED);
    }
}
