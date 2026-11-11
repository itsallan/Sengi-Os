//! What the keys of `settings.toml` mean: their names, defaults and limits.
//!
//! Two files are read, the system's (`/etc/xdg/sengi-os/settings.toml`)
//! and the user's (`~/.config/sengi-os/settings.toml`). A key in the user's
//! file wins; a key in neither has the default written here. Only the
//! user's file is ever written, and only the key that changed.

use std::path::PathBuf;

use crate::store::{self, Dirs, Document, Value};

/// Idle times are whole seconds; zero means never.
pub const MAX_IDLE_SECONDS: i64 = 4 * 60 * 60;
pub const MAX_LAYOUTS: usize = 4;

/// Every key this version knows, with its default. The README lists the
/// same table for people.
pub const KEYS: &[(&str, &str, Default)] = &[
    ("power", "blank-after", Default::Number(300)),
    ("power", "lock-after", Default::Number(300)),
    ("power", "lock-on-sleep", Default::Flag(true)),
    ("clock", "format", Default::Text("24h")),
    ("clock", "time-zone", Default::Text("")),
    ("keyboard", "layouts", Default::List(&["us"])),
    ("keyboard", "switch", Default::Text("")),
    ("keyboard", "repeat-delay", Default::Number(600)),
    ("keyboard", "repeat-rate", Default::Number(25)),
    ("mouse", "speed", Default::Number(0)),
    ("mouse", "natural-scroll", Default::Flag(false)),
    ("mouse", "left-handed", Default::Flag(false)),
    ("touchpad", "speed", Default::Number(0)),
    ("touchpad", "natural-scroll", Default::Flag(false)),
    ("touchpad", "tap-to-click", Default::Flag(true)),
    ("touchpad", "disable-while-typing", Default::Flag(true)),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Default {
    Text(&'static str),
    Number(i64),
    Flag(bool),
    List(&'static [&'static str]),
}

impl Default {
    fn value(self) -> Value {
        match self {
            Default::Text(text) => Value::text(text),
            Default::Number(number) => Value::Number(number),
            Default::Flag(flag) => Value::Flag(flag),
            Default::List(items) => Value::List(items.iter().map(|s| s.to_string()).collect()),
        }
    }
}

fn default_of(section: &str, key: &str) -> Option<Value> {
    KEYS.iter()
        .find(|(s, k, _)| *s == section && *k == key)
        .map(|(_, _, default)| default.value())
}

/// A value of the right kind and within its limits, or why not.
pub fn check(section: &str, key: &str, value: &Value) -> Result<Value, String> {
    let default = default_of(section, key).ok_or("unknown key")?;
    let number = |low: i64, high: i64| match value {
        Value::Number(n) if (low..=high).contains(n) => Ok(value.clone()),
        Value::Number(_) => Err(format!("must be between {low} and {high}")),
        _ => Err("must be a whole number".to_string()),
    };
    match (section, key) {
        ("power", "blank-after" | "lock-after") => number(0, MAX_IDLE_SECONDS),
        ("keyboard", "repeat-delay") => number(100, 2000),
        ("keyboard", "repeat-rate") => number(1, 100),
        (_, "speed") => number(-100, 100),
        ("clock", "format") => match value {
            Value::Text(text) if text == "24h" || text == "12h" => Ok(value.clone()),
            _ => Err("must be \"24h\" or \"12h\"".to_string()),
        },
        ("clock", "time-zone") => match value {
            Value::Text(zone) if zone.is_empty() || zone_is_plain(zone) => Ok(value.clone()),
            _ => Err("must be a zone name such as \"Europe/Paris\", or empty".to_string()),
        },
        ("keyboard", "switch") => match value {
            Value::Text(option) if option.is_empty() || xkb_word(option) => Ok(value.clone()),
            _ => Err("must be an XKB option such as \"grp:alt_shift_toggle\"".to_string()),
        },
        ("keyboard", "layouts") => match value {
            Value::List(items)
                if !items.is_empty()
                    && items.len() <= MAX_LAYOUTS
                    && items.iter().all(|item| Layout::parse(item).is_some()) =>
            {
                Ok(value.clone())
            }
            _ => Err(format!(
                "must be a list of one to {MAX_LAYOUTS} layouts such as [\"us\", \"fr(azerty)\"]"
            )),
        },
        _ => match (&default, value) {
            (Value::Flag(_), Value::Flag(_)) => Ok(value.clone()),
            (Value::Flag(_), _) => Err("must be true or false".to_string()),
            _ => Err("has the wrong kind of value".to_string()),
        },
    }
}

/// A zone name is a path under the zone directory and goes into `TZ`, so
/// it is held to the characters real zone names use.
pub fn zone_is_plain(zone: &str) -> bool {
    zone.len() <= 64
        && !zone.starts_with('/')
        && zone.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'))
        })
}

/// A name as XKB writes them: layouts, variants, options.
pub fn xkb_word(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= 64
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
}

/// A keyboard layout with an optional variant, written `fr` or
/// `fr(azerty)` as XKB does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub layout: String,
    pub variant: String,
}

impl Layout {
    pub fn parse(text: &str) -> Option<Layout> {
        let (layout, variant) = match text.split_once('(') {
            Some((layout, rest)) => (layout, rest.strip_suffix(')')?),
            None => (text, ""),
        };
        let fine = xkb_word(layout) && (variant.is_empty() || xkb_word(variant));
        fine.then(|| Layout {
            layout: layout.to_string(),
            variant: variant.to_string(),
        })
    }

    pub fn render(&self) -> String {
        if self.variant.is_empty() {
            self.layout.clone()
        } else {
            format!("{}({})", self.layout, self.variant)
        }
    }
}

/// The two files, and the user's path for writing back.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    system: Document,
    user: Document,
    user_path: Option<PathBuf>,
}

impl Settings {
    /// Read both files. The warnings are for standard error: unreadable
    /// lines, keys this version does not know, values out of range.
    pub fn load(dirs: &Dirs) -> (Settings, Vec<String>) {
        let mut warnings = Vec::new();
        let mut read = |path: Option<PathBuf>| match path {
            Some(path) => {
                let (document, mut found) = store::load(&path);
                found.extend(
                    complaints(&document)
                        .into_iter()
                        .map(|c| format!("{}: {c}", path.display())),
                );
                warnings.append(&mut found);
                document
            }
            None => Document::default(),
        };
        let system = read(dirs.system(store::FILE));
        let user_path = dirs.user(store::FILE);
        let user = read(user_path.clone());
        (
            Settings {
                system,
                user,
                user_path,
            },
            warnings,
        )
    }

    pub fn from_documents(
        system: Document,
        user: Document,
        user_path: Option<PathBuf>,
    ) -> Settings {
        Settings {
            system,
            user,
            user_path,
        }
    }

    /// The value in effect: the user's if it is valid, else the system's
    /// if that is, else the default. `None` only for a key nobody knows.
    pub fn get(&self, section: &str, key: &str) -> Option<Value> {
        let default = default_of(section, key)?;
        let valid = |document: &Document| {
            document
                .get(section, key)
                .and_then(|value| check(section, key, value).ok())
        };
        Some(
            valid(&self.user)
                .or_else(|| valid(&self.system))
                .unwrap_or(default),
        )
    }

    pub fn number(&self, section: &str, key: &str) -> i64 {
        match self.get(section, key) {
            Some(Value::Number(number)) => number,
            _ => 0,
        }
    }

    pub fn flag(&self, section: &str, key: &str) -> bool {
        matches!(self.get(section, key), Some(Value::Flag(true)))
    }

    pub fn text(&self, section: &str, key: &str) -> String {
        match self.get(section, key) {
            Some(Value::Text(text)) => text,
            _ => String::new(),
        }
    }

    pub fn layouts(&self) -> Vec<Layout> {
        match self.get("keyboard", "layouts") {
            Some(Value::List(items)) => items.iter().filter_map(|i| Layout::parse(i)).collect(),
            _ => Vec::new(),
        }
    }

    /// Change one key in the user's file and write the file. On failure
    /// the value in memory is put back, so what is shown stays what is
    /// stored.
    pub fn set(&mut self, section: &str, key: &str, value: Value) -> Result<(), String> {
        let value = check(section, key, &value).map_err(|why| format!("{section}.{key} {why}"))?;
        if self.get(section, key).as_ref() == Some(&value) {
            return Ok(());
        }
        let path = self
            .user_path
            .clone()
            .ok_or("there is no home directory to keep settings in")?;
        let before = self.user.clone();
        self.user.set(section, key, value);
        store::write_atomic(&path, &self.user.render()).map_err(|error| {
            self.user = before;
            format!("could not write {}: {error}", path.display())
        })
    }
}

/// What is wrong with the keys of a file, for its owner to read.
fn complaints(document: &Document) -> Vec<String> {
    let mut found = Vec::new();
    for (section, key) in document.keys() {
        let Some(value) = document.get(section, key) else {
            continue;
        };
        let name = if section.is_empty() || section == "\0" {
            key.to_string()
        } else {
            format!("{section}.{key}")
        };
        let complaint = match check(section, key, value) {
            Ok(_) => continue,
            Err(why) if why == "unknown key" => format!("unknown key `{name}` kept but not used"),
            Err(why) => format!("`{name}` {why}; using the default"),
        };
        if !found.contains(&complaint) {
            found.push(complaint);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn doc(text: &str) -> Document {
        Document::parse(text).0
    }

    fn settings(system: &str, user: &str) -> Settings {
        Settings::from_documents(doc(system), doc(user), None)
    }

    #[test]
    fn every_key_has_a_default_that_passes_its_own_check() {
        let empty = settings("", "");
        for (section, key, default) in KEYS {
            let value = default.value();
            assert_eq!(
                check(section, key, &value),
                Ok(value.clone()),
                "{section}.{key}"
            );
            assert_eq!(empty.get(section, key), Some(value));
        }
        assert_eq!(empty.get("power", "nonsense"), None);
    }

    #[test]
    fn the_user_file_overrides_the_system_file() {
        let s = settings(
            "[power]\nlock-after = 600\nblank-after = 120\n",
            "[power]\nlock-after = 60\n",
        );
        assert_eq!(s.number("power", "lock-after"), 60);
        assert_eq!(s.number("power", "blank-after"), 120);
        assert!(s.flag("power", "lock-on-sleep"));
    }

    #[test]
    fn a_bad_value_falls_through_to_the_next_layer() {
        let s = settings(
            "[power]\nlock-after = 600\n[clock]\nformat = \"noon\"\n",
            "[power]\nlock-after = -4\nblank-after = \"never\"\n",
        );
        assert_eq!(s.number("power", "lock-after"), 600);
        assert_eq!(s.number("power", "blank-after"), 300);
        assert_eq!(s.text("clock", "format"), "24h");
    }

    #[test]
    fn limits() {
        let bad = |section, key, value| check(section, key, &value).is_err();
        assert!(bad(
            "power",
            "lock-after",
            Value::Number(MAX_IDLE_SECONDS + 1)
        ));
        assert!(!bad("power", "lock-after", Value::Number(0)));
        assert!(bad("keyboard", "repeat-rate", Value::Number(0)));
        assert!(bad("mouse", "speed", Value::Number(101)));
        assert!(bad("mouse", "left-handed", Value::Number(1)));
        assert!(bad("clock", "time-zone", Value::text("../../etc/passwd")));
        assert!(bad("clock", "time-zone", Value::text("/etc/localtime")));
        assert!(bad(
            "clock",
            "time-zone",
            Value::text("Europe/Paris\nLD_PRELOAD=x")
        ));
        assert!(!bad(
            "clock",
            "time-zone",
            Value::text("America/Argentina/Buenos_Aires")
        ));
        assert!(!bad("clock", "time-zone", Value::text("Etc/GMT+5")));
        assert!(bad("keyboard", "switch", Value::text("grp:x,evil=1")));
        assert!(bad("keyboard", "layouts", Value::List(vec![])));
        assert!(bad(
            "keyboard",
            "layouts",
            Value::List(vec!["us".into(); 5])
        ));
        assert!(bad(
            "keyboard",
            "layouts",
            Value::List(vec!["us\nX=1".into()])
        ));
    }

    #[test]
    fn layouts_are_written_the_xkb_way() {
        let fr = Layout::parse("fr(azerty)").unwrap();
        assert_eq!((fr.layout.as_str(), fr.variant.as_str()), ("fr", "azerty"));
        assert_eq!(fr.render(), "fr(azerty)");
        assert_eq!(Layout::parse("us").unwrap().render(), "us");
        for bad in ["", "fr(", "fr()x", "fr(a b)", "(x)", "us,fr", "a$b"] {
            assert_eq!(Layout::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn unknown_keys_and_bad_values_are_reported_once() {
        let found = complaints(&doc(
            "stray = 1\n[power]\nfuture = true\nlock-after = \"x\"\nlock-after = \"y\"\n[newer]\nk = 1\n",
        ));
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(found[0].contains("unknown key `stray`"));
        assert!(found[1].contains("unknown key `power.future`"));
        assert!(found[2].contains("`power.lock-after` must be a whole number"));
    }

    #[test]
    fn a_change_is_written_and_unknown_keys_survive_it() {
        let tmp = TempDir::new();
        tmp.write("etc/sengi-os/settings.toml", "[power]\nlock-after = 600\n");
        tmp.write(
            "home/sengi-os/settings.toml",
            "# mine\n[power]\nfuture = \"keep me\"\n",
        );
        let dirs = Dirs::from(
            None,
            Some(&tmp.path().join("home").display().to_string()),
            Some(&tmp.path().join("etc").display().to_string()),
        );
        let (mut s, warnings) = Settings::load(&dirs);
        assert_eq!(warnings.len(), 1);
        assert_eq!(s.number("power", "lock-after"), 600);
        s.set("power", "lock-after", Value::Number(120)).unwrap();
        s.set("clock", "format", Value::text("12h")).unwrap();
        let written =
            std::fs::read_to_string(tmp.path().join("home/sengi-os/settings.toml")).unwrap();
        assert_eq!(
            written,
            "# mine\n[power]\nfuture = \"keep me\"\nlock-after = 120\n\n[clock]\nformat = \"12h\"\n"
        );
        // The system file is never touched.
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("etc/sengi-os/settings.toml")).unwrap(),
            "[power]\nlock-after = 600\n"
        );
        let (again, _) = Settings::load(&dirs);
        assert_eq!(again.number("power", "lock-after"), 120);
        assert_eq!(again.text("clock", "format"), "12h");
    }

    #[test]
    fn a_refused_or_failed_change_leaves_things_as_they_were() {
        let tmp = TempDir::new();
        let dirs = Dirs::from(
            None,
            Some(&tmp.path().display().to_string()),
            Some("/nonexistent"),
        );
        let (mut s, _) = Settings::load(&dirs);
        assert!(s.set("power", "lock-after", Value::Number(-1)).is_err());
        assert!(s.set("power", "nonsense", Value::Number(1)).is_err());
        assert!(!tmp.path().join("sengi-os").exists());
        // Setting the value already in effect writes nothing.
        s.set("power", "lock-after", Value::Number(300)).unwrap();
        assert!(!tmp.path().join("sengi-os").exists());
        // A directory where the file should be: the write fails.
        std::fs::create_dir_all(tmp.path().join("sengi-os/settings.toml")).unwrap();
        assert!(s.set("power", "lock-after", Value::Number(60)).is_err());
        assert_eq!(s.number("power", "lock-after"), 300);
        // Nowhere to write at all.
        let mut homeless = settings("", "");
        assert!(homeless
            .set("power", "lock-after", Value::Number(60))
            .is_err());
        assert_eq!(homeless.number("power", "lock-after"), 300);
    }
}
