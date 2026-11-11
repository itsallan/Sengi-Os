//! The StatusNotifierHost: which items there are, what each looks like and
//! what a click on one does.
//!
//! [`Host`] is a state machine. Everything that happens reaches it as an
//! [`Event`], and everything it wants done it asks of a [`Backend`]. It
//! never touches the bus itself and never waits, so it can be driven by
//! hand in tests and a stuck app cannot hold it up.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::item::{Properties, Status};
use super::menu::{self, Entry};
use super::pace::Pacer;
use super::pixmap::Pixels;
use super::watcher::{Address, Registry};

/// Reads that may time out in a row before an item is dropped.
const MAX_FAILURES: u8 = 3;

/// How long after a read that timed out the next is tried, times the
/// number of failures so far. An app that is still starting gets shown
/// once it answers; one that is stuck costs three reads and is gone.
const RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// Where on screen a click happened, for placing a menu under it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub output: String,
    pub x: i32,
}

/// An item as last read from the bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The unique name that answered, which is what its signals come from.
    pub owner: String,
    /// The interface the item speaks, KDE's or the freedesktop draft's.
    pub interface: &'static str,
    pub properties: Properties,
    pub icon: Option<Arc<Pixels>>,
}

/// Why an item could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// It has left the bus.
    Gone,
    /// It did not answer in time, or answered nothing useful.
    Unreachable,
}

#[derive(Debug)]
pub enum Event {
    /// An app registered with our watcher.
    Registered(Address),
    /// Another watcher listed or announced an item.
    Announced(String),
    /// Another watcher says an item has left.
    Withdrawn(String),
    /// A bus name has no owner any more.
    OwnerLost(String),
    /// Whether the watcher name is ours or someone else's.
    WatcherOwner {
        ours: bool,
    },
    /// An item, or its menu, says something changed.
    Signal {
        sender: String,
        path: String,
        member: String,
        /// The new status, for `NewStatus`.
        status: Option<String>,
        menu: bool,
    },
    Refreshed {
        address: Address,
        outcome: Result<Box<Snapshot>, Failure>,
    },
    Pressed {
        id: u32,
        button: Button,
        place: Place,
    },
    Scrolled {
        id: u32,
        delta: i32,
        horizontal: bool,
    },
    /// The item has no `Activate` to call.
    ActivateFailed {
        id: u32,
        place: Place,
    },
    /// Icons are now wanted at this many pixels per side.
    Pixels(u32),
    MenuLayout {
        serial: u64,
        entries: Option<Vec<Entry>>,
    },
    /// A submenu was opened in the menu on screen.
    MenuOpened {
        serial: u64,
        id: i32,
    },
    MenuClicked {
        serial: u64,
        id: i32,
    },
    MenuClosed {
        serial: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    Activate(Place),
    SecondaryActivate,
    ContextMenu,
    Scroll { delta: i32, horizontal: bool },
}

/// A method call on an item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub id: u32,
    pub bus: String,
    pub path: String,
    pub interface: &'static str,
    pub method: Method,
}

/// A request to read a menu, or to tell it about a click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuRequest {
    pub serial: u64,
    pub bus: String,
    pub path: String,
    pub action: MenuAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// Read the layout, first telling the app that this row (0 is the whole
    /// menu) is about to be shown.
    Show(i32),
    /// Read the layout again because the app says it changed.
    Reload,
    Click(i32),
}

/// One icon in the bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub id: u32,
    pub label: String,
    pub letter: String,
    pub attention: bool,
    pub icon: Option<Arc<Pixels>>,
}

/// What the host needs done. Nothing here may block.
pub trait Backend {
    /// Start hearing about this item: its signals, and its name's owner.
    fn watch(&mut self, address: &Address);
    fn unwatch(&mut self, address: &Address);
    /// Read the item and answer with [`Event::Refreshed`].
    fn refresh(&mut self, address: &Address, interface: Option<&'static str>, pixels: u32);
    fn call(&mut self, call: Call);
    /// Tell the bus an item of our watcher has gone.
    fn unregistered(&mut self, address: &Address);
    /// Another watcher owns the name: register with it and list its items.
    fn adopt_watcher(&mut self);
    fn publish(&mut self, views: Vec<View>);
    fn watch_menu(&mut self, bus: &str, path: &str);
    fn unwatch_menu(&mut self, bus: &str, path: &str);
    /// Answer with [`Event::MenuLayout`], except for a click.
    fn menu(&mut self, request: MenuRequest);
    fn show_menu(&mut self, serial: u64, place: &Place, entries: &[Entry]);
    fn update_menu(&mut self, entries: &[Entry]);
    fn close_menu(&mut self);
}

struct Item {
    id: u32,
    address: Address,
    snapshot: Option<Snapshot>,
    pacer: Pacer,
    failures: u8,
    /// When to read again after a read that failed.
    retry: Option<Instant>,
}

/// The menu on screen, or on its way there.
struct MenuSession {
    serial: u64,
    bus: String,
    owner: String,
    path: String,
    place: Place,
    /// None until the first layout has arrived and been shown.
    entries: Option<Vec<Entry>>,
    pacer: Pacer,
}

pub struct Host<B: Backend> {
    backend: B,
    registry: Arc<Mutex<Registry>>,
    items: Vec<Item>,
    next_id: u32,
    pixels: u32,
    /// Whether our own watcher is the one apps talk to.
    ours: bool,
    published: Vec<View>,
    menu: Option<MenuSession>,
    next_serial: u64,
}

impl<B: Backend> Host<B> {
    pub fn new(backend: B, registry: Arc<Mutex<Registry>>, pixels: u32) -> Self {
        Host {
            backend,
            registry,
            items: Vec::new(),
            next_id: 1,
            pixels,
            ours: true,
            published: Vec::new(),
            menu: None,
            next_serial: 1,
        }
    }

    #[cfg(test)]
    pub fn backend(&mut self) -> &mut B {
        &mut self.backend
    }

    /// When [`Host::tick`] next needs calling, if at all. With nothing
    /// waiting there is no deadline and the tray thread sleeps for good.
    pub fn deadline(&self) -> Option<Instant> {
        let menu = self.menu.as_ref().and_then(|menu| menu.pacer.due());
        self.items
            .iter()
            .flat_map(|item| [item.pacer.due(), item.retry])
            .flatten()
            .chain(menu)
            .min()
    }

    /// Starts the reads that were put off and are now due.
    pub fn tick(&mut self, now: Instant) {
        for index in 0..self.items.len() {
            if self.items[index].pacer.tick(now) {
                self.start_refresh(index);
            }
            if self.items[index].retry.is_some_and(|retry| retry <= now) {
                self.items[index].retry = None;
                self.request_refresh(index, now);
            }
        }
        if self.menu.as_mut().is_some_and(|menu| menu.pacer.tick(now)) {
            self.reload_menu();
        }
    }

    pub fn handle(&mut self, event: Event, now: Instant) {
        match event {
            Event::Registered(address) => self.add(address, now),
            Event::Announced(service) => {
                // Only another watcher's word counts here, and it goes
                // through the same checks and limits as our own.
                if self.ours {
                    return;
                }
                let added = self
                    .registry
                    .lock()
                    .ok()
                    .and_then(|mut registry| registry.register(&service, None).ok().flatten());
                if let Some(address) = added {
                    self.add(address, now);
                }
            }
            Event::Withdrawn(service) => {
                if let (false, Some(address)) = (self.ours, Address::parse(&service, None)) {
                    self.remove(&address);
                }
            }
            Event::OwnerLost(bus) => {
                let gone = match self.registry.lock() {
                    Ok(mut registry) => registry.owner_lost(&bus),
                    Err(_) => Vec::new(),
                };
                for address in &gone {
                    self.forget(address);
                }
                if !gone.is_empty() {
                    self.publish();
                }
            }
            Event::WatcherOwner { ours } => {
                self.ours = ours;
                if !ours {
                    self.backend.adopt_watcher();
                }
            }
            Event::Signal {
                sender,
                path,
                member,
                status,
                menu,
            } => {
                if menu {
                    self.menu_changed(&sender, &path, now);
                } else {
                    self.item_changed(&sender, &path, &member, status, now);
                }
            }
            Event::Refreshed { address, outcome } => self.refreshed(&address, outcome, now),
            Event::Pressed { id, button, place } => self.pressed(id, button, place, now),
            Event::Scrolled {
                id,
                delta,
                horizontal,
            } => {
                if delta != 0 {
                    self.call(id, Method::Scroll { delta, horizontal });
                }
            }
            Event::ActivateFailed { id, place } => {
                // libappindicator items have a menu and nothing else.
                if self.menu.is_none() {
                    self.open_menu(id, place, now);
                }
            }
            Event::Pixels(pixels) => {
                if pixels != self.pixels {
                    self.pixels = pixels;
                    for index in 0..self.items.len() {
                        self.request_refresh(index, now);
                    }
                }
            }
            Event::MenuLayout { serial, entries } => self.menu_layout(serial, entries, now),
            Event::MenuOpened { serial, id } => {
                let Some(session) = self.session(serial) else {
                    return;
                };
                let known = session.entries.as_deref().unwrap_or_default();
                if menu::find(known, id).is_some_and(|entry| entry.submenu) {
                    self.menu_request(MenuAction::Show(id));
                }
            }
            Event::MenuClicked { serial, id } => {
                let Some(session) = self.session(serial) else {
                    return;
                };
                // Only a row we showed, and only one that can be chosen.
                let known = session.entries.as_deref().unwrap_or_default();
                if menu::find(known, id).is_some_and(|entry| entry.enabled && !entry.submenu) {
                    self.menu_request(MenuAction::Click(id));
                }
                self.end_menu();
            }
            Event::MenuClosed { serial } => {
                if self.session(serial).is_some() {
                    self.end_menu();
                }
            }
        }
    }

    fn index(&self, address: &Address) -> Option<usize> {
        self.items.iter().position(|item| item.address == *address)
    }

    fn add(&mut self, address: Address, now: Instant) {
        let index = match self.index(&address) {
            Some(index) => index,
            None => {
                self.backend.watch(&address);
                self.items.push(Item {
                    id: self.next_id,
                    address,
                    snapshot: None,
                    pacer: Pacer::default(),
                    failures: 0,
                    retry: None,
                });
                self.next_id = self.next_id.wrapping_add(1).max(1);
                self.items.len() - 1
            }
        };
        self.request_refresh(index, now);
    }

    /// Drops an item from the registry and from the bar.
    fn remove(&mut self, address: &Address) {
        if let Ok(mut registry) = self.registry.lock() {
            registry.remove(address);
        }
        self.forget(address);
        self.publish();
    }

    /// Drops an item that the registry has let go of already.
    fn forget(&mut self, address: &Address) {
        let Some(index) = self.index(address) else {
            return;
        };
        let item = self.items.remove(index);
        self.backend.unwatch(&item.address);
        if self.ours {
            self.backend.unregistered(&item.address);
        }
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.bus == item.address.bus)
        {
            self.end_menu();
        }
    }

    fn request_refresh(&mut self, index: usize, now: Instant) {
        if self.items[index].pacer.request(now) {
            self.start_refresh(index);
        }
    }

    fn start_refresh(&mut self, index: usize) {
        let item = &self.items[index];
        let interface = item.snapshot.as_ref().map(|snapshot| snapshot.interface);
        self.backend.refresh(&item.address, interface, self.pixels);
    }

    fn refreshed(
        &mut self,
        address: &Address,
        outcome: Result<Box<Snapshot>, Failure>,
        now: Instant,
    ) {
        let Some(index) = self.index(address) else {
            return;
        };
        let item = &mut self.items[index];
        let again = item.pacer.finished(now);
        match outcome {
            Ok(snapshot) => {
                item.failures = 0;
                item.retry = None;
                item.snapshot = Some(*snapshot);
            }
            Err(Failure::Gone) => return self.remove(address),
            Err(Failure::Unreachable) => {
                item.failures += 1;
                if item.failures >= MAX_FAILURES {
                    return self.remove(address);
                }
                item.retry = Some(now + RETRY_DELAY * u32::from(item.failures));
            }
        }
        if again {
            self.start_refresh(index);
        }
        self.publish();
    }

    fn item_changed(
        &mut self,
        sender: &str,
        path: &str,
        member: &str,
        status: Option<String>,
        now: Instant,
    ) {
        let found = self.items.iter().position(|item| {
            let owner = item
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.owner.as_str());
            item.address.path == path && (item.address.bus == sender || owner == Some(sender))
        });
        let Some(index) = found else {
            return;
        };
        if !(member.starts_with("New") || member.starts_with("XAyatanaNew")) {
            return;
        }
        // The status comes with the signal, so it shows without waiting
        // for the read, which may still change the icon.
        if let (Some(status), Some(snapshot)) = (status, self.items[index].snapshot.as_mut()) {
            snapshot.properties.status = Status::parse(&status);
            self.publish();
        }
        self.request_refresh(index, now);
    }

    fn views(&self) -> Vec<View> {
        self.items
            .iter()
            .filter_map(|item| {
                let snapshot = item.snapshot.as_ref()?;
                let properties = &snapshot.properties;
                (properties.status != Status::Passive).then(|| View {
                    id: item.id,
                    label: properties.label().to_string(),
                    letter: properties.letter(),
                    attention: properties.status == Status::NeedsAttention,
                    icon: snapshot.icon.clone(),
                })
            })
            .collect()
    }

    fn publish(&mut self) {
        let views = self.views();
        if views != self.published {
            self.published = views.clone();
            self.backend.publish(views);
        }
    }

    fn snapshot(&self, id: u32) -> Option<(&Item, &Snapshot)> {
        let item = self.items.iter().find(|item| item.id == id)?;
        Some((item, item.snapshot.as_ref()?))
    }

    fn call(&mut self, id: u32, method: Method) {
        let Some((item, snapshot)) = self.snapshot(id) else {
            return;
        };
        let call = Call {
            id,
            bus: item.address.bus.clone(),
            path: item.address.path.clone(),
            interface: snapshot.interface,
            method,
        };
        self.backend.call(call);
    }

    fn pressed(&mut self, id: u32, button: Button, place: Place, now: Instant) {
        let Some((_, snapshot)) = self.snapshot(id) else {
            return;
        };
        let has_menu = snapshot.properties.menu.is_some();
        let is_menu = snapshot.properties.is_menu;
        match button {
            Button::Left if is_menu && has_menu => self.open_menu(id, place, now),
            Button::Left => self.call(id, Method::Activate(place)),
            Button::Middle => self.call(id, Method::SecondaryActivate),
            Button::Right if has_menu => self.open_menu(id, place, now),
            Button::Right => self.call(id, Method::ContextMenu),
        }
    }

    fn open_menu(&mut self, id: u32, place: Place, now: Instant) {
        let Some((item, snapshot)) = self.snapshot(id) else {
            return;
        };
        let Some(path) = snapshot.properties.menu.clone() else {
            return;
        };
        let (bus, owner) = (item.address.bus.clone(), snapshot.owner.clone());
        self.end_menu();

        let mut pacer = Pacer::default();
        pacer.request(now);
        let serial = self.next_serial;
        self.next_serial += 1;
        self.backend.watch_menu(&bus, &path);
        self.menu = Some(MenuSession {
            serial,
            bus,
            owner,
            path,
            place,
            entries: None,
            pacer,
        });
        self.menu_request(MenuAction::Show(0));
    }

    fn session(&self, serial: u64) -> Option<&MenuSession> {
        self.menu.as_ref().filter(|menu| menu.serial == serial)
    }

    fn menu_request(&mut self, action: MenuAction) {
        if let Some(session) = &self.menu {
            self.backend.menu(MenuRequest {
                serial: session.serial,
                bus: session.bus.clone(),
                path: session.path.clone(),
                action,
            });
        }
    }

    fn reload_menu(&mut self) {
        self.menu_request(MenuAction::Reload);
    }

    fn menu_changed(&mut self, sender: &str, path: &str, now: Instant) {
        let Some(session) = self.menu.as_mut() else {
            return;
        };
        let theirs = session.path == path && (session.owner == sender || session.bus == sender);
        if theirs && session.pacer.request(now) {
            self.reload_menu();
        }
    }

    fn menu_layout(&mut self, serial: u64, entries: Option<Vec<Entry>>, now: Instant) {
        let Some(session) = self.menu.as_mut().filter(|menu| menu.serial == serial) else {
            return;
        };
        let again = session.pacer.finished(now);
        let first = session.entries.is_none();
        match entries {
            Some(entries) if !entries.is_empty() => {
                if first {
                    self.backend.show_menu(serial, &session.place, &entries);
                } else if session.entries.as_ref() != Some(&entries) {
                    self.backend.update_menu(&entries);
                }
                session.entries = Some(entries);
            }
            // Nothing to show. A menu already up stays as it was.
            _ if first => return self.end_menu(),
            _ => {}
        }
        if again {
            self.reload_menu();
        }
    }

    fn end_menu(&mut self) {
        if let Some(session) = self.menu.take() {
            self.backend.unwatch_menu(&session.bus, &session.path);
            self.backend.close_menu();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray::pace::BASE_INTERVAL;
    use std::time::Duration;

    const KDE: &str = "org.kde.StatusNotifierItem";

    /// Writes down what the host asks for.
    #[derive(Default)]
    struct Recorder {
        log: Vec<String>,
        views: Vec<View>,
        calls: Vec<Call>,
        menus: Vec<MenuRequest>,
        shown: Option<Vec<Entry>>,
    }

    impl Recorder {
        fn take(&mut self) -> Vec<String> {
            std::mem::take(&mut self.log)
        }
    }

    impl Backend for Recorder {
        fn watch(&mut self, address: &Address) {
            self.log.push(format!("watch {}", address.service()));
        }
        fn unwatch(&mut self, address: &Address) {
            self.log.push(format!("unwatch {}", address.service()));
        }
        fn refresh(&mut self, address: &Address, interface: Option<&'static str>, pixels: u32) {
            let known = if interface.is_some() { " known" } else { "" };
            self.log
                .push(format!("refresh {} {pixels}{known}", address.service()));
        }
        fn call(&mut self, call: Call) {
            self.log.push(format!("call {:?}", call.method));
            self.calls.push(call);
        }
        fn unregistered(&mut self, address: &Address) {
            self.log.push(format!("unregistered {}", address.service()));
        }
        fn adopt_watcher(&mut self) {
            self.log.push("adopt".into());
        }
        fn publish(&mut self, views: Vec<View>) {
            let ids: Vec<String> = views.iter().map(|view| view.label.clone()).collect();
            self.log.push(format!("publish [{}]", ids.join(", ")));
            self.views = views;
        }
        fn watch_menu(&mut self, bus: &str, path: &str) {
            self.log.push(format!("watch menu {bus}{path}"));
        }
        fn unwatch_menu(&mut self, bus: &str, path: &str) {
            self.log.push(format!("unwatch menu {bus}{path}"));
        }
        fn menu(&mut self, request: MenuRequest) {
            self.log.push(format!("menu {:?}", request.action));
            self.menus.push(request);
        }
        fn show_menu(&mut self, _serial: u64, place: &Place, entries: &[Entry]) {
            self.log
                .push(format!("show menu at {} {}", place.output, place.x));
            self.shown = Some(entries.to_vec());
        }
        fn update_menu(&mut self, entries: &[Entry]) {
            self.log.push("update menu".into());
            self.shown = Some(entries.to_vec());
        }
        fn close_menu(&mut self) {
            self.log.push("close menu".into());
            self.shown = None;
        }
    }

    struct Rig {
        host: Host<Recorder>,
        registry: Arc<Mutex<Registry>>,
        now: Instant,
    }

    fn rig() -> Rig {
        let registry = Arc::new(Mutex::new(Registry::default()));
        Rig {
            host: Host::new(Recorder::default(), registry.clone(), 16),
            registry,
            now: Instant::now(),
        }
    }

    fn snapshot(title: &str) -> Snapshot {
        Snapshot {
            owner: ":1.50".into(),
            interface: KDE,
            properties: Properties {
                title: title.into(),
                ..Properties::default()
            },
            icon: None,
        }
    }

    fn place() -> Place {
        Place {
            output: "eDP-1".into(),
            x: 900,
        }
    }

    impl Rig {
        fn send(&mut self, event: Event) -> Vec<String> {
            self.host.handle(event, self.now);
            self.host.backend().take()
        }

        fn wait(&mut self, time: Duration) -> Vec<String> {
            self.now += time;
            self.host.tick(self.now);
            self.host.backend().take()
        }

        /// Registers the way the watcher does, then tells the host.
        fn register(&mut self, service: &str, sender: &str) -> (Address, Vec<String>) {
            let address = self
                .registry
                .lock()
                .unwrap()
                .register(service, Some(sender))
                .unwrap()
                .unwrap();
            let log = self.send(Event::Registered(address.clone()));
            (address, log)
        }

        fn answer(&mut self, address: &Address, snapshot: Snapshot) -> Vec<String> {
            self.send(Event::Refreshed {
                address: address.clone(),
                outcome: Ok(Box::new(snapshot)),
            })
        }

        /// A registered item that has been read once.
        fn item(&mut self, sender: &str, snapshot: Snapshot) -> (Address, u32) {
            let (address, _) = self.register("/StatusNotifierItem", sender);
            let snapshot = Snapshot {
                owner: sender.into(),
                ..snapshot
            };
            self.answer(&address, snapshot);
            // Long enough ago that the next change is read at once.
            self.now += Duration::from_secs(5);
            let id = self.host.items.last().unwrap().id;
            (address, id)
        }

        fn signal(&mut self, sender: &str, member: &str, status: Option<&str>) -> Vec<String> {
            self.send(Event::Signal {
                sender: sender.into(),
                path: "/StatusNotifierItem".into(),
                member: member.into(),
                status: status.map(str::to_string),
                menu: false,
            })
        }

        fn press(&mut self, id: u32, button: Button) -> Vec<String> {
            self.send(Event::Pressed {
                id,
                button,
                place: place(),
            })
        }
    }

    fn with_menu(title: &str, is_menu: bool) -> Snapshot {
        let mut snapshot = snapshot(title);
        snapshot.properties.menu = Some("/MenuBar".into());
        snapshot.properties.is_menu = is_menu;
        snapshot
    }

    fn row(id: i32, label: &str) -> Entry {
        Entry {
            id,
            label: label.into(),
            enabled: true,
            ..Entry::default()
        }
    }

    #[test]
    fn a_registered_item_is_watched_read_and_shown() {
        let mut rig = rig();
        let (address, log) = rig.register("/StatusNotifierItem", ":1.50");
        assert_eq!(
            log,
            [
                "watch :1.50/StatusNotifierItem",
                "refresh :1.50/StatusNotifierItem 16"
            ]
        );
        // Nothing is shown until it has answered.
        assert!(rig.host.backend().views.is_empty());
        assert_eq!(rig.answer(&address, snapshot("Chat")), ["publish [Chat]"]);
        let view = &rig.host.backend().views[0];
        assert_eq!((view.letter.as_str(), view.attention), ("C", false));
        assert_eq!(rig.host.deadline(), None);
    }

    #[test]
    fn registering_again_reads_the_item_again_but_adds_nothing() {
        let mut rig = rig();
        let (address, _) = rig.register("/StatusNotifierItem", ":1.50");
        rig.answer(&address, snapshot("Chat"));
        rig.now += Duration::from_secs(5);
        let log = rig.send(Event::Registered(address.clone()));
        assert_eq!(log, ["refresh :1.50/StatusNotifierItem 16 known"]);
        assert_eq!(rig.host.items.len(), 1);
    }

    #[test]
    fn items_keep_the_order_they_arrived_in() {
        let mut rig = rig();
        for (n, title) in ["One", "Two", "Three"].into_iter().enumerate() {
            rig.item(&format!(":1.{n}"), snapshot(title));
        }
        let labels: Vec<_> = rig.host.backend().views.iter().map(|v| &v.label).collect();
        assert_eq!(labels, ["One", "Two", "Three"]);
    }

    #[test]
    fn an_item_whose_owner_leaves_disappears() {
        let mut rig = rig();
        rig.item(":1.50", snapshot("Chat"));
        rig.item(":1.51", snapshot("Sync"));
        let log = rig.send(Event::OwnerLost(":1.50".into()));
        assert_eq!(
            log,
            [
                "unwatch :1.50/StatusNotifierItem",
                "unregistered :1.50/StatusNotifierItem",
                "publish [Sync]"
            ]
        );
        assert!(rig.registry.lock().unwrap().services().len() == 1);
        // A name we never heard of is nothing to us.
        assert!(rig.send(Event::OwnerLost(":1.99".into())).is_empty());
    }

    #[test]
    fn an_item_that_has_left_the_bus_is_dropped_when_a_read_finds_out() {
        let mut rig = rig();
        let (address, _) = rig.register("/StatusNotifierItem", ":1.50");
        let log = rig.send(Event::Refreshed {
            address,
            outcome: Err(Failure::Gone),
        });
        assert_eq!(
            log,
            [
                "unwatch :1.50/StatusNotifierItem",
                "unregistered :1.50/StatusNotifierItem"
            ]
        );
        assert!(rig.registry.lock().unwrap().services().is_empty());
    }

    #[test]
    fn an_item_that_keeps_timing_out_is_dropped() {
        let mut rig = rig();
        let (address, _) = rig.item(":1.50", snapshot("Stuck"));
        for round in 1..=MAX_FAILURES {
            rig.now += Duration::from_secs(60);
            assert_eq!(rig.signal(":1.50", "NewIcon", None).len(), 1);
            let log = rig.send(Event::Refreshed {
                address: address.clone(),
                outcome: Err(Failure::Unreachable),
            });
            if round < MAX_FAILURES {
                // Until then it keeps the look it had.
                assert!(log.is_empty(), "{log:?}");
                assert_eq!(rig.host.backend().views.len(), 1);
            } else {
                assert!(log.contains(&"publish []".to_string()));
            }
        }
        assert!(rig.host.items.is_empty());
    }

    #[test]
    fn an_item_that_was_slow_to_answer_is_tried_again() {
        let mut rig = rig();
        // Still starting up: the first read times out.
        let (address, _) = rig.register("/StatusNotifierItem", ":1.50");
        let failed = rig.send(Event::Refreshed {
            address: address.clone(),
            outcome: Err(Failure::Unreachable),
        });
        assert!(failed.is_empty());
        assert_eq!(rig.host.deadline(), Some(rig.now + RETRY_DELAY));
        assert!(rig.wait(RETRY_DELAY / 2).is_empty());
        assert_eq!(
            rig.wait(RETRY_DELAY),
            ["refresh :1.50/StatusNotifierItem 16"]
        );
        // This time it answers, and that is the end of the retries.
        assert_eq!(rig.answer(&address, snapshot("Slow")), ["publish [Slow]"]);
        assert_eq!(rig.host.deadline(), None);
    }

    #[test]
    fn passive_items_are_hidden_and_attention_is_marked() {
        let mut rig = rig();
        let (address, _) = rig.item(":1.50", snapshot("Chat"));
        assert_eq!(
            rig.signal(":1.50", "NewStatus", Some("NeedsAttention")),
            [
                "publish [Chat]",
                "refresh :1.50/StatusNotifierItem 16 known"
            ]
        );
        assert!(rig.host.backend().views[0].attention);

        rig.now += Duration::from_secs(5);
        let mut passive = snapshot("Chat");
        passive.properties.status = Status::Passive;
        assert_eq!(rig.answer(&address, passive), ["publish []"]);
        rig.signal(":1.50", "NewStatus", Some("Active"));
        assert_eq!(rig.host.backend().views.len(), 1);
        assert!(!rig.host.backend().views[0].attention);
    }

    #[test]
    fn signals_are_matched_to_items_by_owner_and_path() {
        let mut rig = rig();
        // Registered under a well-known name, answered by :1.50.
        let (address, _) = rig.register("org.example.Chat", ":1.50");
        rig.answer(&address, snapshot("Chat"));
        rig.now += Duration::from_secs(5);
        assert_eq!(rig.signal(":1.50", "NewIcon", None).len(), 1);
        rig.send(Event::Refreshed {
            address: address.clone(),
            outcome: Ok(Box::new(snapshot("Chat"))),
        });
        rig.now += Duration::from_secs(5);
        // Someone else, or another object, or a signal that is not news.
        assert!(rig.signal(":1.77", "NewIcon", None).is_empty());
        assert!(rig.signal(":1.50", "SomethingElse", None).is_empty());
        let stray = rig.send(Event::Signal {
            sender: ":1.50".into(),
            path: "/Other".into(),
            member: "NewIcon".into(),
            status: None,
            menu: false,
        });
        assert!(stray.is_empty());
    }

    #[test]
    fn an_item_spamming_new_icon_is_read_at_a_bounded_rate() {
        let mut rig = rig();
        rig.item(":1.50", snapshot("Spam"));
        let mut reads = 0;
        // Ten seconds of a signal every millisecond, each read answered
        // straight away.
        for _ in 0..10_000 {
            rig.now += Duration::from_millis(1);
            rig.host.tick(rig.now);
            rig.host.handle(
                Event::Signal {
                    sender: ":1.50".into(),
                    path: "/StatusNotifierItem".into(),
                    member: "NewIcon".into(),
                    status: None,
                    menu: false,
                },
                rig.now,
            );
            let log = rig.host.backend().take();
            for line in log {
                assert!(line.starts_with("refresh"), "{line}");
                reads += 1;
                let address = rig.host.items[0].address.clone();
                rig.host.handle(
                    Event::Refreshed {
                        address,
                        outcome: Ok(Box::new(snapshot("Spam"))),
                    },
                    rig.now,
                );
            }
        }
        assert!((3..=8).contains(&reads), "{reads} reads");
        // And it leaves a deadline only while a read is owed.
        assert!(rig.host.deadline().is_some());
        rig.wait(Duration::from_secs(20));
        let address = rig.host.items[0].address.clone();
        rig.answer(&address, snapshot("Spam"));
        assert_eq!(rig.host.deadline(), None);
    }

    #[test]
    fn a_deferred_read_starts_when_it_is_due() {
        let mut rig = rig();
        let (address, _) = rig.item(":1.50", snapshot("Chat"));
        assert_eq!(rig.signal(":1.50", "NewIcon", None).len(), 1);
        rig.answer(&address, snapshot("Chat"));
        // A second change right behind the first has to wait.
        assert!(rig.signal(":1.50", "NewIcon", None).is_empty());
        let due = rig.host.deadline().unwrap();
        assert!(due <= rig.now + BASE_INTERVAL);
        assert!(rig.wait(Duration::from_millis(10)).is_empty());
        assert_eq!(
            rig.wait(BASE_INTERVAL),
            ["refresh :1.50/StatusNotifierItem 16 known"]
        );
        assert_eq!(rig.host.deadline(), None);
        rig.answer(&address, snapshot("Chat"));
    }

    #[test]
    fn a_change_of_scale_reads_every_icon_again() {
        let mut rig = rig();
        rig.item(":1.50", snapshot("One"));
        rig.item(":1.51", snapshot("Two"));
        rig.now += Duration::from_secs(5);
        let log = rig.send(Event::Pixels(32));
        assert_eq!(
            log,
            [
                "refresh :1.50/StatusNotifierItem 32 known",
                "refresh :1.51/StatusNotifierItem 32 known"
            ]
        );
        assert!(rig.send(Event::Pixels(32)).is_empty());
    }

    #[test]
    fn clicks_become_calls_on_the_item() {
        let mut rig = rig();
        let (_, id) = rig.item(":1.50", snapshot("Chat"));
        rig.press(id, Button::Left);
        rig.press(id, Button::Middle);
        // No menu of its own: the app is asked to show one.
        rig.press(id, Button::Right);
        rig.send(Event::Scrolled {
            id,
            delta: -120,
            horizontal: false,
        });
        rig.send(Event::Scrolled {
            id,
            delta: 0,
            horizontal: true,
        });
        let calls = &rig.host.backend().calls;
        let methods: Vec<_> = calls.iter().map(|call| call.method.clone()).collect();
        assert_eq!(
            methods,
            [
                Method::Activate(place()),
                Method::SecondaryActivate,
                Method::ContextMenu,
                Method::Scroll {
                    delta: -120,
                    horizontal: false
                },
            ]
        );
        assert_eq!(calls[0].bus, ":1.50");
        assert_eq!(calls[0].path, "/StatusNotifierItem");
        assert_eq!(calls[0].interface, KDE);
        // A click on something that is not there does nothing.
        assert!(rig.press(999, Button::Left).is_empty());
    }

    #[test]
    fn a_right_click_fetches_and_shows_the_menu() {
        let mut rig = rig();
        let (_, id) = rig.item(":1.50", with_menu("Chat", false));
        assert_eq!(
            rig.press(id, Button::Right),
            ["watch menu :1.50/MenuBar", "menu Show(0)"]
        );
        let serial = rig.host.backend().menus[0].serial;
        let entries = vec![row(1, "Open"), row(2, "Quit")];
        let log = rig.send(Event::MenuLayout {
            serial,
            entries: Some(entries.clone()),
        });
        assert_eq!(log, ["show menu at eDP-1 900"]);
        assert_eq!(rig.host.backend().shown, Some(entries));

        // Choosing a row tells the app and closes the menu.
        let log = rig.send(Event::MenuClicked { serial, id: 2 });
        assert_eq!(
            log,
            ["menu Click(2)", "unwatch menu :1.50/MenuBar", "close menu"]
        );
        // The helper exiting afterwards is old news.
        assert!(rig.send(Event::MenuClosed { serial }).is_empty());
    }

    #[test]
    fn a_left_click_opens_the_menu_of_an_item_that_is_only_a_menu() {
        let mut rig = rig();
        let (_, id) = rig.item(":1.50", with_menu("Indicator", true));
        assert_eq!(rig.press(id, Button::Left)[1], "menu Show(0)");

        // And of one that turns out to have no Activate.
        let (_, id) = rig.item(":1.51", with_menu("Appindicator", false));
        assert_eq!(
            rig.press(id, Button::Left).last().unwrap(),
            "call Activate(Place { output: \"eDP-1\", x: 900 })"
        );
        rig.send(Event::MenuClosed { serial: 1 });
        let log = rig.send(Event::ActivateFailed { id, place: place() });
        assert_eq!(log, ["watch menu :1.51/MenuBar", "menu Show(0)"]);
    }

    #[test]
    fn only_rows_that_were_shown_and_can_be_chosen_are_clicked() {
        let mut rig = rig();
        let (_, id) = rig.item(":1.50", with_menu("Chat", false));
        let open = |rig: &mut Rig| {
            rig.press(id, Button::Right);
            let serial = rig.host.backend().menus.last().unwrap().serial;
            let disabled = Entry {
                enabled: false,
                ..row(2, "Greyed")
            };
            let sub = Entry {
                submenu: true,
                children: vec![row(4, "Inner")],
                ..row(3, "More")
            };
            rig.send(Event::MenuLayout {
                serial,
                entries: Some(vec![row(1, "Open"), disabled, sub]),
            });
            serial
        };
        for (clicked, expected) in [(2, false), (3, false), (99, false), (4, true), (1, true)] {
            let serial = open(&mut rig);
            let log = rig.send(Event::MenuClicked {
                serial,
                id: clicked,
            });
            assert_eq!(
                log.contains(&format!("menu Click({clicked})")),
                expected,
                "row {clicked}"
            );
            assert!(log.contains(&"close menu".to_string()));
        }
        // A click from a menu that is no longer the current one is ignored.
        let stale = open(&mut rig);
        open(&mut rig);
        assert!(rig
            .send(Event::MenuClicked {
                serial: stale,
                id: 1
            })
            .is_empty());
    }

    #[test]
    fn opening_a_submenu_asks_the_app_to_fill_it() {
        let mut rig = rig();
        let (_, id) = rig.item(":1.50", with_menu("Chat", false));
        rig.press(id, Button::Right);
        let serial = rig.host.backend().menus[0].serial;
        let sub = Entry {
            submenu: true,
            ..row(3, "More")
        };
        rig.send(Event::MenuLayout {
            serial,
            entries: Some(vec![row(1, "Open"), sub.clone()]),
        });
        assert_eq!(
            rig.send(Event::MenuOpened { serial, id: 3 }),
            ["menu Show(3)"]
        );
        // Not a submenu, or not a row at all.
        assert!(rig.send(Event::MenuOpened { serial, id: 1 }).is_empty());
        assert!(rig.send(Event::MenuOpened { serial, id: 77 }).is_empty());

        let filled = Entry {
            children: vec![row(4, "Inner")],
            ..sub
        };
        let log = rig.send(Event::MenuLayout {
            serial,
            entries: Some(vec![row(1, "Open"), filled]),
        });
        assert_eq!(log, ["update menu"]);
    }

    #[test]
    fn a_menu_that_changes_while_open_is_read_again_at_a_bounded_rate() {
        let mut rig = rig();
        let (_, id) = rig.item(":1.50", with_menu("Chat", false));
        rig.press(id, Button::Right);
        let serial = rig.host.backend().menus[0].serial;
        rig.send(Event::MenuLayout {
            serial,
            entries: Some(vec![row(1, "Open")]),
        });
        let changed = |rig: &mut Rig, sender: &str, path: &str| {
            rig.send(Event::Signal {
                sender: sender.into(),
                path: path.into(),
                member: "LayoutUpdated".into(),
                status: None,
                menu: true,
            })
        };
        // Too soon after the first read: put off, not dropped.
        assert!(changed(&mut rig, ":1.50", "/MenuBar").is_empty());
        assert!(changed(&mut rig, ":1.50", "/MenuBar").is_empty());
        assert_eq!(rig.wait(BASE_INTERVAL), ["menu Reload"]);
        // The same layout again is not sent to the screen twice.
        let log = rig.send(Event::MenuLayout {
            serial,
            entries: Some(vec![row(1, "Open")]),
        });
        assert!(log.is_empty());
        // Another app's menu, or another menu of this app, is not ours.
        rig.now += Duration::from_secs(5);
        assert!(changed(&mut rig, ":1.77", "/MenuBar").is_empty());
        assert!(changed(&mut rig, ":1.50", "/Other").is_empty());
        assert_eq!(changed(&mut rig, ":1.50", "/MenuBar"), ["menu Reload"]);
    }

    #[test]
    fn an_empty_or_unreadable_menu_is_not_shown() {
        let mut rig = rig();
        let (_, id) = rig.item(":1.50", with_menu("Chat", false));
        for entries in [None, Some(Vec::new())] {
            rig.press(id, Button::Right);
            let serial = rig.host.backend().menus.last().unwrap().serial;
            let log = rig.send(Event::MenuLayout { serial, entries });
            assert_eq!(log, ["unwatch menu :1.50/MenuBar", "close menu"]);
        }
    }

    #[test]
    fn the_menu_goes_when_its_item_does_or_another_is_opened() {
        let mut rig = rig();
        let (_, first) = rig.item(":1.50", with_menu("One", false));
        let (_, second) = rig.item(":1.51", with_menu("Two", false));
        rig.press(first, Button::Right);
        let log = rig.press(second, Button::Right);
        assert_eq!(
            log,
            [
                "unwatch menu :1.50/MenuBar",
                "close menu",
                "watch menu :1.51/MenuBar",
                "menu Show(0)"
            ]
        );
        let log = rig.send(Event::OwnerLost(":1.51".into()));
        assert!(log.contains(&"close menu".to_string()), "{log:?}");
    }

    #[test]
    fn with_another_watcher_its_items_are_adopted() {
        let mut rig = rig();
        assert_eq!(rig.send(Event::WatcherOwner { ours: false }), ["adopt"]);
        let log = rig.send(Event::Announced(":1.50/StatusNotifierItem".into()));
        assert_eq!(
            log,
            [
                "watch :1.50/StatusNotifierItem",
                "refresh :1.50/StatusNotifierItem 16"
            ]
        );
        // Listed again, or nonsense.
        assert!(rig
            .send(Event::Announced(":1.50/StatusNotifierItem".into()))
            .is_empty());
        assert!(rig
            .send(Event::Announced("not a service".into()))
            .is_empty());

        // It is the other watcher's place to announce that one has left.
        let log = rig.send(Event::Withdrawn(":1.50/StatusNotifierItem".into()));
        assert_eq!(log, ["unwatch :1.50/StatusNotifierItem"]);
        assert!(rig.registry.lock().unwrap().services().is_empty());
    }

    #[test]
    fn taking_over_the_watcher_name_keeps_the_items() {
        let mut rig = rig();
        rig.send(Event::WatcherOwner { ours: false });
        rig.send(Event::Announced(":1.50/StatusNotifierItem".into()));
        assert!(rig.send(Event::WatcherOwner { ours: true }).is_empty());
        assert_eq!(rig.host.items.len(), 1);
        assert_eq!(
            rig.registry.lock().unwrap().services(),
            [":1.50/StatusNotifierItem"]
        );
        // From here on announcements from elsewhere are not believed.
        assert!(rig
            .send(Event::Announced(":1.51/StatusNotifierItem".into()))
            .is_empty());
        assert!(rig
            .send(Event::Withdrawn(":1.50/StatusNotifierItem".into()))
            .is_empty());
    }
}
