//! The settings file as text: `sengi-os/settings.toml`.
//!
//! The file is the flat subset of TOML that a person writes by hand:
//! `[section]` headers and `key = value` lines, where a value is a string,
//! a whole number, `true`/`false`, or a one-line list of strings. That is
//! little enough to read without a TOML library, and reading it ourselves
//! is what lets a rewrite keep everything it did not understand: the file
//! is held as its lines, a changed key replaces its own line, and comments,
//! blank lines, unknown keys and even lines that make no sense stay where
//! they were.
//!
//! What the keys mean is in `settings.rs`. Putting a changed document back
//! on disk is left to sengi-settings, the one program that writes it.

use std::fs;
use std::path::{Path, PathBuf};

/// A settings file larger than this is not one of ours.
pub const MAX_BYTES: u64 = 256 * 1024;

pub const FILE: &str = "sengi-os/settings.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Number(i64),
    Flag(bool),
    List(Vec<String>),
}

impl Value {
    pub fn text(value: &str) -> Value {
        Value::Text(value.to_string())
    }

    /// As it is written in the file.
    pub fn render(&self) -> String {
        match self {
            Value::Text(text) => quote(text),
            Value::Number(number) => number.to_string(),
            Value::Flag(flag) => flag.to_string(),
            Value::List(items) => {
                let items: Vec<String> = items.iter().map(|item| quote(item)).collect();
                format!("[{}]", items.join(", "))
            }
        }
    }

    /// As a shell script wants it: bare text, a list joined by commas.
    pub fn plain(&self) -> String {
        match self {
            Value::Text(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            Value::Flag(flag) => flag.to_string(),
            Value::List(items) => items.join(","),
        }
    }
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Reads a quoted string from the start of `text`. Returns it and what
/// follows the closing quote.
fn unquote(text: &str) -> Option<(String, &str)> {
    let mut chars = text.strip_prefix('"')?.char_indices();
    let body = &text[1..];
    let mut out = String::new();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => return Some((out, &body[index + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

/// Where a `#` comment starts in `line`, minding that `#` may appear
/// inside a string.
fn comment_start(line: &str) -> usize {
    let mut in_string = false;
    let mut escaped = false;
    for (index, c) in line.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '#' if !in_string => return index,
            _ => {}
        }
    }
    line.len()
}

fn parse_value(text: &str) -> Option<Value> {
    let text = text.trim();
    if let Some(list) = text.strip_prefix('[') {
        let mut rest = list.trim_start();
        let mut items = Vec::new();
        loop {
            if let Some(after) = rest.strip_prefix(']') {
                return after.trim().is_empty().then_some(Value::List(items));
            }
            let (item, after) = unquote(rest)?;
            items.push(item);
            rest = after.trim_start();
            if let Some(after) = rest.strip_prefix(',') {
                rest = after.trim_start();
            } else if !rest.starts_with(']') {
                return None;
            }
        }
    }
    if text.starts_with('"') {
        let (value, after) = unquote(text)?;
        return after.trim().is_empty().then_some(Value::Text(value));
    }
    match text {
        "true" => Some(Value::Flag(true)),
        "false" => Some(Value::Flag(false)),
        _ => text.parse().ok().map(Value::Number),
    }
}

fn key_is_plain(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    /// A comment, a blank line, or something that could not be read. Kept
    /// as it is.
    Other(String),
    Section {
        name: String,
        raw: String,
    },
    Entry {
        section: String,
        key: String,
        value: Value,
        /// The comment that followed the value, with its `#`.
        comment: String,
        /// The line as it was written, until the value is changed.
        raw: Option<String>,
    },
}

/// A settings file, in the order it was written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    lines: Vec<Line>,
}

impl Document {
    /// Reads `text`. Nothing is fatal: a line that cannot be read is kept
    /// as it stands and reported.
    pub fn parse(text: &str) -> (Document, Vec<String>) {
        let mut lines = Vec::new();
        let mut warnings = Vec::new();
        let mut section = String::new();
        for (number, raw) in text.lines().enumerate() {
            let number = number + 1;
            let code = raw[..comment_start(raw)].trim();
            let comment = raw[comment_start(raw)..].to_string();
            if code.is_empty() {
                lines.push(Line::Other(raw.to_string()));
                continue;
            }
            if let Some(name) = code.strip_prefix('[').and_then(|c| c.strip_suffix(']')) {
                let name = name.trim();
                if key_is_plain(name) {
                    section = name.to_string();
                    lines.push(Line::Section {
                        name: section.clone(),
                        raw: raw.to_string(),
                    });
                } else {
                    warnings.push(format!("line {number}: `{code}` is not a section name"));
                    // What follows belongs to a section we cannot name.
                    section = "\0".to_string();
                    lines.push(Line::Other(raw.to_string()));
                }
                continue;
            }
            let entry = code.split_once('=').and_then(|(key, value)| {
                let key = key.trim();
                Some((key_is_plain(key).then_some(key)?, parse_value(value)?))
            });
            match entry {
                Some((key, value)) => lines.push(Line::Entry {
                    section: section.clone(),
                    key: key.to_string(),
                    value,
                    comment,
                    raw: Some(raw.to_string()),
                }),
                None => {
                    warnings.push(format!("line {number}: cannot read `{code}`"));
                    lines.push(Line::Other(raw.to_string()));
                }
            }
        }
        (Document { lines }, warnings)
    }

    /// The last value given for `key` in `section`, as TOML would have it
    /// if it allowed a key twice.
    pub fn get(&self, section: &str, key: &str) -> Option<&Value> {
        self.lines.iter().rev().find_map(|line| match line {
            Line::Entry {
                section: s,
                key: k,
                value,
                ..
            } if s == section && k == key => Some(value),
            _ => None,
        })
    }

    /// Every `(section, key)` in the file, in order.
    pub fn keys(&self) -> impl Iterator<Item = (&str, &str)> {
        self.lines.iter().filter_map(|line| match line {
            Line::Entry { section, key, .. } => Some((section.as_str(), key.as_str())),
            _ => None,
        })
    }

    /// Give `key` in `section` a value: in place if the key is there, at
    /// the end of its section if not, in a new section if there is none.
    pub fn set(&mut self, section: &str, key: &str, new: Value) {
        let mut found = false;
        for line in self.lines.iter_mut().rev() {
            if let Line::Entry {
                section: s,
                key: k,
                value,
                raw,
                ..
            } = line
            {
                if s == section && k == key {
                    if *value != new {
                        *raw = None;
                    }
                    *value = new.clone();
                    found = true;
                    break;
                }
            }
        }
        if found {
            return;
        }
        let entry = Line::Entry {
            section: section.to_string(),
            key: key.to_string(),
            value: new,
            comment: String::new(),
            raw: None,
        };
        let header = self
            .lines
            .iter()
            .rposition(|line| matches!(line, Line::Section { name, .. } if name == section));
        match header {
            Some(header) => {
                // After the last line of that section that says anything.
                let mut at = header + 1;
                for (index, line) in self.lines.iter().enumerate().skip(header + 1) {
                    match line {
                        Line::Section { .. } => break,
                        Line::Entry { .. } => at = index + 1,
                        Line::Other(raw) if !raw.trim().is_empty() => at = index + 1,
                        Line::Other(_) => {}
                    }
                }
                self.lines.insert(at, entry);
            }
            None => {
                if self
                    .lines
                    .last()
                    .is_some_and(|line| !matches!(line, Line::Other(raw) if raw.trim().is_empty()))
                {
                    self.lines.push(Line::Other(String::new()));
                }
                self.lines.push(Line::Section {
                    name: section.to_string(),
                    raw: format!("[{section}]"),
                });
                self.lines.push(entry);
            }
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            match line {
                Line::Other(raw) | Line::Section { raw, .. } => out.push_str(raw),
                Line::Entry { raw: Some(raw), .. } => out.push_str(raw),
                Line::Entry {
                    key,
                    value,
                    comment,
                    ..
                } => {
                    out.push_str(key);
                    out.push_str(" = ");
                    out.push_str(&value.render());
                    if !comment.is_empty() {
                        out.push_str("  ");
                        out.push_str(comment);
                    }
                }
            }
            out.push('\n');
        }
        out
    }
}

/// Read `path`. A file that is missing, unreadable, too large or not
/// text is an empty document and a warning (none for a missing file).
pub fn load(path: &Path) -> (Document, Vec<String>) {
    let unreadable = |why: &str| {
        (
            Document::default(),
            vec![format!("{}: {why}", path.display())],
        )
    };
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (Document::default(), Vec::new())
        }
        Err(error) => return unreadable(&error.to_string()),
    };
    if !meta.is_file() {
        return unreadable("not a file");
    }
    if meta.len() > MAX_BYTES {
        return unreadable("too large to be a settings file");
    }
    match fs::read_to_string(path) {
        Ok(text) => {
            let (document, warnings) = Document::parse(&text);
            let warnings = warnings
                .into_iter()
                .map(|warning| format!("{}: {warning}", path.display()))
                .collect();
            (document, warnings)
        }
        Err(error) => unreadable(&error.to_string()),
    }
}

/// Where things live, worked out once from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// `$XDG_CONFIG_HOME`, or `~/.config`.
    pub config_home: Option<PathBuf>,
    /// `$XDG_CONFIG_DIRS`, most important first.
    pub config_dirs: Vec<PathBuf>,
}

impl Dirs {
    pub fn from(home: Option<&str>, config_home: Option<&str>, config_dirs: Option<&str>) -> Dirs {
        let absolute = |dir: &str| Some(PathBuf::from(dir)).filter(|path| path.is_absolute());
        let config_home = config_home
            .and_then(absolute)
            .or_else(|| home.and_then(absolute).map(|home| home.join(".config")));
        let config_dirs = config_dirs
            .filter(|dirs| !dirs.is_empty())
            .unwrap_or("/etc/xdg")
            .split(':')
            .filter_map(absolute)
            .collect();
        Dirs {
            config_home,
            config_dirs,
        }
    }

    pub fn from_env() -> Dirs {
        let var = |name: &str| std::env::var(name).ok();
        Dirs::from(
            var("HOME").as_deref(),
            var("XDG_CONFIG_HOME").as_deref(),
            var("XDG_CONFIG_DIRS").as_deref(),
        )
    }

    /// The user's own copy of `file`, whether or not it exists.
    pub fn user(&self, file: &str) -> Option<PathBuf> {
        self.config_home.as_ref().map(|dir| dir.join(file))
    }

    /// The system's copy of `file`: the first that exists.
    pub fn system(&self, file: &str) -> Option<PathBuf> {
        self.config_dirs
            .iter()
            .map(|dir| dir.join(file))
            .find(|path| path.is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn parsed(text: &str) -> Document {
        let (document, warnings) = Document::parse(text);
        assert_eq!(warnings, Vec::<String>::new());
        document
    }

    #[test]
    fn values_of_each_kind_are_read() {
        let doc = parsed(
            "top = 1\n[power]\nlock-after = 300\nlock-on-sleep = true\n\
             [clock]\nformat = \"24h\"\n[keyboard]\nlayouts = [\"us\", \"fr(azerty)\"]\nnone = []\n",
        );
        assert_eq!(doc.get("", "top"), Some(&Value::Number(1)));
        assert_eq!(doc.get("power", "lock-after"), Some(&Value::Number(300)));
        assert_eq!(doc.get("power", "lock-on-sleep"), Some(&Value::Flag(true)));
        assert_eq!(doc.get("clock", "format"), Some(&Value::text("24h")));
        assert_eq!(
            doc.get("keyboard", "layouts"),
            Some(&Value::List(vec!["us".into(), "fr(azerty)".into()]))
        );
        assert_eq!(doc.get("keyboard", "none"), Some(&Value::List(vec![])));
        assert_eq!(doc.get("power", "format"), None);
        assert_eq!(doc.get("nowhere", "lock-after"), None);
    }

    #[test]
    fn comments_and_odd_spacing_are_allowed() {
        let doc = parsed(
            "# heading\n\n  [ power ]   # the section\n   lock-after=120# seconds\n\
             name = \"a # b\"  # not a comment inside\n neg = -5\n",
        );
        assert_eq!(doc.get("power", "lock-after"), Some(&Value::Number(120)));
        assert_eq!(doc.get("power", "name"), Some(&Value::text("a # b")));
        assert_eq!(doc.get("power", "neg"), Some(&Value::Number(-5)));
        // Untouched lines go back out exactly as they came in.
        let text = "  [ power ]\n   lock-after=120# seconds\n";
        let mut doc = parsed(text);
        assert_eq!(doc.render(), text);
        doc.set("power", "lock-after", Value::Number(120));
        assert_eq!(doc.render(), text);
        doc.set("power", "lock-after", Value::Number(60));
        assert_eq!(doc.render(), "  [ power ]\nlock-after = 60  # seconds\n");
    }

    #[test]
    fn strings_escape_and_come_back() {
        let tricky = "quote \" slash \\ line\nnext\ttab # hash";
        let mut doc = Document::default();
        doc.set("s", "k", Value::text(tricky));
        let again = parsed(&doc.render());
        assert_eq!(again.get("s", "k"), Some(&Value::text(tricky)));
        // Other control characters are not written at all.
        assert_eq!(Value::text("a\u{7}b").render(), "\"ab\"");
    }

    #[test]
    fn lines_that_cannot_be_read_are_reported_and_kept() {
        let text = "[power]\nlock-after = soon\nthis is not toml\nblank-after = 60\n\
                    list = [\"open\n[bad section!]\nlost = 1\n[clock]\nformat = \"12h\"\n= 3\n\
                    s = \"unclosed\nx = \"a\" trailing\n";
        let (doc, warnings) = Document::parse(text);
        assert_eq!(warnings.len(), 7, "{warnings:?}");
        assert!(warnings[0].starts_with("line 2:"));
        assert_eq!(doc.get("power", "blank-after"), Some(&Value::Number(60)));
        assert_eq!(doc.get("power", "lock-after"), None);
        // A key under a header that could not be read belongs nowhere.
        assert_eq!(doc.get("power", "lost"), None);
        assert_eq!(doc.get("bad section!", "lost"), None);
        assert_eq!(doc.get("clock", "format"), Some(&Value::text("12h")));
        // And nothing is lost on the way back out.
        assert_eq!(doc.render(), text);
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for text in [
            "\0\0\0",
            "[",
            "]",
            "[]",
            "=",
            "a = [",
            "a = [,]",
            "a = [\"x\",,]",
            "a = \"\\",
            "a = \"\\q\"",
            "a = 99999999999999999999999",
            "é = 1",
            "a = \"é\u{202e}\"",
            &"[".repeat(10_000),
            &"a = [\"x\", ".repeat(2_000),
        ] {
            let (doc, _) = Document::parse(text);
            let _ = doc.render();
        }
    }

    #[test]
    fn the_last_of_a_repeated_key_wins() {
        let doc = parsed("[a]\nk = 1\nk = 2\n[b]\nk = 3\n[a]\nk = 4\n");
        assert_eq!(doc.get("a", "k"), Some(&Value::Number(4)));
        assert_eq!(doc.get("b", "k"), Some(&Value::Number(3)));
    }

    #[test]
    fn setting_a_key_changes_only_its_line() {
        let text = "# Mine\n[power]\n# how long\nlock-after = 300  # seconds\nfuture-key = \"kept\"\n\n[clock]\nformat = \"24h\"\n";
        let mut doc = parsed(text);
        doc.set("power", "lock-after", Value::Number(60));
        assert_eq!(
            doc.render(),
            text.replace("lock-after = 300", "lock-after = 60")
        );
    }

    #[test]
    fn a_new_key_goes_to_the_end_of_its_section() {
        let mut doc = parsed("[power]\nlock-after = 300\n# note\n\n[clock]\nformat = \"24h\"\n");
        doc.set("power", "blank-after", Value::Number(0));
        assert_eq!(
            doc.render(),
            "[power]\nlock-after = 300\n# note\nblank-after = 0\n\n[clock]\nformat = \"24h\"\n"
        );
        doc.set("mouse", "left-handed", Value::Flag(true));
        assert!(doc
            .render()
            .ends_with("format = \"24h\"\n\n[mouse]\nleft-handed = true\n"));
        let mut empty = Document::default();
        empty.set("clock", "format", Value::text("12h"));
        assert_eq!(empty.render(), "[clock]\nformat = \"12h\"\n");
    }

    #[test]
    fn a_missing_file_is_empty_and_silent() {
        let tmp = TempDir::new();
        let (doc, warnings) = load(&tmp.path().join("nope.toml"));
        assert_eq!(doc, Document::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn files_that_are_not_settings_are_refused() {
        let tmp = TempDir::new();
        std::fs::create_dir_all(tmp.path().join("dir.toml")).unwrap();
        assert_eq!(load(&tmp.path().join("dir.toml")).1.len(), 1);
        std::fs::write(tmp.path().join("bin.toml"), [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(load(&tmp.path().join("bin.toml")).1.len(), 1);
        let huge = "# padding\n".repeat(MAX_BYTES as usize / 10 + 1);
        tmp.write("huge.toml", &huge);
        let (doc, warnings) = load(&tmp.path().join("huge.toml"));
        assert_eq!(doc, Document::default());
        assert!(warnings[0].contains("too large"));
    }

    #[test]
    fn directories_follow_the_xdg_rules() {
        let dirs = Dirs::from(Some("/home/a"), None, None);
        assert_eq!(
            dirs.user(FILE).unwrap(),
            Path::new("/home/a/.config/sengi-os/settings.toml")
        );
        assert_eq!(dirs.config_dirs, [PathBuf::from("/etc/xdg")]);
        let dirs = Dirs::from(Some("/home/a"), Some("/cfg"), Some("/one:relative::/two"));
        assert_eq!(dirs.user("x").unwrap(), Path::new("/cfg/x"));
        assert_eq!(
            dirs.config_dirs,
            [PathBuf::from("/one"), PathBuf::from("/two")]
        );
        // A relative XDG_CONFIG_HOME is to be ignored, says the spec.
        let dirs = Dirs::from(Some("/home/a"), Some("cfg"), Some(""));
        assert_eq!(dirs.user("x").unwrap(), Path::new("/home/a/.config/x"));
        assert_eq!(Dirs::from(None, None, None).user("x"), None);
    }

    #[test]
    fn the_first_system_file_that_exists_is_used() {
        let tmp = TempDir::new();
        tmp.write("two/sengi-os/settings.toml", "");
        let dirs = Dirs::from(
            None,
            None,
            Some(&format!(
                "{}:{}",
                tmp.path().join("one").display(),
                tmp.path().join("two").display()
            )),
        );
        assert_eq!(
            dirs.system(FILE).unwrap(),
            tmp.path().join("two/sengi-os/settings.toml")
        );
        assert_eq!(dirs.system("other"), None);
    }
}
