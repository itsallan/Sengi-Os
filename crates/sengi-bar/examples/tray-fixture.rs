//! A tray app to test the bar against: a StatusNotifierItem with a menu,
//! every part of which can be chosen on the command line or changed while
//! it runs.
//!
//!     tray-fixture NAME [--title T] [--icon pixmap|none|NAME] [--theme-path DIR]
//!                  [--status S] [--by-path] [--menu-only] [--no-activate]
//!                  [--hostile] [--huge]
//!
//! While running it answers on the bus name `org.sengi.TrayFixture.NAME`:
//!
//!     busctl --user call org.sengi.TrayFixture.NAME /Fixture org.sengi.TrayFixture Set ss status NeedsAttention
//!
//! with `status`, `title`, `icon` (as --icon), `colour` (red, green, blue),
//! `overlay` (an icon name), `tooltip`, `check` (toggles the menu's check
//! row), `spam` (that many NewIcon signals at once) and `quit`. Calls it
//! receives from the bar are printed, one per line.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::blocking::connection;
use zbus::zvariant::{OwnedObjectPath, Structure, Value};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

type Pixmap = (i32, i32, Vec<u8>);

struct State {
    name: String,
    title: String,
    status: String,
    /// "pixmap", "none" or an icon name.
    icon: String,
    colour: [u8; 3],
    theme_path: String,
    overlay: String,
    tooltip: String,
    menu_only: bool,
    activate: bool,
    hostile: bool,
    /// Answer IconPixmap with 16 MB.
    huge: bool,
    checked: bool,
    revision: u32,
}

type Shared = Arc<Mutex<State>>;

/// A disc of one colour, in ARGB32 network byte order.
fn disc(side: i32, [r, g, b]: [u8; 3]) -> Pixmap {
    let mut data = Vec::with_capacity((side * side * 4) as usize);
    let middle = (side as f32 - 1.0) / 2.0;
    for y in 0..side {
        for x in 0..side {
            let distance = ((x as f32 - middle).powi(2) + (y as f32 - middle).powi(2)).sqrt();
            // A soft edge, to show that alpha is handled.
            let alpha = ((side as f32 / 2.0 - distance).clamp(0.0, 1.0) * 255.0) as u8;
            data.extend_from_slice(&[alpha, r, g, b]);
        }
    }
    (side, side, data)
}

struct Item(Shared);

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn activate(&self, x: i32, y: i32) -> zbus::fdo::Result<()> {
        let state = self.0.lock().unwrap();
        if !state.activate {
            return Err(zbus::fdo::Error::UnknownMethod("no Activate here".into()));
        }
        println!("{}: Activate {x} {y}", state.name);
        Ok(())
    }

    fn secondary_activate(&self, x: i32, y: i32) {
        println!("{}: SecondaryActivate {x} {y}", self.0.lock().unwrap().name);
    }

    fn context_menu(&self, x: i32, y: i32) {
        println!("{}: ContextMenu {x} {y}", self.0.lock().unwrap().name);
    }

    fn scroll(&self, delta: i32, orientation: &str) {
        println!(
            "{}: Scroll {delta} {orientation}",
            self.0.lock().unwrap().name
        );
    }

    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> String {
        self.0.lock().unwrap().name.clone()
    }

    #[zbus(property)]
    fn title(&self) -> String {
        self.0.lock().unwrap().title.clone()
    }

    #[zbus(property)]
    fn status(&self) -> String {
        self.0.lock().unwrap().status.clone()
    }

    #[zbus(property)]
    fn icon_name(&self) -> String {
        let state = self.0.lock().unwrap();
        match state.icon.as_str() {
            "pixmap" | "none" => String::new(),
            name => name.to_string(),
        }
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> String {
        self.0.lock().unwrap().theme_path.clone()
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        let state = self.0.lock().unwrap();
        if state.hostile || state.huge {
            // A lie about the size, then one that is honest and too big:
            // in dimensions, or in bytes as well.
            let side = if state.huge { 2000 } else { 1100 };
            return vec![
                (100_000, 100_000, vec![255; 4]),
                (side, side, vec![255; (side * side * 4) as usize]),
            ];
        }
        match state.icon.as_str() {
            "pixmap" => vec![
                disc(16, state.colour),
                disc(22, state.colour),
                disc(48, state.colour),
            ],
            _ => Vec::new(),
        }
    }

    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Vec<Pixmap> {
        let state = self.0.lock().unwrap();
        match state.icon.as_str() {
            // The attention icon is the same disc in white.
            "pixmap" => vec![disc(32, [255, 255, 255])],
            _ => Vec::new(),
        }
    }

    #[zbus(property)]
    fn overlay_icon_name(&self) -> String {
        self.0.lock().unwrap().overlay.clone()
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<Pixmap>, String, String) {
        let tooltip = self.0.lock().unwrap().tooltip.clone();
        (String::new(), Vec::new(), tooltip, String::new())
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        self.0.lock().unwrap().menu_only
    }

    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(MENU_PATH).unwrap()
    }
}

/// A menu node on its way to becoming `(ia{sv}av)`.
struct Node {
    id: i32,
    properties: Vec<(&'static str, Value<'static>)>,
    children: Vec<Node>,
}

type Layout = (i32, HashMap<String, Value<'static>>, Vec<Value<'static>>);

impl Node {
    fn row(id: i32, label: &str) -> Node {
        Node {
            id,
            properties: vec![("label", Value::from(label.to_string()))],
            children: Vec::new(),
        }
    }

    fn with(mut self, key: &'static str, value: impl Into<Value<'static>>) -> Node {
        self.properties.push((key, value.into()));
        self
    }

    fn separator(id: i32) -> Node {
        Node::row(id, "").with("type", "separator")
    }

    fn submenu(self, children: Vec<Node>) -> Node {
        Node {
            children,
            ..self.with("children-display", "submenu")
        }
    }

    fn layout(self) -> Layout {
        let properties = self
            .properties
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        let children = self
            .children
            .into_iter()
            .map(|child| Value::from(Structure::from(child.layout())))
            .collect();
        (self.id, properties, children)
    }
}

fn menu(state: &State) -> Node {
    let root = Node {
        id: 0,
        properties: Vec::new(),
        children: Vec::new(),
    };
    if state.hostile {
        let rows = (1..=10_000)
            .map(|id| Node::row(id, &format!("Row {id}\u{202e}\n\u{7}")))
            .collect();
        return root.submenu(rows);
    }
    root.submenu(vec![
        Node::row(1, "_Open window"),
        Node::separator(2),
        Node::row(3, "Sync _enabled")
            .with("toggle-type", "checkmark")
            .with("toggle-state", i32::from(state.checked)),
        Node::row(4, "Not available").with("enabled", false),
        Node::row(5, "Hidden").with("visible", false),
        Node::row(6, "_More").submenu(vec![
            Node::row(7, "Fast")
                .with("toggle-type", "radio")
                .with("toggle-state", 1i32),
            Node::row(8, "Slow")
                .with("toggle-type", "radio")
                .with("toggle-state", 0i32),
            Node::row(9, "Deeper").submenu(vec![Node::row(10, "Leaf")]),
        ]),
        Node::separator(11),
        Node::row(12, "_Quit"),
    ])
}

struct Menu(Shared);

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    fn get_layout(&self, parent: i32, depth: i32, _properties: Vec<String>) -> (u32, Layout) {
        let state = self.0.lock().unwrap();
        println!("{}: GetLayout {parent} {depth}", state.name);
        (state.revision, menu(&state).layout())
    }

    fn about_to_show(&self, id: i32) -> bool {
        println!("{}: AboutToShow {id}", self.0.lock().unwrap().name);
        false
    }

    fn event(&self, id: i32, event: &str, _data: Value<'_>, _timestamp: u32) {
        println!("{}: Event {id} {event}", self.0.lock().unwrap().name);
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
}

struct Control(Shared);

#[zbus::interface(name = "org.sengi.TrayFixture")]
impl Control {
    async fn set(
        &self,
        what: &str,
        value: &str,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let mut signals: Vec<(&str, &str, Option<String>)> = Vec::new();
        {
            let mut state = self.0.lock().unwrap();
            match what {
                "status" => {
                    state.status = value.to_string();
                    signals.push((ITEM, "NewStatus", Some(value.to_string())));
                }
                "title" => {
                    state.title = value.to_string();
                    signals.push((ITEM, "NewTitle", None));
                }
                "tooltip" => {
                    state.tooltip = value.to_string();
                    signals.push((ITEM, "NewToolTip", None));
                }
                "icon" => {
                    state.icon = value.to_string();
                    signals.push((ITEM, "NewIcon", None));
                }
                "overlay" => {
                    state.overlay = value.to_string();
                    signals.push((ITEM, "NewOverlayIcon", None));
                }
                "colour" => {
                    state.colour = colour(value);
                    signals.push((ITEM, "NewIcon", None));
                }
                "check" => {
                    state.checked = !state.checked;
                    state.revision += 1;
                    signals.push((MENU, "LayoutUpdated", None));
                }
                "spam" => {
                    let count = value.parse().unwrap_or(1000);
                    signals.extend(std::iter::repeat_n((ITEM, "NewIcon", None), count));
                }
                "quit" => std::process::exit(0),
                other => return Err(zbus::fdo::Error::InvalidArgs(other.to_string())),
            }
        }
        for (interface, member, text) in signals {
            let path = if interface == MENU {
                MENU_PATH
            } else {
                ITEM_PATH
            };
            let sent = match (member, text) {
                ("LayoutUpdated", _) => {
                    connection
                        .emit_signal(None::<&str>, path, interface, member, &(0u32, 0i32))
                        .await
                }
                (_, Some(text)) => {
                    connection
                        .emit_signal(None::<&str>, path, interface, member, &text)
                        .await
                }
                (_, None) => {
                    connection
                        .emit_signal(None::<&str>, path, interface, member, &())
                        .await
                }
            };
            sent.map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        }
        Ok(())
    }
}

fn colour(name: &str) -> [u8; 3] {
    match name {
        "red" => [229, 83, 75],
        "green" => [76, 174, 107],
        "blue" => [80, 140, 230],
        _ => [217, 119, 58],
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let name = args.next().ok_or("usage: tray-fixture NAME [options]")?;
    let mut state = State {
        title: name.clone(),
        name,
        status: "Active".into(),
        icon: "pixmap".into(),
        colour: colour(""),
        theme_path: String::new(),
        overlay: String::new(),
        tooltip: String::new(),
        menu_only: false,
        activate: true,
        hostile: false,
        huge: false,
        checked: true,
        revision: 1,
    };
    let mut by_path = false;
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--title" => state.title = value()?,
            "--icon" => state.icon = value()?,
            "--colour" => state.colour = colour(&value()?),
            "--theme-path" => state.theme_path = value()?,
            "--status" => state.status = value()?,
            "--tooltip" => state.tooltip = value()?,
            "--by-path" => by_path = true,
            "--menu-only" => state.menu_only = true,
            "--no-activate" => state.activate = false,
            "--huge" => state.huge = true,
            "--hostile" => {
                state.hostile = true;
                state.title = format!(
                    "\u{1b}[31m{}\u{202e}evil\u{7}\n\t{}",
                    state.name,
                    "long ".repeat(2_000)
                );
            }
            other => return Err(format!("unknown option {other}").into()),
        }
    }

    let bus_name = format!("org.sengi.TrayFixture.{}", state.name);
    let shared = Arc::new(Mutex::new(state));
    let connection = connection::Builder::session()?
        .serve_at(ITEM_PATH, Item(shared.clone()))?
        .serve_at(MENU_PATH, Menu(shared.clone()))?
        .serve_at("/Fixture", Control(shared))?
        .name(bus_name)?
        .build()?;

    // Apps register in one of two ways; see the watcher.
    let service = if by_path {
        ITEM_PATH.to_string()
    } else {
        connection.unique_name().unwrap().to_string()
    };
    // The bar may still be starting.
    for attempt in 0.. {
        let registered = connection.call_method(
            Some(WATCHER),
            "/StatusNotifierWatcher",
            Some(WATCHER),
            "RegisterStatusNotifierItem",
            &service,
        );
        match registered {
            Ok(_) => break,
            Err(error) if attempt >= 50 => return Err(error.into()),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    println!("registered {service}");
    loop {
        std::thread::park();
    }
}
