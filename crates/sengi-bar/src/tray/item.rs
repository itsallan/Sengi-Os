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

/// Which of an item's properties to read. An item says what changed with
/// a signal per kind, and only that kind is read again: an app that
/// updates its tooltip every second costs one small call each time, not a
/// dozen and its icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Parts(u8);

impl Parts {
    pub const NONE: Parts = Parts(0);
    pub const TITLE: Parts = Parts(1);
    pub const TOOLTIP: Parts = Parts(1 << 1);
    pub const STATUS: Parts = Parts(1 << 2);
    pub const ICON: Parts = Parts(1 << 3);
    pub const ATTENTION_ICON: Parts = Parts(1 << 4);
    pub const OVERLAY_ICON: Parts = Parts(1 << 5);
    /// The id, the menu and whether the item is only a menu, which have no
    /// signal of their own.
    pub const IDENTITY: Parts = Parts(1 << 6);
    pub const ALL: Parts = Parts(0x7f);
    /// Everything that goes into the picture.
    pub const ICONS: Parts = Parts(Self::ICON.0 | Self::ATTENTION_ICON.0 | Self::OVERLAY_ICON.0);

    /// What a change signal asks to have read again. Signals we do not
    /// know, such as libappindicator's own, mean everything; ones that are
    /// not news mean nothing.
    pub fn for_signal(member: &str) -> Parts {
        match member {
            "NewTitle" => Parts::TITLE,
            "NewToolTip" => Parts::TOOLTIP,
            "NewStatus" => Parts::STATUS,
            "NewIcon" | "NewIconThemePath" => Parts::ICON,
            "NewAttentionIcon" => Parts::ATTENTION_ICON,
            "NewOverlayIcon" => Parts::OVERLAY_ICON,
            // A label next to the icon, which the bar does not draw.
            "XAyatanaNewLabel" => Parts::NONE,
            other if other.starts_with("New") || other.starts_with("XAyatanaNew") => Parts::ALL,
            _ => Parts::NONE,
        }
    }

    pub fn contains(self, other: Parts) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn intersects(self, other: Parts) -> bool {
        self.0 & other.0 != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Parts {
    type Output = Parts;
    fn bitor(self, other: Parts) -> Parts {
        Parts(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for Parts {
    fn bitor_assign(&mut self, other: Parts) {
        self.0 |= other.0;
    }
}

/// What a connection has sent that was over a limit, and so is not asked
/// for again. An over-sized icon costs the app its icon, not its place in
/// the tray: an app that closes to the tray has no other way back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Refused(u8);

impl Refused {
    pub const NONE: Refused = Refused(0);
    pub const ICON_PIXMAP: Refused = Refused(1);
    pub const ATTENTION_PIXMAP: Refused = Refused(1 << 1);
    pub const TOOLTIP: Refused = Refused(1 << 2);
    pub const MENU: Refused = Refused(1 << 3);
    /// A line of text, which is asked for again: there is no doing
    /// without a title. A second one this long is a second offence.
    pub const TEXT: Refused = Refused(1 << 4);

    /// What an over-sized answer to this property costs.
    pub fn for_property(property: &str) -> Refused {
        match property {
            "IconPixmap" => Refused::ICON_PIXMAP,
            "AttentionIconPixmap" => Refused::ATTENTION_PIXMAP,
            "ToolTip" => Refused::TOOLTIP,
            _ => Refused::TEXT,
        }
    }

    pub fn contains(self, other: Refused) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Refused {
    type Output = Refused;
    fn bitor(self, other: Refused) -> Refused {
        Refused(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for Refused {
    fn bitor_assign(&mut self, other: Refused) {
        self.0 |= other.0;
    }
}

/// Properties exactly as they were read off the bus. None is one the item
/// does not have or that could not be read.
#[derive(Clone, Debug, Default)]
pub struct Raw {
    pub id: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub icon_name: Option<String>,
    pub icon_theme_path: Option<String>,
    pub attention_icon_name: Option<String>,
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
    pub attention_icon_name: String,
    pub overlay_icon_name: String,
    pub tooltip: String,
    /// The item has no window to raise: a click opens its menu.
    pub is_menu: bool,
    /// Object path of the item's `com.canonical.dbusmenu`, if it has one.
    pub menu: Option<String>,
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
    /// Takes over the named parts from a fresh read, cleaning them on the
    /// way in. Parts that were not read keep what they had.
    pub fn update(&mut self, parts: Parts, raw: Raw) {
        let line = |value: Option<String>| text::clean(&value.unwrap_or_default(), text::MAX_TEXT);
        if parts.contains(Parts::IDENTITY) {
            self.id = line(raw.id);
            self.is_menu = raw.is_menu.unwrap_or(false);
            self.menu = raw.menu.filter(|path| {
                // KDE's way of saying there is no menu.
                path != "/NO_DBUSMENU" && path != "/" && ObjectPath::try_from(path.as_str()).is_ok()
            });
        }
        if parts.contains(Parts::TITLE) {
            self.title = line(raw.title);
        }
        if parts.contains(Parts::STATUS) {
            self.status = raw.status.as_deref().map(Status::parse).unwrap_or_default();
        }
        if parts.contains(Parts::TOOLTIP) {
            let tooltip = raw
                .tooltip
                .map(|(title, body)| if title.trim().is_empty() { body } else { title });
            // Tooltips may carry a little markup; it is not interpreted.
            self.tooltip = line(tooltip.map(|text| strip_markup(&text)));
        }
        // Names and paths are looked up, not drawn; icons.rs vets them.
        if parts.contains(Parts::ICON) {
            self.icon_name = raw.icon_name.unwrap_or_default();
            self.icon_theme_path = raw.icon_theme_path.unwrap_or_default();
        }
        if parts.contains(Parts::ATTENTION_ICON) {
            self.attention_icon_name = raw.attention_icon_name.unwrap_or_default();
        }
        if parts.contains(Parts::OVERLAY_ICON) {
            self.overlay_icon_name = raw.overlay_icon_name.unwrap_or_default();
        }
    }

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
}

/// Which of an item's two pixmap properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Icon,
    Attention,
}

/// Where icons come from. The real one decodes files in a child process
/// and fetches pixmaps over the bus, either of which can fail in a way
/// that ends the read; tests use tables.
pub trait IconSource {
    type Error;

    /// The icon file by this name, decoded.
    fn named(
        &mut self,
        name: &str,
        theme_path: &str,
        pixels: u32,
    ) -> Result<Option<Pixels>, Self::Error>;

    /// The item's pixmaps. Only asked for when a name has not done.
    fn pixmaps(&mut self, which: Which) -> Result<Vec<Pixmap>, Self::Error>;
}

/// The icon to draw for an item, `pixels` per side. A name is tried first
/// and the pixmaps fetched only if it gives nothing, which is the order
/// Plasma and Waybar use and saves carrying the pixels over the bus for
/// every item that ships an icon file. An item that needs attention shows
/// its attention icon if it has one, and the overlay icon is drawn over
/// the bottom right corner.
pub fn icon<S: IconSource>(
    properties: &Properties,
    pixels: u32,
    source: &mut S,
) -> Result<Option<Pixels>, S::Error> {
    let theme = properties.icon_theme_path.as_str();
    let mut pick = |name: &str, which: Which| -> Result<Option<Pixels>, S::Error> {
        if !name.is_empty() {
            if let Some(icon) = source.named(name, theme, pixels)? {
                return Ok(Some(icon));
            }
        }
        Ok(pixmap::render(&source.pixmaps(which)?, pixels))
    };
    let mut icon = None;
    if properties.status == Status::NeedsAttention {
        icon = pick(&properties.attention_icon_name, Which::Attention)?;
    }
    if icon.is_none() {
        icon = pick(&properties.icon_name, Which::Icon)?;
    }
    let Some(mut icon) = icon else {
        return Ok(None);
    };
    if !properties.overlay_icon_name.is_empty() {
        if let Some(badge) = source.named(&properties.overlay_icon_name, theme, pixels / 2)? {
            pixmap::overlay(&mut icon, &badge);
        }
    }
    Ok(Some(icon))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Icons by name are a solid square whose red channel is the name's
    /// length; pixmaps are whatever the test puts in. Counts what is asked.
    #[derive(Default)]
    struct Table {
        names: &'static [&'static str],
        icon: Vec<Pixmap>,
        attention: Vec<Pixmap>,
        pixmap_reads: u32,
        fail_pixmaps: bool,
    }

    impl IconSource for Table {
        type Error = &'static str;

        fn named(
            &mut self,
            name: &str,
            _theme_path: &str,
            pixels: u32,
        ) -> Result<Option<Pixels>, Self::Error> {
            Ok(self.names.contains(&name).then(|| Pixels {
                size: pixels,
                data: [name.len() as u8, 0, 0, 255].repeat((pixels * pixels) as usize),
            }))
        }

        fn pixmaps(&mut self, which: Which) -> Result<Vec<Pixmap>, Self::Error> {
            self.pixmap_reads += 1;
            if self.fail_pixmaps {
                return Err("timed out");
            }
            Ok(match which {
                Which::Icon => self.icon.clone(),
                Which::Attention => self.attention.clone(),
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

    fn read(raw: Raw) -> Properties {
        let mut properties = Properties::default();
        properties.update(Parts::ALL, raw);
        properties
    }

    #[test]
    fn statuses() {
        assert_eq!(Status::parse("Passive"), Status::Passive);
        assert_eq!(Status::parse("Active"), Status::Active);
        assert_eq!(Status::parse("NeedsAttention"), Status::NeedsAttention);
        assert_eq!(Status::parse("passive"), Status::Active);
        assert_eq!(Status::parse(""), Status::Active);
        // An item that never says is shown.
        assert_eq!(read(Raw::default()).status, Status::Active);
    }

    #[test]
    fn each_signal_names_what_to_read_again() {
        assert_eq!(Parts::for_signal("NewTitle"), Parts::TITLE);
        assert_eq!(Parts::for_signal("NewToolTip"), Parts::TOOLTIP);
        assert_eq!(Parts::for_signal("NewStatus"), Parts::STATUS);
        assert_eq!(Parts::for_signal("NewIcon"), Parts::ICON);
        assert_eq!(Parts::for_signal("NewAttentionIcon"), Parts::ATTENTION_ICON);
        assert_eq!(Parts::for_signal("NewOverlayIcon"), Parts::OVERLAY_ICON);
        assert_eq!(Parts::for_signal("NewMenu"), Parts::ALL);
        assert_eq!(Parts::for_signal("XAyatanaNewLabel"), Parts::NONE);
        assert_eq!(Parts::for_signal("SomethingElse"), Parts::NONE);
        assert!(Parts::ALL.contains(Parts::ICONS | Parts::IDENTITY));
        assert!(!Parts::TOOLTIP.intersects(Parts::ICONS));
    }

    #[test]
    fn an_oversized_answer_costs_the_property_it_was_for() {
        assert_eq!(Refused::for_property("IconPixmap"), Refused::ICON_PIXMAP);
        assert_eq!(
            Refused::for_property("AttentionIconPixmap"),
            Refused::ATTENTION_PIXMAP
        );
        assert_eq!(Refused::for_property("ToolTip"), Refused::TOOLTIP);
        assert_eq!(Refused::for_property("Title"), Refused::TEXT);
        let both = Refused::ICON_PIXMAP | Refused::MENU;
        assert!(both.contains(Refused::MENU) && !both.contains(Refused::TOOLTIP));
        assert!(Refused::NONE.is_empty() && !both.is_empty());
    }

    #[test]
    fn only_the_parts_that_were_read_are_replaced() {
        let mut properties = read(Raw {
            id: some("chat"),
            title: some("Chat"),
            icon_name: some("chat"),
            status: some("NeedsAttention"),
            menu: some("/MenuBar"),
            ..Raw::default()
        });
        // A tooltip read knows nothing of the rest and must not blank it.
        properties.update(
            Parts::TOOLTIP,
            Raw {
                tooltip: Some(("3 unread".into(), String::new())),
                ..Raw::default()
            },
        );
        assert_eq!(properties.tooltip, "3 unread");
        assert_eq!(properties.title, "Chat");
        assert_eq!(properties.icon_name, "chat");
        assert_eq!(properties.status, Status::NeedsAttention);
        assert_eq!(properties.menu, some("/MenuBar"));
        // A part that was read and came back empty is empty.
        properties.update(Parts::TITLE | Parts::STATUS, Raw::default());
        assert_eq!(properties.title, "");
        assert_eq!(properties.status, Status::Active);
        assert_eq!(properties.id, "chat");
    }

    #[test]
    fn text_is_cleaned_on_the_way_in() {
        let properties = read(Raw {
            id: some("chat\u{0}client"),
            title: some("Chat\n\u{202e}evil\t\u{7}"),
            tooltip: Some(("".into(), "<b>3 unread</b><br/>from Sam".into())),
            ..Raw::default()
        });
        assert_eq!(properties.id, "chat client");
        assert_eq!(properties.title, "Chat evil");
        assert_eq!(properties.tooltip, "3 unread from Sam");

        let long = read(Raw {
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
        assert_eq!(read(raw.clone()).label(), "Wired connection");
        raw.tooltip = Some((" ".into(), "".into()));
        assert_eq!(read(raw.clone()).label(), "Network");
        raw.title = None;
        let properties = read(raw);
        assert_eq!(properties.label(), "nm-applet");
        assert_eq!(properties.letter(), "N");
        assert_eq!(read(Raw::default()).label(), "");
        assert_eq!(read(Raw::default()).letter(), "?");
    }

    #[test]
    fn only_a_real_object_path_is_a_menu() {
        let menu = |path: &str| {
            read(Raw {
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
    fn a_name_that_resolves_spares_the_pixmap() {
        let properties = Properties {
            icon_name: "chat".into(),
            ..Properties::default()
        };
        let mut table = Table {
            names: &["chat"],
            icon: vec![solid(16, [255, 9, 9, 9])],
            ..Table::default()
        };
        let drawn = icon(&properties, 16, &mut table).unwrap().unwrap();
        assert_eq!(&drawn.data[..4], [4, 0, 0, 255]);
        assert_eq!(table.pixmap_reads, 0);
    }

    #[test]
    fn the_pixmap_is_fetched_when_the_name_gives_nothing() {
        let mut table = Table {
            names: &[],
            icon: vec![solid(16, [255, 9, 9, 9])],
            ..Table::default()
        };
        // A name that is not installed, and no name at all.
        for name in ["missing", ""] {
            let properties = Properties {
                icon_name: name.into(),
                ..Properties::default()
            };
            let drawn = icon(&properties, 16, &mut table).unwrap().unwrap();
            assert_eq!(&drawn.data[..4], [9, 9, 9, 255]);
        }
        assert_eq!(table.pixmap_reads, 2);

        // Neither: no icon, and the bar draws the letter tile.
        table.icon.clear();
        assert_eq!(icon(&Properties::default(), 16, &mut table), Ok(None));
        // A pixmap that cannot be used is no icon either.
        table.icon = vec![Pixmap {
            width: 100_000,
            height: 100_000,
            data: vec![0; 4],
        }];
        assert_eq!(icon(&Properties::default(), 16, &mut table), Ok(None));
    }

    #[test]
    fn a_failed_fetch_ends_the_read() {
        let mut table = Table {
            fail_pixmaps: true,
            ..Table::default()
        };
        assert_eq!(
            icon(&Properties::default(), 16, &mut table),
            Err("timed out")
        );
    }

    #[test]
    fn an_item_needing_attention_shows_its_attention_icon() {
        let mut properties = Properties {
            icon_name: "chat".into(),
            attention_icon_name: "chat-unread".into(),
            ..Properties::default()
        };
        let mut table = Table {
            names: &["chat", "chat-unread"],
            ..Table::default()
        };
        let red = |properties: &Properties, table: &mut Table| {
            icon(properties, 16, table).unwrap().unwrap().data[0]
        };
        assert_eq!(red(&properties, &mut table), 4);
        properties.status = Status::NeedsAttention;
        assert_eq!(red(&properties, &mut table), 11);
        // An attention pixmap, when there is no attention name.
        properties.attention_icon_name.clear();
        table.attention = vec![solid(16, [255, 77, 0, 0])];
        assert_eq!(red(&properties, &mut table), 77);
        // With no attention icon at all, the ordinary one stays.
        table.attention.clear();
        assert_eq!(red(&properties, &mut table), 4);
    }

    #[test]
    fn the_overlay_icon_is_drawn_over_a_corner() {
        let properties = Properties {
            overlay_icon_name: "badge".into(),
            ..Properties::default()
        };
        let mut table = Table {
            names: &["badge"],
            icon: vec![solid(16, [255, 0, 0, 200])],
            ..Table::default()
        };
        let drawn = icon(&properties, 16, &mut table).unwrap().unwrap();
        assert_eq!(&drawn.data[..4], [0, 0, 200, 255]);
        assert_eq!(&drawn.data[drawn.data.len() - 4..], [5, 0, 0, 255]);
        // An overlay that cannot be found changes nothing.
        table.names = &[];
        let plain = icon(&properties, 16, &mut table).unwrap().unwrap();
        assert_eq!(&plain.data[plain.data.len() - 4..], [0, 0, 200, 255]);
    }
}
