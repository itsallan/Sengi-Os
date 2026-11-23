//! The layout of the outputs, kept between sessions.
//!
//! It lives in `sengi-os/outputs.toml` beside the settings file, in the
//! same flat format: one section per output, named after it. The compositor
//! reads it when it starts, when a screen is plugged in and on SIGHUP, and
//! writes it a while after a program that manages outputs changed
//! something: Settings puts a change on trial for fifteen seconds and takes
//! it back unless it is kept, so what is written is what was left alone
//! for longer than that. Entries for screens that are not there are kept in
//! the file and ignored.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use sengi_config::{store, Dirs, Document, Value};
use smithay::utils::Transform;

use crate::outputs::{near, size_is_sane, SCALE_RANGE};

pub const FILE: &str = "sengi-os/outputs.toml";

/// How long a change that a program made stays unsaved. Longer than the
/// trial Settings gives a change, so that one it takes back is not saved.
pub const SAVE_AFTER: Duration = Duration::from_secs(20);

/// How long a change has to have stood for the session's end to keep it:
/// the page's trial is fifteen seconds.
pub const TRIAL_AND_ONE: Duration = Duration::from_secs(16);

/// The most entries kept, so that screens passing through cannot make the
/// file grow without end.
const MAX_ENTRIES: usize = 32;

/// What one output was last set to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StoredHead {
    pub name: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub enabled: bool,
    /// Width, height and refresh in mHz.
    pub mode: Option<(i32, i32, i32)>,
    pub position: Option<(i32, i32)>,
    /// In thousandths.
    pub scale: Option<u32>,
    pub transform: Option<Transform>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StoredLayout {
    pub heads: Vec<StoredHead>,
}

const TRANSFORMS: [(&str, Transform); 8] = [
    ("normal", Transform::Normal),
    ("90", Transform::_90),
    ("180", Transform::_180),
    ("270", Transform::_270),
    ("flipped", Transform::Flipped),
    ("flipped-90", Transform::Flipped90),
    ("flipped-180", Transform::Flipped180),
    ("flipped-270", Transform::Flipped270),
];

pub fn transform_name(transform: Transform) -> &'static str {
    TRANSFORMS
        .iter()
        .find(|(_, known)| *known == transform)
        .map_or("normal", |(name, _)| name)
}

fn transform_from(name: &str) -> Option<Transform> {
    TRANSFORMS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, transform)| *transform)
}

/// A name as a section of the file: only letters, digits, `-` and `_`.
fn section_for(name: &str) -> String {
    let plain: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    if plain.is_empty() {
        "output".into()
    } else {
        plain
    }
}

fn mode_text(mode: (i32, i32, i32)) -> String {
    format!("{}x{}@{}", mode.0, mode.1, mode.2)
}

fn parse_mode(text: &str) -> Option<(i32, i32, i32)> {
    let (size, refresh) = text.split_once('@').unwrap_or((text, "0"));
    let (w, h) = size.split_once('x')?;
    let mode = (w.parse().ok()?, h.parse().ok()?, refresh.parse().ok()?);
    (size_is_sane(mode.0, mode.1) && (0..=1_000_000).contains(&mode.2)).then_some(mode)
}

impl StoredLayout {
    /// The entries in `text`; what cannot be read is left out.
    pub fn parse(text: &str) -> Self {
        let (document, _) = Document::parse(text);
        let mut sections: Vec<&str> = Vec::new();
        for (section, _) in document.keys() {
            if !section.is_empty() && !sections.contains(&section) {
                sections.push(section);
            }
        }
        let text_of = |section: &str, key: &str| -> String {
            match document.get(section, key) {
                Some(Value::Text(text)) => text.chars().take(64).collect(),
                _ => String::new(),
            }
        };
        let number = |section: &str, key: &str| match document.get(section, key) {
            Some(Value::Number(number)) => i32::try_from(*number).ok(),
            _ => None,
        };
        let heads = sections
            .into_iter()
            .take(MAX_ENTRIES)
            .map(|section| {
                let name = match text_of(section, "name") {
                    name if name.is_empty() => section.to_string(),
                    name => name,
                };
                let enabled = match document.get(section, "enabled") {
                    Some(Value::Flag(flag)) => *flag,
                    _ => true,
                };
                let mode = match document.get(section, "mode") {
                    Some(Value::Text(text)) => parse_mode(text),
                    _ => None,
                };
                // A place far off is no place: it would only make sums overflow.
                let position = number(section, "x")
                    .zip(number(section, "y"))
                    .filter(|place| near(*place));
                let scale = number(section, "scale")
                    .filter(|t| {
                        let scale = f64::from(*t) / 1000.0;
                        (SCALE_RANGE.0..=SCALE_RANGE.1).contains(&scale)
                    })
                    .map(|t| t as u32);
                let transform = match document.get(section, "transform") {
                    Some(Value::Text(text)) => transform_from(text),
                    _ => None,
                };
                StoredHead {
                    name,
                    make: text_of(section, "make"),
                    model: text_of(section, "model"),
                    serial: text_of(section, "serial"),
                    enabled,
                    mode,
                    position,
                    scale,
                    transform,
                }
            })
            .collect();
        Self { heads }
    }

    pub fn render(&self) -> String {
        let mut document = Document::default();
        let mut used: Vec<String> = Vec::new();
        for head in &self.heads {
            let mut section = section_for(&head.name);
            let mut n = 2;
            while used.contains(&section) {
                section = format!("{}-{n}", section_for(&head.name));
                n += 1;
            }
            used.push(section.clone());
            document.set(&section, "name", Value::text(&head.name));
            for (key, value) in [
                ("make", &head.make),
                ("model", &head.model),
                ("serial", &head.serial),
            ] {
                if !value.is_empty() {
                    document.set(&section, key, Value::text(value));
                }
            }
            document.set(&section, "enabled", Value::Flag(head.enabled));
            if let Some(mode) = head.mode {
                document.set(&section, "mode", Value::text(&mode_text(mode)));
            }
            if let Some((x, y)) = head.position {
                document.set(&section, "x", Value::Number(i64::from(x)));
                document.set(&section, "y", Value::Number(i64::from(y)));
            }
            if let Some(scale) = head.scale {
                document.set(&section, "scale", Value::Number(i64::from(scale)));
            }
            if let Some(transform) = head.transform {
                document.set(
                    &section,
                    "transform",
                    Value::text(transform_name(transform)),
                );
            }
        }
        let mut out = String::from(
            "# The layout of the screens, written by sengi-compositor.\n# Section per screen; scale is in thousandths.\n",
        );
        out.push_str(&document.render());
        out
    }

    /// The entry for a screen: by make, model and serial when it has a
    /// serial, else by its name (and its model, if both know it).
    pub fn find(&self, name: &str, make: &str, model: &str, serial: &str) -> Option<&StoredHead> {
        if !serial.is_empty() {
            let by_serial = self
                .heads
                .iter()
                .find(|head| head.serial == serial && head.make == make && head.model == model);
            if by_serial.is_some() {
                return by_serial;
            }
        }
        self.heads.iter().find(|head| {
            head.name == name
                && (head.model.is_empty() || model.is_empty() || head.model == model)
                // A screen with a serial that is another one's is another.
                && (head.serial.is_empty() || serial.is_empty() || head.serial == serial)
        })
    }

    /// Puts `new` in the place of the entry for the same screen, or at the
    /// end. Entries of screens that are not there are kept; the oldest go
    /// when there are too many.
    pub fn update(&mut self, new: StoredHead) {
        let at = self.heads.iter().position(|head| {
            if !new.serial.is_empty() && !head.serial.is_empty() {
                head.serial == new.serial && head.make == new.make && head.model == new.model
            } else {
                head.name == new.name
            }
        });
        match at {
            Some(at) => self.heads[at] = new,
            None => self.heads.push(new),
        }
        let extra = self.heads.len().saturating_sub(MAX_ENTRIES);
        self.heads.drain(..extra);
    }

    pub fn path() -> Option<PathBuf> {
        Dirs::from_env().user(FILE)
    }

    pub fn load() -> Self {
        match Self::path() {
            Some(path) => Self::load_from(&path),
            None => Self::default(),
        }
    }

    pub fn load_from(path: &Path) -> Self {
        let (document, _) = store::load(path);
        Self::parse(&document.render())
    }

    /// Writes the file whole, through a temporary one so that a crash does
    /// not leave half of it.
    pub fn save(&self) -> std::io::Result<()> {
        let path =
            Self::path().ok_or_else(|| std::io::Error::other("no configuration directory"))?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let temporary = path.with_extension("toml.new");
        let mut file = fs::File::create(&temporary)?;
        file.write_all(self.render().as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(name: &str) -> StoredHead {
        StoredHead {
            name: name.into(),
            enabled: true,
            ..StoredHead::default()
        }
    }

    #[test]
    fn a_layout_survives_being_written_and_read() {
        let layout = StoredLayout {
            heads: vec![
                StoredHead {
                    make: "Dell".into(),
                    model: "U2720Q".into(),
                    serial: "ABC123".into(),
                    mode: Some((3840, 2160, 59997)),
                    position: Some((1536, -20)),
                    scale: Some(1750),
                    transform: Some(Transform::Flipped90),
                    ..head("DP-1")
                },
                StoredHead {
                    enabled: false,
                    ..head("HDMI-A-1")
                },
            ],
        };
        assert_eq!(StoredLayout::parse(&layout.render()), layout);
    }

    #[test]
    fn nonsense_is_left_out_not_trusted() {
        let layout = StoredLayout::parse(
            "[DP-1]\nmode = \"0x5\"\nscale = 90000\ntransform = \"upside\"\nx = \"left\"\n",
        );
        let [one] = &layout.heads[..] else {
            panic!("one entry")
        };
        assert_eq!(one.name, "DP-1");
        assert_eq!(one.mode, None);
        assert_eq!(one.scale, None);
        assert_eq!(one.transform, None);
        assert_eq!(one.position, None);
        assert!(one.enabled);
        assert!(StoredLayout::parse("").heads.is_empty());
        assert!(StoredLayout::parse("= = [").heads.is_empty());
    }

    #[test]
    fn a_place_at_the_end_of_the_number_range_is_not_kept() {
        let layout = StoredLayout::parse(
            "[DP-1]\nx = 2147483647\ny = -2147483648\n[DP-2]\nx = 100\ny = 5\n",
        );
        assert_eq!(layout.heads[0].position, None);
        assert_eq!(layout.heads[1].position, Some((100, 5)));
    }

    #[test]
    fn a_screen_is_found_by_serial_before_name() {
        let mut dell = head("DP-1");
        dell.make = "Dell".into();
        dell.model = "U2720Q".into();
        dell.serial = "ABC123".into();
        dell.scale = Some(1500);
        let layout = StoredLayout { heads: vec![dell] };
        // On another connector it is still that screen.
        assert!(layout.find("DP-2", "Dell", "U2720Q", "ABC123").is_some());
        // Another monitor of the same model on the same connector is not.
        assert!(layout.find("DP-1", "Dell", "U2720Q", "ZZZ").is_none());
        // Without a serial to go by, the name and model do.
        assert!(layout.find("DP-1", "Dell", "U2720Q", "").is_some());
        assert!(layout.find("DP-1", "LG", "Other", "").is_none());
    }

    #[test]
    fn an_update_replaces_its_own_entry_and_keeps_the_rest() {
        let mut layout = StoredLayout {
            heads: vec![head("DP-1"), head("DP-2")],
        };
        let mut again = head("DP-1");
        again.scale = Some(2000);
        layout.update(again);
        assert_eq!(layout.heads.len(), 2);
        assert_eq!(layout.heads[0].scale, Some(2000));
        for n in 0..40 {
            layout.update(head(&format!("X-{n}")));
        }
        assert_eq!(layout.heads.len(), MAX_ENTRIES);
    }

    #[test]
    fn names_that_are_not_plain_still_make_a_section() {
        assert_eq!(section_for("X11:1"), "X11_1");
        assert_eq!(section_for(""), "output");
        let layout = StoredLayout {
            heads: vec![head("a:b"), head("a;b")],
        };
        let again = StoredLayout::parse(&layout.render());
        assert_eq!(again.heads.len(), 2);
        assert_eq!(again.heads[1].name, "a;b");
    }

    #[test]
    fn transforms_have_names_that_come_back() {
        for (name, transform) in TRANSFORMS {
            assert_eq!(transform_name(transform), name);
            assert_eq!(transform_from(name), Some(transform));
        }
    }
}
