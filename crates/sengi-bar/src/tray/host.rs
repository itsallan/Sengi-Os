//! The StatusNotifierHost: which items there are, what each looks like and
//! what a click on one does.
//!
//! [`Host`] is a state machine. Everything that happens reaches it as an
//! [`Event`], and everything it wants done it asks of a [`Backend`]. It
//! never touches the bus itself and never waits, so it can be driven by
//! hand in tests and a stuck app cannot hold it up.
//!
//! Items are other people's programs, and the rules here are about what
//! one of them can cost. An item is read at a bounded rate however often
//! it says it has changed, and only the part it names is read. Nothing an
//! item sends reaches the screen except through such a read. An item has
//! at most one read, one activation and one menu request under way at a
//! time, so a dead one ties up one worker of each kind and no more.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::item::{Parts, Properties, Refused, Status};
use super::menu::{self, Entry};
use super::pace::Pacer;
use super::pixmap::Pixels;
use super::text;
use super::watcher::{Address, Registry};

/// Reads that may time out in a row before an item is dropped.
const MAX_FAILURES: u8 = 3;

/// How long after a read that timed out the next is tried, times the
/// number of failures so far. An app that is still starting gets shown
/// once it answers; one that is stuck costs three reads and is gone.
const RETRY_DELAY: Duration = Duration::from_secs(5);

/// Shortest time between two `Scroll` calls to one item. A touchpad sends
/// a stream of small movements; they are added up and sent as one.
const SCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// Most scrolling that is kept for an item that is not being sent any.
const MAX_SCROLL: f32 = 10_000.0;

/// Shortest time between an item appearing and disappearing, or the
/// other way about. When an item to the right hides, every icon to its
/// left moves, and clicks on those are ignored for a moment; an item
/// that could flip as fast as it is read could keep them unclickable.
const FLIP_INTERVAL: Duration = Duration::from_secs(5);

/// Connections whose refusals are remembered at once.
const MAX_REFUSALS: usize = 64;

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

/// An item's icon at each size it is drawn at, smallest first.
pub type Icons = Vec<(u32, Arc<Pixels>)>;

/// An item as last read from the bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The unique name that answered, which is what its signals come from.
    pub owner: String,
    /// The interface the item speaks, KDE's or the freedesktop draft's.
    pub interface: &'static str,
    pub properties: Properties,
    pub icons: Icons,
    /// Answers in this read that were over their limit and were dropped.
    pub over: Refused,
}

/// Why an item could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// It has left the bus.
    Gone,
    /// It did not answer in time, or answered nothing useful.
    Unreachable,
}

/// A request to read an item, or some of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refresh {
    pub address: Address,
    /// What was read before, to build on. None reads everything.
    pub previous: Option<Snapshot>,
    pub parts: Parts,
    /// The sizes icons are wanted at.
    pub pixels: Vec<u32>,
    /// What the item's connection is not asked for any more.
    pub refused: Refused,
}

#[derive(Debug)]
pub enum Event {
    /// An app registered with our watcher.
    Registered(Address),
    /// The watcher we are a guest of listed or announced an item.
    Announced(String),
    /// That watcher says an item has left.
    Withdrawn(String),
    /// A bus name has no owner any more.
    OwnerLost(String),
    /// A well-known name passed straight from one connection to another.
    OwnerChanged {
        name: String,
        owner: String,
    },
    /// Whether the watcher name is ours or someone else's.
    WatcherOwner {
        ours: bool,
    },
    /// An item, or its menu, says something changed.
    Signal {
        sender: String,
        path: String,
        member: String,
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
    /// The wheel moved over an item, by this much.
    Scrolled {
        id: u32,
        delta: f32,
        horizontal: bool,
    },
    /// The "more" item was pressed on a bar too narrow for these items.
    More {
        place: Place,
        hidden: Vec<u32>,
    },
    /// An `Activate` came back. `fallback` is where to open the menu
    /// instead, when the item turned out to have no `Activate`.
    Activated {
        id: u32,
        fallback: Option<Place>,
    },
    /// Icons are now wanted at these sizes.
    Pixels(Vec<u32>),
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
    /// A connection sent something far over a limit that nobody was
    /// waiting for.
    Misbehaved {
        owner: String,
    },
    /// A connection answered a request with something over its limit.
    Oversized {
        owner: String,
        what: Refused,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    Activate(Place),
    SecondaryActivate,
    ContextMenu,
    Scroll { delta: i32, horizontal: bool },
}

/// A method call on an item. Only `Activate` is waited for, and answered
/// with [`Event::Activated`]; the others are sent and forgotten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub id: u32,
    pub bus: String,
    pub path: String,
    pub interface: &'static str,
    pub method: Method,
}

/// A request to read a menu, or to tell it something.
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
    /// menu) is about to be shown. Answered with [`Event::MenuLayout`].
    Show(i32),
    /// Read the layout again because the app says it changed. Answered
    /// with [`Event::MenuLayout`].
    Reload,
    /// A row was chosen. Not answered.
    Click(i32),
    /// The menu has gone from the screen. Not answered.
    Closed,
}

/// One icon in the bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub id: u32,
    pub label: String,
    pub letter: String,
    pub attention: bool,
    pub icons: Icons,
}

/// What the host needs done. Nothing here may block.
pub trait Backend {
    /// Start hearing about this item: its signals, and its name's owner.
    fn watch(&mut self, address: &Address);
    fn unwatch(&mut self, address: &Address);
    /// Start hearing when this name leaves the bus, and nothing else.
    fn watch_owner(&mut self, name: &str);
    fn unwatch_owner(&mut self, name: &str);
    /// Read the item and answer with [`Event::Refreshed`].
    fn refresh(&mut self, request: Refresh);
    fn call(&mut self, call: Call);
    /// Tell the bus an item of our watcher has gone.
    fn unregistered(&mut self, address: &Address);
    /// Another watcher owns the name: register with it and list its items.
    fn adopt_watcher(&mut self);
    /// The icons to draw, leftmost first.
    fn publish(&mut self, views: Vec<View>);
    fn watch_menu(&mut self, bus: &str, path: &str);
    fn unwatch_menu(&mut self, bus: &str, path: &str);
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
    /// What has changed since the last read began.
    wanted: Parts,
    /// What the read under way is fetching.
    reading: Parts,
    failures: u8,
    /// When to read again after a read that failed.
    retry: Option<Instant>,
    /// An `Activate` is on its way to the app and has not come back.
    activating: bool,
    /// Scrolling not yet sent on: vertical, then horizontal.
    scroll: [f32; 2],
    /// The earliest the next `Scroll` may go out.
    scroll_after: Option<Instant>,
    /// Set while scrolling is waiting for that moment.
    scroll_due: Option<Instant>,
    /// Whether the item is in the tray as the bars have it. This follows
    /// its status, but no faster than [`FLIP_INTERVAL`].
    visible: bool,
    /// The earliest it may next appear or disappear.
    flip_after: Option<Instant>,
    /// Set while a change of status is waiting for that moment.
    flip_due: Option<Instant>,
}

/// Whose menu is on screen.
enum Target {
    /// An item's own, read from the app.
    Item {
        bus: String,
        owner: String,
        path: String,
    },
    /// The list of items that did not fit on the bar.
    Overflow,
}

/// The menu on screen, or on its way there.
struct MenuSession {
    serial: u64,
    target: Target,
    place: Place,
    /// None until the first layout has arrived and been shown.
    entries: Option<Vec<Entry>>,
    pacer: Pacer,
    /// A row the app is still to be told is about to show.
    about: Option<i32>,
}

pub struct Host<B: Backend> {
    backend: B,
    registry: Arc<Mutex<Registry>>,
    items: Vec<Item>,
    next_id: u32,
    pixels: Vec<u32>,
    /// Whether our own watcher is the one apps talk to.
    ours: bool,
    published: Vec<View>,
    menu: Option<MenuSession>,
    next_serial: u64,
    /// What each connection has been refused, by unique name, oldest
    /// first. Forgotten when the connection leaves the bus.
    refusals: Vec<(String, Refused)>,
}

impl<B: Backend> Host<B> {
    pub fn new(backend: B, registry: Arc<Mutex<Registry>>, pixels: Vec<u32>) -> Self {
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
            refusals: Vec::new(),
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
            .flat_map(|item| [item.pacer.due(), item.retry, item.scroll_due, item.flip_due])
            .flatten()
            .chain(menu)
            .min()
    }

    /// Starts the work that was put off and is now due.
    pub fn tick(&mut self, now: Instant) {
        for index in 0..self.items.len() {
            if self.items[index].pacer.tick(now) {
                self.start_refresh(index);
            }
            if self.items[index].retry.is_some_and(|retry| retry <= now) {
                self.items[index].retry = None;
                self.request_refresh(index, Parts::NONE, now);
            }
            if self.items[index].scroll_due.is_some_and(|due| due <= now) {
                self.flush_scroll(index, now);
            }
            if self.items[index].flip_due.is_some_and(|due| due <= now) {
                self.settle(index, now);
                self.publish();
            }
        }
        if self.menu.as_mut().is_some_and(|menu| menu.pacer.tick(now)) {
            self.fetch_menu();
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
                    .and_then(|mut registry| registry.adopt(&service).ok().flatten());
                if let Some(address) = added {
                    self.add(address, now);
                }
            }
            Event::Withdrawn(service) => {
                if let (false, Some(address)) = (self.ours, Address::listed(&service)) {
                    self.remove(&address);
                }
            }
            Event::OwnerLost(bus) => {
                let (gone, unbanned) = match self.registry.lock() {
                    Ok(mut registry) => registry.owner_lost(&bus),
                    Err(_) => (Vec::new(), false),
                };
                for address in &gone {
                    self.forget(address);
                }
                if unbanned {
                    self.backend.unwatch_owner(&bus);
                }
                let held = self.refusals.len();
                self.refusals.retain(|(owner, _)| *owner != bus);
                if self.refusals.len() != held {
                    self.backend.unwatch_owner(&bus);
                }
                if !gone.is_empty() {
                    self.publish();
                }
            }
            Event::OwnerChanged { name, owner } => self.owner_changed(&name, &owner, now),
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
                menu,
            } => {
                if menu {
                    self.menu_changed(&sender, &path, now);
                } else {
                    self.item_changed(&sender, &path, &member, now);
                }
            }
            Event::Refreshed { address, outcome } => self.refreshed(&address, outcome, now),
            Event::Pressed { id, button, place } => self.pressed(id, button, place, now),
            Event::Scrolled {
                id,
                delta,
                horizontal,
            } => self.scrolled(id, delta, horizontal, now),
            Event::More { place, hidden } => self.open_overflow(place, &hidden),
            Event::Activated { id, fallback } => {
                if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
                    item.activating = false;
                }
                // libappindicator items have a menu and nothing else.
                if let (Some(place), true) = (fallback, self.menu.is_none()) {
                    self.open_menu(id, place, now);
                }
            }
            Event::Pixels(pixels) => {
                if pixels != self.pixels {
                    self.pixels = pixels;
                    for index in 0..self.items.len() {
                        self.request_refresh(index, Parts::ICONS, now);
                    }
                }
            }
            Event::MenuLayout { serial, entries } => self.menu_layout(serial, entries, now),
            Event::MenuOpened { serial, id } => self.menu_opened(serial, id, now),
            Event::MenuClicked { serial, id } => self.menu_clicked(serial, id, now),
            Event::MenuClosed { serial } => {
                if self.session(serial).is_some() {
                    self.end_menu();
                }
            }
            Event::Misbehaved { owner } => self.ban(&owner),
            Event::Oversized { owner, what } => {
                self.refuse(&owner, what);
                // A menu that is not to be asked for is not left open.
                let theirs = self.menu.as_ref().is_some_and(|menu| {
                    matches!(&menu.target, Target::Item { owner: held, .. } if *held == owner)
                });
                if theirs && what.contains(Refused::MENU) {
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
                    wanted: Parts::NONE,
                    reading: Parts::NONE,
                    failures: 0,
                    retry: None,
                    activating: false,
                    scroll: [0.0; 2],
                    scroll_after: None,
                    scroll_due: None,
                    visible: false,
                    flip_after: None,
                    flip_due: None,
                });
                self.next_id = self.next_id.wrapping_add(1).max(1);
                self.items.len() - 1
            }
        };
        self.request_refresh(index, Parts::ALL, now);
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
        let theirs = self.menu.as_ref().is_some_and(|menu| match &menu.target {
            Target::Item { bus, .. } => *bus == item.address.bus,
            // The list is of items; with one gone it is out of date.
            Target::Overflow => true,
        });
        if theirs {
            self.end_menu();
        }
    }

    /// Stops listening to a connection until it leaves the bus: one that
    /// sent megabytes nobody asked for, or went over a limit again after
    /// being let off once.
    fn ban(&mut self, owner: &str) {
        // Its items may be registered under well-known names, which is
        // all that is known of who registered them when they were
        // adopted from another watcher. Those names are banned with it.
        let mut names = vec![owner.to_string()];
        for item in &self.items {
            let theirs = item.snapshot.as_ref().is_some_and(|s| s.owner == owner);
            if theirs && !names.contains(&item.address.bus) {
                names.push(item.address.bus.clone());
            }
        }
        for name in &names {
            let banned = match self.registry.lock() {
                Ok(mut registry) => registry.ban(name),
                Err(_) => return,
            };
            for address in &banned.gone {
                self.forget(address);
            }
            if banned.new {
                // To know when the name is free again.
                self.backend.watch_owner(name);
            }
            if let Some(evicted) = banned.evicted {
                self.backend.unwatch_owner(&evicted);
            }
        }
        self.publish();
    }

    fn refused(&self, owner: &str) -> Refused {
        self.refusals
            .iter()
            .find(|(held, _)| held == owner)
            .map_or(Refused::NONE, |(_, refused)| *refused)
    }

    /// A connection answered with something over a limit. The first time
    /// costs it that property: it is not asked for again, and the item
    /// stays, with a letter for an icon or without its tooltip or menu.
    /// So the most one connection can make us carry is one such answer.
    /// A second time and it is not listened to at all.
    fn refuse(&mut self, owner: &str, what: Refused) {
        if what.is_empty() {
            return;
        }
        if !self.refused(owner).is_empty() {
            return self.ban(owner);
        }
        if self.refusals.len() >= MAX_REFUSALS {
            let (evicted, _) = self.refusals.remove(0);
            self.backend.unwatch_owner(&evicted);
        }
        self.refusals.push((owner.to_string(), what));
        // To forget this when the name is free again.
        self.backend.watch_owner(owner);
    }

    /// Brings whether an item is in the tray into line with its status,
    /// unless it changed too recently, in which case it is left as it is
    /// and looked at again when the time is up.
    fn settle(&mut self, index: usize, now: Instant) {
        let item = &mut self.items[index];
        item.flip_due = None;
        let wanted = item
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.properties.status != Status::Passive);
        if wanted == item.visible {
            return;
        }
        match item.flip_after {
            Some(after) if now < after => item.flip_due = Some(after),
            _ => {
                item.visible = wanted;
                item.flip_after = Some(now + FLIP_INTERVAL);
            }
        }
    }

    fn owner_changed(&mut self, name: &str, owner: &str, now: Instant) {
        for index in 0..self.items.len() {
            let item = &mut self.items[index];
            if item.address.bus != name {
                continue;
            }
            // Its signals now come from the new owner, which may be quite
            // another program.
            if let Some(snapshot) = &mut item.snapshot {
                snapshot.owner = owner.to_string();
            }
            self.request_refresh(index, Parts::ALL, now);
        }
        if let Some(Target::Item {
            bus, owner: held, ..
        }) = self.menu.as_mut().map(|menu| &mut menu.target)
        {
            if bus == name {
                *held = owner.to_string();
            }
        }
    }

    /// Notes that these parts of an item have changed and reads them when
    /// its pacer allows.
    fn request_refresh(&mut self, index: usize, parts: Parts, now: Instant) {
        let item = &mut self.items[index];
        item.wanted |= parts;
        if item.pacer.request(now) {
            self.start_refresh(index);
        }
    }

    fn start_refresh(&mut self, index: usize) {
        let item = &mut self.items[index];
        let mut parts = std::mem::take(&mut item.wanted);
        if item.snapshot.is_none() || parts.is_empty() {
            parts = Parts::ALL;
        }
        item.reading = parts;
        let request = Refresh {
            address: item.address.clone(),
            previous: item.snapshot.clone(),
            parts,
            pixels: self.pixels.clone(),
            refused: Refused::NONE,
        };
        let refused = request
            .previous
            .as_ref()
            .map_or(Refused::NONE, |snapshot| self.refused(&snapshot.owner));
        self.backend.refresh(Refresh { refused, ..request });
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
        item.pacer.finished(now);
        match outcome {
            Ok(mut snapshot) => {
                item.failures = 0;
                item.retry = None;
                // An icon that was drawn again and came out the same is
                // the same icon: the bars keep the picture they have, and
                // nothing has changed under the pointer.
                if let Some(old) = &item.snapshot {
                    for (size, icon) in &mut snapshot.icons {
                        let held = old.icons.iter().find(|(s, held)| s == size && held == icon);
                        if let Some((_, held)) = held {
                            *icon = held.clone();
                        }
                    }
                }
                let (owner, over) = (snapshot.owner.clone(), snapshot.over);
                snapshot.over = Refused::NONE;
                item.snapshot = Some(*snapshot);
                self.settle(index, now);
                // After the item is in place: a second offence bans the
                // connection and takes the item with it.
                self.refuse(&owner, over);
            }
            Err(Failure::Gone) => return self.remove(address),
            Err(Failure::Unreachable) => {
                item.failures += 1;
                if item.failures >= MAX_FAILURES {
                    return self.remove(address);
                }
                // What was being read still needs reading.
                item.wanted |= item.reading;
                item.retry = Some(now + RETRY_DELAY * u32::from(item.failures));
            }
        }
        self.publish();
    }

    fn item_changed(&mut self, sender: &str, path: &str, member: &str, now: Instant) {
        let parts = Parts::for_signal(member);
        if parts.is_empty() {
            return;
        }
        let found = self.items.iter().position(|item| {
            let owner = item
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.owner.as_str());
            item.address.path == path && (item.address.bus == sender || owner == Some(sender))
        });
        // Whatever the signal says, a new status included, is only a
        // reason to read. What is shown comes from the read, which is
        // paced; a signal that changed the screen directly would let an
        // app redraw every bar as fast as it can send.
        if let Some(index) = found {
            self.request_refresh(index, parts, now);
        }
    }

    /// The icons to draw, leftmost first. The newest item is on the left,
    /// at the outer end of the tray, so that one arriving does not move
    /// the others from under the pointer.
    fn views(&self) -> Vec<View> {
        self.items
            .iter()
            .rev()
            .filter_map(|item| {
                let snapshot = item.snapshot.as_ref()?;
                let properties = &snapshot.properties;
                item.visible.then(|| View {
                    id: item.id,
                    label: text::clean(properties.label(), text::MAX_LABEL),
                    letter: properties.letter(),
                    attention: properties.status == Status::NeedsAttention,
                    icons: snapshot.icons.clone(),
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
        let refused = self.refused(&snapshot.owner).contains(Refused::MENU);
        let has_menu = snapshot.properties.menu.is_some() && !refused;
        let is_menu = snapshot.properties.is_menu;
        match button {
            Button::Left if is_menu && has_menu => self.open_menu(id, place, now),
            Button::Left => self.activate(id, place),
            Button::Middle => self.call(id, Method::SecondaryActivate),
            Button::Right if has_menu => self.open_menu(id, place, now),
            Button::Right => self.call(id, Method::ContextMenu),
        }
    }

    /// Sends `Activate`, unless one is still on its way: an app that does
    /// not answer gets one, not one per click.
    fn activate(&mut self, id: u32, place: Place) {
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else {
            return;
        };
        if !std::mem::replace(&mut item.activating, true) {
            self.call(id, Method::Activate(place));
        }
    }

    fn scrolled(&mut self, id: u32, delta: f32, horizontal: bool, now: Instant) {
        let Some(index) = self.items.iter().position(|item| item.id == id) else {
            return;
        };
        if !delta.is_finite() {
            return;
        }
        let kept = &mut self.items[index].scroll[usize::from(horizontal)];
        *kept = (*kept + delta).clamp(-MAX_SCROLL, MAX_SCROLL);
        self.flush_scroll(index, now);
    }

    /// Sends on the scrolling that has added up to whole steps, if the
    /// item was not sent some a moment ago; otherwise leaves it for
    /// [`Host::tick`]. What is less than a step is kept for next time.
    fn flush_scroll(&mut self, index: usize, now: Instant) {
        let item = &mut self.items[index];
        if item.scroll_after.is_some_and(|after| now < after) {
            item.scroll_due = item.scroll_after;
            return;
        }
        item.scroll_due = None;
        let id = item.id;
        let mut steps = [0i32; 2];
        for (kept, whole) in item.scroll.iter_mut().zip(&mut steps) {
            *whole = kept.trunc() as i32;
            *kept -= *whole as f32;
        }
        if steps != [0, 0] {
            item.scroll_after = Some(now + SCROLL_INTERVAL);
        }
        for (delta, horizontal) in [(steps[0], false), (steps[1], true)] {
            if delta != 0 {
                self.call(id, Method::Scroll { delta, horizontal });
            }
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
        if self.refused(&owner).contains(Refused::MENU) {
            return;
        }
        self.end_menu();

        let mut pacer = Pacer::default();
        pacer.request(now);
        let serial = self.next_serial;
        self.next_serial += 1;
        self.backend.watch_menu(&bus, &path);
        self.menu = Some(MenuSession {
            serial,
            target: Target::Item { bus, owner, path },
            place,
            entries: None,
            pacer,
            about: Some(0),
        });
        self.fetch_menu();
    }

    /// Shows the items that did not fit on a bar as a menu of their names.
    fn open_overflow(&mut self, place: Place, hidden: &[u32]) {
        let entries: Vec<Entry> = hidden
            .iter()
            .filter_map(|id| {
                let (_, snapshot) = self.snapshot(*id)?;
                let properties = &snapshot.properties;
                let label = match properties.label() {
                    "" => properties.letter(),
                    label => text::clean(label, text::MAX_LABEL),
                };
                Some(Entry {
                    id: i32::try_from(*id).ok()?,
                    label,
                    enabled: true,
                    ..Entry::default()
                })
            })
            .collect();
        self.end_menu();
        if entries.is_empty() {
            return;
        }
        let serial = self.next_serial;
        self.next_serial += 1;
        self.backend.show_menu(serial, &place, &entries);
        self.menu = Some(MenuSession {
            serial,
            target: Target::Overflow,
            place,
            entries: Some(entries),
            pacer: Pacer::default(),
            about: None,
        });
    }

    fn session(&self, serial: u64) -> Option<&MenuSession> {
        self.menu.as_ref().filter(|menu| menu.serial == serial)
    }

    fn menu_request(&mut self, action: MenuAction) {
        if let Some(MenuSession {
            serial,
            target: Target::Item { bus, path, .. },
            ..
        }) = &self.menu
        {
            self.backend.menu(MenuRequest {
                serial: *serial,
                bus: bus.clone(),
                path: path.clone(),
                action,
            });
        }
    }

    /// Asks for the menu's layout. Only called when the session's pacer
    /// has said so, which is what keeps it to one request at a time.
    fn fetch_menu(&mut self) {
        let about = self.menu.as_mut().and_then(|menu| menu.about.take());
        self.menu_request(about.map_or(MenuAction::Reload, MenuAction::Show));
    }

    fn menu_changed(&mut self, sender: &str, path: &str, now: Instant) {
        let Some(session) = self.menu.as_mut() else {
            return;
        };
        let Target::Item {
            bus,
            owner,
            path: held,
        } = &session.target
        else {
            return;
        };
        let theirs = held == path && (owner == sender || bus == sender);
        if theirs && session.pacer.request(now) {
            self.fetch_menu();
        }
    }

    fn menu_layout(&mut self, serial: u64, entries: Option<Vec<Entry>>, now: Instant) {
        let Some(session) = self.menu.as_mut().filter(|menu| menu.serial == serial) else {
            return;
        };
        session.pacer.finished(now);
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
            _ if first => self.end_menu(),
            _ => {}
        }
    }

    fn menu_opened(&mut self, serial: u64, id: i32, now: Instant) {
        let Some(session) = self.menu.as_mut().filter(|menu| menu.serial == serial) else {
            return;
        };
        let known = session.entries.as_deref().unwrap_or_default();
        if !menu::find(known, id).is_some_and(|entry| entry.submenu) {
            return;
        }
        session.about = Some(id);
        if session.pacer.request(now) {
            self.fetch_menu();
        }
    }

    fn menu_clicked(&mut self, serial: u64, id: i32, now: Instant) {
        let Some(session) = self.session(serial) else {
            return;
        };
        // Only a row we showed, and only one that can be chosen.
        let known = session.entries.as_deref().unwrap_or_default();
        let valid = menu::find(known, id).is_some_and(|entry| entry.enabled && !entry.submenu);
        let overflow = matches!(session.target, Target::Overflow);
        let place = session.place.clone();
        if valid && !overflow {
            self.menu_request(MenuAction::Click(id));
        }
        self.end_menu();
        if let (true, true, Ok(id)) = (valid, overflow, u32::try_from(id)) {
            // The hidden item's own menu if it has one, as most that end
            // up here are indicators; otherwise what a click would do.
            let has_menu = self
                .snapshot(id)
                .is_some_and(|(_, snapshot)| snapshot.properties.menu.is_some());
            if has_menu {
                self.open_menu(id, place, now);
            } else {
                self.activate(id, place);
            }
        }
    }

    fn end_menu(&mut self) {
        if matches!(
            self.menu,
            Some(MenuSession {
                target: Target::Item { .. },
                ..
            })
        ) {
            self.menu_request(MenuAction::Closed);
        }
        if let Some(session) = self.menu.take() {
            if let Target::Item { bus, path, .. } = &session.target {
                self.backend.unwatch_menu(bus, path);
            }
            self.backend.close_menu();
        }
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
