//! What the keys of `settings.toml` mean: their names, defaults and limits.
//!
//! Two files are read, the system's (`/etc/xdg/sengi-os/settings.toml`)
//! and the user's (`~/.config/sengi-os/settings.toml`). A key in the user's
//! file wins; a key in neither has the default written here. Only the
//! user's file is ever written, and only the key that changed; the writing
//! is sengi-settings' own.

use std::path::{Path, PathBuf};

use crate::store::{self, Dirs, Document, Value};

/// Idle times are whole seconds; zero means never.
pub const MAX_IDLE_SECONDS: i64 = 4 * 60 * 60;
pub const MAX_LAYOUTS: usize = 4;

/// The value of `appearance.wallpaper` that means the plain desktop colour.
pub const WALLPAPER_SOLID: &str = "solid";
/// The values of `appearance.theme`.
pub const THEME_DARK: &str = "dark";
pub const THEME_LIGHT: &str = "light";
/// THE LIGHT THEME SWITCH. The light theme is not offered yet: a saved
/// `appearance.theme = "light"` reads as dark and no window offers it.
/// The palette and its tests stay. Set this to `true` to bring it back.
pub const LIGHT_THEME_ENABLED: bool = false;
pub const THEME_HIGH_CONTRAST: &str = "high-contrast";
/// The values of `appearance.text-size` with the percent each scales the
/// type by. The same steps are in `[text-scale]` of the tokens.
pub const TEXT_SIZES: [(&str, u32); 4] = [
    ("small", 90),
    ("default", 100),
    ("large", 120),
    ("extra-large", 140),
];
/// The values of `appearance.cursor-size` with the pointer size in pixels.
/// The same sizes are in `[cursor]` of the tokens.
pub const CURSOR_SIZES: [(&str, u32); 3] = [("normal", 24), ("large", 36), ("extra-large", 48)];
/// The values of `touchpad.click-method` and `touchpad.scroll-method`.
pub const CLICK_AREAS: &str = "areas";
pub const CLICK_CLICKFINGER: &str = "clickfinger";
pub const SCROLL_TWO_FINGER: &str = "two-finger";
pub const SCROLL_EDGE: &str = "edge";
/// Longest wallpaper path kept.
pub const MAX_WALLPAPER_PATH: usize = 1024;

/// Every key this version knows, with its default. The README lists the
/// same table for people.
pub const KEYS: &[(&str, &str, Default)] = &[
    ("power", "dim-after", Default::Number(120)),
    ("power", "blank-after", Default::Number(300)),
    ("power", "lock-after", Default::Number(0)),
    ("power", "lock-on-sleep", Default::Flag(true)),
    ("power", "lid", Default::Text("sleep")),
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
    ("touchpad", "click-method", Default::Text(CLICK_AREAS)),
    (
        "touchpad",
        "scroll-method",
        Default::Text(SCROLL_TWO_FINGER),
    ),
    ("touchpad", "gestures", Default::Flag(true)),
    ("display", "primary", Default::Text("")),
    ("display", "night-light", Default::Flag(false)),
    ("display", "night-from", Default::Text("21:00")),
    ("display", "night-to", Default::Text("07:00")),
    ("display", "night-strength", Default::Number(50)),
    ("appearance", "window-shadows", Default::Flag(true)),
    ("appearance", "theme", Default::Text(THEME_DARK)),
    ("appearance", "text-size", Default::Text("default")),
    ("appearance", "reduce-motion", Default::Flag(false)),
    ("appearance", "cursor-size", Default::Text("normal")),
    ("appearance", "wallpaper", Default::Text(WALLPAPER_SOLID)),
    ("appearance", "week-start", Default::Text("monday")),
    ("clipboard", "history", Default::Flag(false)),
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
        ("power", "dim-after" | "blank-after" | "lock-after") => number(0, MAX_IDLE_SECONDS),
        ("power", "lid") => match value {
            Value::Text(text) if text == "sleep" || text == "nothing" => Ok(value.clone()),
            _ => Err("must be \"sleep\" or \"nothing\"".to_string()),
        },
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
        ("touchpad", "click-method") => match value {
            Value::Text(text) if text == CLICK_AREAS || text == CLICK_CLICKFINGER => {
                Ok(value.clone())
            }
            _ => Err("must be \"areas\" or \"clickfinger\"".to_string()),
        },
        ("touchpad", "scroll-method") => match value {
            Value::Text(text) if text == SCROLL_TWO_FINGER || text == SCROLL_EDGE => {
                Ok(value.clone())
            }
            _ => Err("must be \"two-finger\" or \"edge\"".to_string()),
        },
        ("display", "night-from" | "night-to") => match value {
            Value::Text(text) if parse_clock(text).is_some() => Ok(value.clone()),
            _ => Err("must be a time such as \"21:00\"".to_string()),
        },
        ("display", "night-strength") => number(0, 100),
        ("display", "primary") => match value {
            Value::Text(name) if name.len() <= 64 && !name.chars().any(char::is_control) => {
                Ok(value.clone())
            }
            _ => Err("must be an output name such as \"eDP-1\", or empty".to_string()),
        },
        ("appearance", "theme") => match value {
            Value::Text(theme)
                if theme == THEME_DARK || theme == THEME_LIGHT || theme == THEME_HIGH_CONTRAST =>
            {
                Ok(value.clone())
            }
            _ if LIGHT_THEME_ENABLED => {
                Err("must be \"dark\", \"light\" or \"high-contrast\"".to_string())
            }
            _ => Err("must be \"dark\" or \"high-contrast\"".to_string()),
        },
        ("appearance", "text-size") => match value {
            Value::Text(name) if TEXT_SIZES.iter().any(|(known, _)| known == name) => {
                Ok(value.clone())
            }
            _ => Err("must be \"small\", \"default\", \"large\" or \"extra-large\"".to_string()),
        },
        ("appearance", "cursor-size") => match value {
            Value::Text(name) if CURSOR_SIZES.iter().any(|(known, _)| known == name) => {
                Ok(value.clone())
            }
            _ => Err("must be \"normal\", \"large\" or \"extra-large\"".to_string()),
        },
        ("appearance", "week-start") => match value {
            Value::Text(day) if matches!(day.as_str(), "monday" | "sunday" | "saturday") => {
                Ok(value.clone())
            }
            _ => Err("must be \"monday\", \"sunday\" or \"saturday\"".to_string()),
        },
        ("appearance", "wallpaper") => match value {
            Value::Text(path) if wallpaper_is_plain(path) => Ok(value.clone()),
            _ => Err("must be \"solid\" or the full path of a picture".to_string()),
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

/// `HH:MM` on a 24-hour clock, as minutes after midnight.
pub fn parse_clock(text: &str) -> Option<u32> {
    let (hours, minutes) = text.split_once(':')?;
    let digits = |part: &str| part.len() == 2 && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(hours) || !digits(minutes) {
        return None;
    }
    let (hours, minutes): (u32, u32) = (hours.parse().ok()?, minutes.parse().ok()?);
    (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

/// A wallpaper is `solid` or an absolute path without control characters:
/// it is handed to a program on a command line.
pub fn wallpaper_is_plain(value: &str) -> bool {
    value == WALLPAPER_SOLID
        || (value.starts_with('/')
            && value.len() <= MAX_WALLPAPER_PATH
            && !value.chars().any(char::is_control))
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

/// Where tzdata keeps the zones.
pub const ZONE_DIR: &str = "/usr/share/zoneinfo";

/// The file of `zone` under `zones`, if the name is a zone's and there is
/// such a zone. A name that passes [`zone_is_plain`] cannot lead out of
/// the directory.
pub fn zone_file(zones: &Path, zone: &str) -> Option<PathBuf> {
    if !zone_is_plain(zone) {
        return None;
    }
    Some(zones.join(zone)).filter(|path| path.is_file())
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
        let value = valid(&self.user)
            .or_else(|| valid(&self.system))
            .unwrap_or(default);
        // The file keeps the word, so it counts again once the theme does.
        let hidden = !LIGHT_THEME_ENABLED
            && (section, key) == ("appearance", "theme")
            && value == Value::text(THEME_LIGHT);
        Some(if hidden {
            Value::text(THEME_DARK)
        } else {
            value
        })
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

    /// Whether the light theme is chosen, never while
    /// [`LIGHT_THEME_ENABLED`] is off. The user's file decides, then the
    /// system's, which is all a program that runs as another user, such as
    /// the login screen, can see.
    pub fn light(&self) -> bool {
        self.text("appearance", "theme") == THEME_LIGHT
    }

    /// Whether the high contrast theme is chosen. It is a palette of its
    /// own: `light` is false then.
    pub fn high_contrast(&self) -> bool {
        self.text("appearance", "theme") == THEME_HIGH_CONTRAST
    }

    /// The type size as a percent of the design size: 90, 100, 120 or 140.
    /// A name nobody knows is the default size.
    pub fn text_percent(&self) -> u32 {
        let name = self.text("appearance", "text-size");
        TEXT_SIZES
            .iter()
            .find(|(known, _)| *known == name)
            .map_or(100, |(_, percent)| *percent)
    }

    /// Whether animations are turned off.
    pub fn reduce_motion(&self) -> bool {
        self.flag("appearance", "reduce-motion")
    }

    /// The pointer's size in pixels, 24, 36 or 48.
    pub fn cursor_px(&self) -> u32 {
        let name = self.text("appearance", "cursor-size");
        CURSOR_SIZES
            .iter()
            .find(|(known, _)| *known == name)
            .map_or(24, |(_, px)| *px)
    }

    pub fn layouts(&self) -> Vec<Layout> {
        match self.get("keyboard", "layouts") {
            Some(Value::List(items)) => items.iter().filter_map(|i| Layout::parse(i)).collect(),
            _ => Vec::new(),
        }
    }

    /// The user's file as it was read, for whoever writes it back.
    pub fn user(&self) -> &Document {
        &self.user
    }

    pub fn user_mut(&mut self) -> &mut Document {
        &mut self.user
    }

    /// Where the user's file is, if there is a home to keep it in.
    pub fn user_path(&self) -> Option<&Path> {
        self.user_path.as_deref()
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

    #[test]
    fn a_zone_has_to_be_a_file_under_the_zone_directory() {
        let tmp = TempDir::new();
        tmp.write("Europe/Paris", "TZif");
        tmp.write("UTC", "TZif");
        let file = |zone| zone_file(tmp.path(), zone);
        assert_eq!(file("Europe/Paris"), Some(tmp.path().join("Europe/Paris")));
        assert_eq!(file("UTC"), Some(tmp.path().join("UTC")));
        // A directory, a zone that is not there, and names that are not
        // zone names at all.
        for bad in [
            "Europe",
            "Europe/Atlantis",
            "",
            "/etc/localtime",
            ":Europe/Paris",
            "Europe/../UTC",
            "../UTC",
            "Europe//Paris",
            "Europe/Paris\n",
            "Europe/Par is",
        ] {
            assert_eq!(file(bad), None, "{bad:?}");
        }
    }

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
    fn the_screen_dims_then_goes_off_and_only_sleep_locks_unless_asked() {
        let s = settings("", "");
        assert_eq!(s.number("power", "dim-after"), 120);
        assert_eq!(s.number("power", "blank-after"), 300);
        assert_eq!(s.number("power", "lock-after"), 0);
        assert!(s.flag("power", "lock-on-sleep"));
        assert_eq!(s.text("power", "lid"), "sleep");
    }

    #[test]
    fn the_lid_is_sleep_or_nothing() {
        let bad = |value| check("power", "lid", &value).is_err();
        assert!(!bad(Value::text("sleep")));
        assert!(!bad(Value::text("nothing")));
        assert!(bad(Value::text("poweroff")));
        assert!(bad(Value::Number(1)));
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
        assert!(!bad("appearance", "wallpaper", Value::text("solid")));
        assert!(!bad(
            "appearance",
            "wallpaper",
            Value::text("/home/a b/Pictures/x.png")
        ));
        assert!(bad(
            "appearance",
            "wallpaper",
            Value::text("Pictures/x.png")
        ));
        assert!(bad("appearance", "wallpaper", Value::text("")));
        assert!(bad("appearance", "wallpaper", Value::text("/x\ny")));
        assert!(bad("appearance", "wallpaper", Value::Flag(true)));
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
    fn the_display_and_touchpad_keys_are_checked() {
        let bad = |section, key, value| check(section, key, &value).is_err();
        assert!(!bad("display", "night-from", Value::text("21:00")));
        assert!(bad("display", "night-from", Value::text("24:00")));
        assert!(bad("display", "night-to", Value::text("7:00")));
        assert!(bad("display", "night-strength", Value::Number(101)));
        assert!(bad("display", "primary", Value::text("a\nb")));
        assert!(bad("touchpad", "click-method", Value::text("tap")));
        assert!(!bad("touchpad", "scroll-method", Value::text("edge")));
        assert_eq!(parse_clock("07:30"), Some(450));
        assert_eq!(parse_clock("7:30"), None);
    }

    #[test]
    fn a_clickpad_has_its_buttons_in_the_corners_unless_told_otherwise() {
        let method = |system, user| settings(system, user).text("touchpad", "click-method");
        assert_eq!(method("", ""), CLICK_AREAS);
        assert!(settings("", "").flag("touchpad", "tap-to-click"));
        let chosen = "[touchpad]\nclick-method = \"clickfinger\"\n";
        assert_eq!(method("", chosen), CLICK_CLICKFINGER);
        assert_eq!(method(chosen, ""), CLICK_CLICKFINGER);
        assert_eq!(
            method(chosen, "[touchpad]\nclick-method = \"areas\"\n"),
            CLICK_AREAS
        );
    }

    #[test]
    fn the_theme_is_dark_unless_light_is_chosen() {
        assert!(!settings("", "").light());
        let on = LIGHT_THEME_ENABLED;
        let saved = settings("", "[appearance]\ntheme = \"light\"\n");
        assert_eq!(saved.light(), on);
        // The login screen cannot read the user's file: the system's counts.
        let system = settings("[appearance]\ntheme = \"light\"\n", "");
        assert_eq!(system.light(), on);
        assert!(!settings(
            "[appearance]\ntheme = \"light\"\n",
            "[appearance]\ntheme = \"dark\"\n"
        )
        .light());
        // A value nobody knows is no theme.
        assert!(!settings("", "[appearance]\ntheme = \"blue\"\n").light());
        assert!(check("appearance", "theme", &Value::text("blue")).is_err());
        assert!(check("appearance", "theme", &Value::Flag(true)).is_err());
    }

    #[test]
    fn a_saved_light_theme_reads_as_dark_while_it_is_not_offered() {
        let saved = settings("", "[appearance]\ntheme = \"light\"\n");
        let shown = if LIGHT_THEME_ENABLED {
            THEME_LIGHT
        } else {
            THEME_DARK
        };
        assert_eq!(saved.text("appearance", "theme"), shown);
        assert!(!saved.high_contrast());
        // The word is still accepted, so an old file draws no complaint.
        assert!(check("appearance", "theme", &Value::text("light")).is_ok());
    }

    #[test]
    fn high_contrast_is_a_palette_of_its_own() {
        let hc = settings("", "[appearance]\ntheme = \"high-contrast\"\n");
        assert!(hc.high_contrast());
        assert!(!hc.light());
        assert!(!settings("", "").high_contrast());
        assert!(!settings("", "[appearance]\ntheme = \"light\"\n").high_contrast());
        assert!(check("appearance", "theme", &Value::text("high-contrast")).is_ok());
    }

    #[test]
    fn text_size_maps_to_a_percent_and_round_trips() {
        assert_eq!(settings("", "").text_percent(), 100);
        for (name, percent) in TEXT_SIZES {
            let user = format!("[appearance]\ntext-size = \"{name}\"\n");
            assert_eq!(settings("", &user).text_percent(), percent, "{name}");
            assert!(check("appearance", "text-size", &Value::text(name)).is_ok());
        }
        // The system's file counts when the user has not chosen.
        let system = "[appearance]\ntext-size = \"large\"\n";
        assert_eq!(settings(system, "").text_percent(), 120);
        assert!(check("appearance", "text-size", &Value::text("huge")).is_err());
        assert!(check("appearance", "text-size", &Value::Number(2)).is_err());
        assert_eq!(
            settings("", "[appearance]\ntext-size = \"huge\"\n").text_percent(),
            100
        );
    }

    #[test]
    fn cursor_size_maps_to_pixels() {
        assert_eq!(settings("", "").cursor_px(), 24);
        for (name, px) in CURSOR_SIZES {
            let user = format!("[appearance]\ncursor-size = \"{name}\"\n");
            assert_eq!(settings("", &user).cursor_px(), px, "{name}");
        }
        assert!(check("appearance", "cursor-size", &Value::text("giant")).is_err());
    }

    #[test]
    fn reduced_motion_is_a_flag_that_is_off_by_default() {
        assert!(!settings("", "").reduce_motion());
        assert!(settings("", "[appearance]\nreduce-motion = true\n").reduce_motion());
        assert!(check("appearance", "reduce-motion", &Value::Flag(true)).is_ok());
        assert!(check("appearance", "reduce-motion", &Value::text("yes")).is_err());
    }

    /// The numbers of `[section]` in the tokens file, in order.
    fn token_numbers(section: &str) -> Vec<(String, u32)> {
        let source = include_str!("../../../tokens/sengi.toml");
        let mut inside = false;
        let mut found = Vec::new();
        for line in source.lines().map(str::trim) {
            if let Some(name) = line.strip_prefix('[') {
                inside = name.strip_suffix(']') == Some(section);
            } else if inside && !line.starts_with('#') {
                if let Some((key, value)) = line.split_once('=') {
                    found.push((key.trim().to_string(), value.trim().parse().unwrap()));
                }
            }
        }
        found
    }

    #[test]
    fn the_steps_here_are_the_ones_in_the_tokens() {
        let own = |steps: &[(&str, u32)]| -> Vec<(String, u32)> {
            steps.iter().map(|(k, v)| (k.to_string(), *v)).collect()
        };
        assert_eq!(token_numbers("text-scale"), own(&TEXT_SIZES));
        assert_eq!(token_numbers("cursor"), own(&CURSOR_SIZES));
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
}
