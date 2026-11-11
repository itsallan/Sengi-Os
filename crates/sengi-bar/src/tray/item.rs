//! What a tray item says about itself, and which icon that adds up to.

use zbus::zvariant::ObjectPath;

use super::pixmap::{self, Pixels, Pixmap};
use super::text;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// Nothing to show; the item is hidden.
    Passive,
    #[default]
    Active,
    NeedsAttention,
}

impl Status {
    /// Anything we do not recognise is shown rather than hidden.
    pub fn parse(status: &str) -> Status {
        match status {
            "Passive" => Status::Passive,
            "NeedsAttention" => Status::NeedsAttention,
            _ => Status::Active,
        }
    }
}

/// An item's properties exactly as they were read off the bus. None is a
/// property the item does not have or that could not be read.
#[derive(Clone, Debug, Default)]
pub struct Raw {
    pub id: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub icon_name: Option<String>,
    pub icon_theme_path: Option<String>,
    pub icon_pixmaps: Option<Vec<Pixmap>>,
    pub attention_icon_name: Option<String>,
    pub attention_pixmaps: Option<Vec<Pixmap>>,
    pub overlay_icon_name: Option<String>,
    /// The title and the body of the tooltip.
    pub tooltip: Option<(String, String)>,
    pub is_menu: Option<bool>,
    pub menu: Option<String>,
}

impl Raw {
    /// Whether the item answered anything at all.
    pub fn is_empty(&self) -> bool {
        self.id.is_none()
            && self.title.is_none()
            && self.status.is_none()
            && self.icon_name.is_none()
            && self.icon_pixmaps.is_none()
            && self.menu.is_none()
    }
}

/// The same, made safe to show and to act on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Properties {
    pub id: String,
    pub title: String,
    pub status: Status,
    pub icon_name: String,
    pub icon_theme_path: String,
    pub icon_pixmaps: Vec<Pixmap>,
    pub attention_icon_name: String,
    pub attention_pixmaps: Vec<Pixmap>,
    pub overlay_icon_name: String,
    pub tooltip: String,
    /// The item has no window to raise: a click opens its menu.
    pub is_menu: bool,
    /// Object path of the item's `com.canonical.dbusmenu`, if it has one.
    pub menu: Option<String>,
}

impl From<Raw> for Properties {
    fn from(raw: Raw) -> Self {
        let line = |value: Option<String>| text::clean(&value.unwrap_or_default(), text::MAX_TEXT);
        let tooltip = raw
            .tooltip
            .map(|(title, body)| if title.trim().is_empty() { body } else { title });
        Properties {
            id: line(raw.id),
            title: line(raw.title),
            status: raw.status.as_deref().map(Status::parse).unwrap_or_default(),
            // Names and paths are looked up, not drawn; icons.rs vets them.
            icon_name: raw.icon_name.unwrap_or_default(),
            icon_theme_path: raw.icon_theme_path.unwrap_or_default(),
            icon_pixmaps: raw.icon_pixmaps.unwrap_or_default(),
            attention_icon_name: raw.attention_icon_name.unwrap_or_default(),
            attention_pixmaps: raw.attention_pixmaps.unwrap_or_default(),
            overlay_icon_name: raw.overlay_icon_name.unwrap_or_default(),
            // Tooltips may carry a little markup; it is not interpreted.
            tooltip: line(tooltip.map(|text| strip_markup(&text))),
            is_menu: raw.is_menu.unwrap_or(false),
            menu: raw.menu.filter(|path| {
                // KDE's way of saying there is no menu.
                path != "/NO_DBUSMENU" && path != "/" && ObjectPath::try_from(path.as_str()).is_ok()
            }),
        }
    }
}

/// Removes `<b>`-style tags. What is left is still cleaned as plain text.
fn strip_markup(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push(' ');
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

impl Properties {
    /// What to call the item: its tooltip, else its title, else its id.
    pub fn label(&self) -> &str {
        [&self.tooltip, &self.title, &self.id]
            .into_iter()
            .find(|text| !text.is_empty())
            .map_or("", String::as_str)
    }

    /// The letter for the tile shown when there is no icon.
    pub fn letter(&self) -> String {
        let name = if self.title.is_empty() {
            &self.id
        } else {
            &self.title
        };
        text::initial(name)
    }

    /// Drops the raw pixmaps once the icon has been made from them.
    pub fn without_pixmaps(mut self) -> Self {
        self.icon_pixmaps = Vec::new();
        self.attention_pixmaps = Vec::new();
        self
    }
}

/// Decodes icon files by name. The real one runs a child process; tests
/// use a table.
pub trait IconFiles {
    fn named(&self, name: &str, theme_path: &str, pixels: u32) -> Option<Pixels>;
}

/// The icon to draw for an item, `pixels` per side. Pixmaps are preferred
/// to names, an item that needs attention shows its attention icon if it
/// has one, and the overlay icon is drawn over the bottom right corner.
pub fn icon(properties: &Properties, pixels: u32, files: &dyn IconFiles) -> Option<Pixels> {
    let pick = |pixmaps: &[Pixmap], name: &str| {
        pixmap::render(pixmaps, pixels).or_else(|| {
            (!name.is_empty())
                .then(|| files.named(name, &properties.icon_theme_path, pixels))
                .flatten()
        })
    };
    let attention = (properties.status == Status::NeedsAttention)
        .then(|| {
            pick(
                &properties.attention_pixmaps,
                &properties.attention_icon_name,
            )
        })
        .flatten();
    let mut icon = attention.or_else(|| pick(&properties.icon_pixmaps, &properties.icon_name))?;

    if !properties.overlay_icon_name.is_empty() {
        let badge = files.named(
            &properties.overlay_icon_name,
            &properties.icon_theme_path,
            pixels / 2,
        );
        if let Some(badge) = badge {
            pixmap::overlay(&mut icon, &badge);
        }
    }
    Some(icon)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Icons by name: a solid square whose red channel is the name's length.
    struct Named(&'static [&'static str]);

    impl IconFiles for Named {
        fn named(&self, name: &str, _theme_path: &str, pixels: u32) -> Option<Pixels> {
            self.0.contains(&name).then(|| Pixels {
                size: pixels,
                data: [name.len() as u8, 0, 0, 255].repeat((pixels * pixels) as usize),
            })
        }
    }

    fn solid(side: i32, argb: [u8; 4]) -> Pixmap {
        Pixmap {
            width: side,
            height: side,
            data: argb.repeat((side * side) as usize),
        }
    }

    fn some(text: &str) -> Option<String> {
        Some(text.to_string())
    }

    #[test]
    fn statuses() {
        assert_eq!(Status::parse("Passive"), Status::Passive);
        assert_eq!(Status::parse("Active"), Status::Active);
        assert_eq!(Status::parse("NeedsAttention"), Status::NeedsAttention);
        assert_eq!(Status::parse("passive"), Status::Active);
        assert_eq!(Status::parse(""), Status::Active);
        // An item that never says is shown.
        assert_eq!(Properties::from(Raw::default()).status, Status::Active);
    }

    #[test]
    fn text_is_cleaned_on_the_way_in() {
        let properties = Properties::from(Raw {
            id: some("chat\u{0}client"),
            title: some("Chat\n\u{202e}evil\t\u{7}"),
            tooltip: Some(("".into(), "<b>3 unread</b><br/>from Sam".into())),
            ..Raw::default()
        });
        assert_eq!(properties.id, "chat client");
        assert_eq!(properties.title, "Chat evil");
        assert_eq!(properties.tooltip, "3 unread from Sam");

        let long = Properties::from(Raw {
            title: Some("t".repeat(100_000)),
            ..Raw::default()
        });
        assert_eq!(long.title.chars().count(), text::MAX_TEXT);
    }

    #[test]
    fn the_label_falls_back_from_tooltip_to_title_to_id() {
        let mut raw = Raw {
            id: some("nm-applet"),
            title: some("Network"),
            tooltip: Some(("Wired connection".into(), "ignored body".into())),
            ..Raw::default()
        };
        assert_eq!(Properties::from(raw.clone()).label(), "Wired connection");
        raw.tooltip = Some((" ".into(), "".into()));
        assert_eq!(Properties::from(raw.clone()).label(), "Network");
        raw.title = None;
        let properties = Properties::from(raw);
        assert_eq!(properties.label(), "nm-applet");
        assert_eq!(properties.letter(), "N");
        assert_eq!(Properties::from(Raw::default()).label(), "");
        assert_eq!(Properties::from(Raw::default()).letter(), "?");
    }

    #[test]
    fn only_a_real_object_path_is_a_menu() {
        let menu = |path: &str| {
            Properties::from(Raw {
                menu: some(path),
                ..Raw::default()
            })
            .menu
        };
        assert_eq!(menu("/MenuBar"), some("/MenuBar"));
        assert_eq!(menu("/NO_DBUSMENU"), None);
        assert_eq!(menu("/"), None);
        assert_eq!(menu(""), None);
        assert_eq!(menu("not/a/path"), None);
        assert_eq!(menu("/bad path'"), None);
    }

    #[test]
    fn a_pixmap_is_preferred_to_a_name() {
        let properties = Properties {
            icon_name: "chat".into(),
            icon_pixmaps: vec![solid(16, [255, 9, 9, 9])],
            ..Properties::default()
        };
        let files = Named(&["chat"]);
        assert_eq!(
            &icon(&properties, 16, &files).unwrap().data[..4],
            [9, 9, 9, 255]
        );

        // A pixmap that cannot be used falls back to the name.
        let broken = Properties {
            icon_pixmaps: vec![Pixmap {
                width: 100_000,
                height: 100_000,
                data: vec![0; 4],
            }],
            ..properties.clone()
        };
        assert_eq!(
            &icon(&broken, 16, &files).unwrap().data[..4],
            [4, 0, 0, 255]
        );

        // Neither: no icon, and the bar draws the letter tile.
        let nameless = Properties {
            icon_name: "missing".into(),
            ..Properties::default()
        };
        assert_eq!(icon(&nameless, 16, &files), None);
        assert_eq!(icon(&Properties::default(), 16, &files), None);
    }

    #[test]
    fn an_item_needing_attention_shows_its_attention_icon() {
        let mut properties = Properties {
            icon_name: "chat".into(),
            attention_icon_name: "chat-unread".into(),
            ..Properties::default()
        };
        let files = Named(&["chat", "chat-unread"]);
        let red = |properties: &Properties| icon(properties, 16, &files).unwrap().data[0];
        assert_eq!(red(&properties), 4);
        properties.status = Status::NeedsAttention;
        assert_eq!(red(&properties), 11);
        properties.attention_pixmaps = vec![solid(16, [255, 77, 0, 0])];
        assert_eq!(red(&properties), 77);
        // With no attention icon, the ordinary one stays.
        properties.attention_pixmaps.clear();
        properties.attention_icon_name.clear();
        assert_eq!(red(&properties), 4);
    }

    #[test]
    fn the_overlay_icon_is_drawn_over_a_corner() {
        let properties = Properties {
            icon_pixmaps: vec![solid(16, [255, 0, 0, 200])],
            overlay_icon_name: "badge".into(),
            ..Properties::default()
        };
        let drawn = icon(&properties, 16, &Named(&["badge"])).unwrap();
        assert_eq!(&drawn.data[..4], [0, 0, 200, 255]);
        assert_eq!(&drawn.data[drawn.data.len() - 4..], [5, 0, 0, 255]);
        // An overlay that cannot be found changes nothing.
        let plain = icon(&properties, 16, &Named(&[])).unwrap();
        assert_eq!(&plain.data[plain.data.len() - 4..], [0, 0, 200, 255]);
    }

    #[test]
    fn pixmaps_are_dropped_once_used() {
        let properties = Properties {
            icon_pixmaps: vec![solid(64, [255; 4])],
            attention_pixmaps: vec![solid(64, [255; 4])],
            ..Properties::default()
        }
        .without_pixmaps();
        assert!(properties.icon_pixmaps.is_empty() && properties.attention_pixmaps.is_empty());
    }
}
