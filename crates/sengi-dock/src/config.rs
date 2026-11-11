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
    let body: String = text
        .lines()
        .map(strip_comment)
        .collect::<Vec<_>>()
        .join("\n");

    let mut rest = body.trim_start();
    let mut pinned = None;
    while !rest.is_empty() {
        let (key, after) = rest
            .split_once('=')
            .ok_or_else(|| format!("expected `key = value` at `{}`", first_line(rest)))?;
        let key = key.trim();
        let after = after.trim_start();
        if key != "pinned" {
            return Err(format!("unknown key `{key}`"));
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
    pinned.ok_or_else(|| "no `pinned` list".to_string())
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
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        match parse(&text) {
            Ok(pinned) => return pinned,
            Err(error) => eprintln!("sengi-dock: {}: {error}", path.display()),
        }
    }
    DEFAULT_PINNED.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::testing::{temp_dir, write};

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

    #[test]
    fn the_shipped_default_parses() {
        let text = include_str!("../dock.toml");
        assert_eq!(parse(text).unwrap(), DEFAULT_PINNED);
    }
}
