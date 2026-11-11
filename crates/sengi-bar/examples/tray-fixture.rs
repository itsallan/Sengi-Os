//! A tray app to test the bar against: a StatusNotifierItem with a menu,
//! every part of which can be chosen on the command line or changed while
//! it runs.
//!
//!     tray-fixture NAME [--title T] [--icon pixmap|none|NAME] [--theme-path DIR]
//!                  [--status S] [--by-path] [--menu-only] [--no-activate]
//!                  [--with-pixmap] [--slow MS] [--hang-activate] [--chatty]
//!                  [--register-as SERVICE] [--names N] [--hostile] [--huge]
//!
//! `--with-pixmap` answers IconPixmap as well as a named icon. `--slow`
//! takes that long over every property, `--hang-activate` never answers
//! Activate, and `--chatty` sends NewToolTip once a second.
//! `--register-as` registers that string instead of the fixture's own
//! name, and `--names` takes that many extra bus names and registers
//! each, both to see the watcher refuse.
//!
//! While running it answers on the bus name `org.sengi.TrayFixture.NAME`:
//!
//!     busctl --user call org.sengi.TrayFixture.NAME /Fixture org.sengi.TrayFixture Set ss status NeedsAttention
//!
//! with `status`, `title`, `icon` (as --icon), `colour` (red, green, blue),
//! `overlay` (an icon name), `tooltip`, `check` (toggles the menu's check
//! row), `spam` (that many NewIcon signals at once), `flap` (that many
//! NewStatus signals, Passive and Active by turns), `forge-unregister` (a
//! StatusNotifierItemUnregistered signal for that service, as if from a
//! watcher) and `quit`. Calls it receives from the bar are printed, one
//! per line.

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
    /// Answer IconPixmap even when the icon is a name.
    with_pixmap: bool,
    /// How long every property takes to answer.
    slow: Duration,
    hang_activate: bool,
    hostile: bool,
    /// Answer IconPixmap with 16 MB.
    huge: bool,
    checked: bool,
    revision: u32,
    /// NewStatus signals of a `flap` still to go out.
    flapping: usize,
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

impl Item {
    /// The state, after taking as long over it as the fixture was told.
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        let slow = self.0.lock().unwrap().slow;
        std::thread::sleep(slow);
        self.0.lock().unwrap()
    }
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn activate(&self, x: i32, y: i32) -> zbus::fdo::Result<()> {
        let state = self.0.lock().unwrap();
        if state.hang_activate {
            println!("{}: Activate received, not answering", state.name);
            drop(state);
            std::thread::sleep(Duration::from_secs(3600));
            return Ok(());
        }
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
        self.state().name.clone()
    }

    #[zbus(property)]
    fn title(&self) -> String {
        let state = self.state();
        println!("{}: Get Title", state.name);
        state.title.clone()
    }

    #[zbus(property)]
    fn status(&self) -> String {
        self.state().status.clone()
    }

    #[zbus(property)]
    fn icon_name(&self) -> String {
        let state = self.state();
        println!("{}: Get IconName", state.name);
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
        let state = self.state();
        println!("{}: Get IconPixmap", state.name);
        if state.hostile || state.huge {
            // A lie about the size, then one that is honest and too big:
            // in dimensions, or in bytes as well.
            let side = if state.huge { 2000 } else { 300 };
            return vec![
                (100_000, 100_000, vec![255; 4]),
                (side, side, vec![255; (side * side * 4) as usize]),
            ];
        }
        match state.icon.as_str() {
            "none" => Vec::new(),
            "pixmap" => vec![
                disc(16, state.colour),
                disc(22, state.colour),
                disc(48, state.colour),
            ],
            _ if state.with_pixmap => vec![disc(32, state.colour)],
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
        let state = self.state();
        println!("{}: Get ToolTip", state.name);
        let tooltip = state.tooltip.clone();
        drop(state);
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
        Node::row(13, "Some of them")
            .with("toggle-type", "checkmark")
            .with("toggle-state", -1i32),
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
                "flap" => {
                    // The property flips with the signals, so every read
                    // really does find the other status.
                    let count: usize = value.parse().unwrap_or(1000);
                    for n in 0..count {
                        let status = if n % 2 == 0 { "Passive" } else { "Active" };
                        signals.push((ITEM, "NewStatus", Some(status.to_string())));
                    }
                    state.flapping = count;
                }
                "forge-unregister" => {
                    signals.push((
                        WATCHER,
                        "StatusNotifierItemUnregistered",
                        Some(value.into()),
                    ));
                }
                "quit" => std::process::exit(0),
                other => return Err(zbus::fdo::Error::InvalidArgs(other.to_string())),
            }
        }
        for (interface, member, text) in signals {
            let path = match interface {
                MENU => MENU_PATH,
                WATCHER => "/StatusNotifierWatcher",
                _ => ITEM_PATH,
            };
            if member == "NewStatus" {
                let mut state = self.0.lock().unwrap();
                if state.flapping > 0 {
                    state.flapping -= 1;
                    state.status = text.clone().unwrap_or_default();
                }
            }
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
        with_pixmap: false,
        slow: Duration::ZERO,
        hang_activate: false,
        hostile: false,
        huge: false,
        checked: true,
        revision: 1,
        flapping: 0,
    };
    let mut by_path = false;
    let mut chatty = false;
    let mut register_as = None;
    let mut names = 0usize;
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
            "--with-pixmap" => state.with_pixmap = true,
            "--slow" => state.slow = Duration::from_millis(value()?.parse()?),
            "--hang-activate" => state.hang_activate = true,
            "--chatty" => chatty = true,
            "--register-as" => register_as = Some(value()?),
            "--names" => names = value()?.parse()?,
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
    let own_name = bus_name.clone();
    let connection = connection::Builder::session()?
        .serve_at(ITEM_PATH, Item(shared.clone()))?
        .serve_at(MENU_PATH, Menu(shared.clone()))?
        .serve_at("/Fixture", Control(shared.clone()))?
        .name(own_name)?
        .build()?;

    // Apps register in one of two ways; see the watcher.
    let service = match register_as {
        Some(service) => service,
        None if by_path => ITEM_PATH.to_string(),
        None => connection.unique_name().unwrap().to_string(),
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
            // The watcher is there and said no.
            Err(zbus::Error::MethodError(name, text, _))
                if !name.as_str().ends_with("ServiceUnknown") =>
            {
                println!("refused {service}: {name} {}", text.unwrap_or_default());
                break;
            }
            Err(error) if attempt >= 50 => return Err(error.into()),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    println!("registered {service}");

    // A connection helping itself to names and registering every one.
    let mut accepted = 0;
    for n in 0..names {
        let name = format!("{bus_name}.n{n}");
        connection.request_name(name.as_str())?;
        let registered = connection.call_method(
            Some(WATCHER),
            "/StatusNotifierWatcher",
            Some(WATCHER),
            "RegisterStatusNotifierItem",
            &name,
        );
        match registered {
            Ok(_) => accepted += 1,
            Err(error) => println!("refused {name}: {error}"),
        }
    }
    if names > 0 {
        println!("extra names accepted: {accepted} of {names}");
    }

    let mut tick = 0;
    loop {
        if !chatty {
            std::thread::park();
            continue;
        }
        std::thread::sleep(Duration::from_secs(1));
        tick += 1;
        shared.lock().unwrap().tooltip = format!("tick {tick}");
        connection.emit_signal(None::<&str>, ITEM_PATH, ITEM, "NewToolTip", &())?;
    }
}
