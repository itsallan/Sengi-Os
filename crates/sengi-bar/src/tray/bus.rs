//! The tray's side of the session bus.
//!
//! None of this runs on the UI thread. The tray thread owns the [`Host`]
//! and sleeps until an event arrives. A second thread forwards bus signals
//! to it. The calls that wait for an answer are made by two small pools of
//! workers, because the other end is some app that may be busy or stuck:
//! one pool reads items, the other carries out activations and menu
//! requests, so that reads cannot delay what the user asked for. The host
//! gives any one item at most one job of each kind at a time, so a dead
//! app holds one worker of a pool and the rest carry on.
//!
//! Every call has a timeout and every job a budget for all of its calls
//! together. Calls whose answer nobody needs are sent without waiting.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use enumflags2::BitFlags;
use futures_lite::FutureExt;
use zbus::address::transport::{Transport, UnixSocket};
use zbus::blocking::{connection, Connection, MessageIterator};
use zbus::export::serde::de::{DeserializeOwned, SeqAccess, Visitor};
use zbus::export::serde::{Deserialize, Deserializer, Serialize};
use zbus::fdo::RequestNameReply;
use zbus::message::Flags;
use zbus::zvariant::{
    DeserializeValue, DynamicType, OwnedObjectPath, Signature, Structure, Type, Value,
};

use super::flow::{Budget, Latest};
use super::host::{
    Backend, Call, Event, Failure, Host, MenuAction, MenuRequest, Method, Place, Refresh, Snapshot,
    View,
};
use super::icons::{Cache, Decoder, DECODE_TIMEOUT, THIS_PROGRAM};
use super::item::{self, IconSource, Parts, Raw, Refused, Status, Which};
use super::menu::{self, Entry};
use super::menu_view;
use super::pixmap::{self, Pixels, Pixmap, MAX_PIXMAPS};
use super::watcher::{
    Address, FreedesktopWatcher, KdeWatcher, Registry, Shared, FDO_WATCHER, KDE_WATCHER, MAX_ITEMS,
};

const KDE_ITEM: &str = "org.kde.StatusNotifierItem";
const FDO_ITEM: &str = "org.freedesktop.StatusNotifierItem";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const DBUSMENU: &str = "com.canonical.dbusmenu";
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// How long any one call to an app may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// How long reading an item may take, all of its calls and its icon
/// together. Without this, an app that answers each of a dozen calls just
/// inside the timeout would hold a worker for most of half a minute.
const READ_BUDGET: Duration = Duration::from_secs(3);

/// The same for fetching a menu.
const MENU_BUDGET: Duration = Duration::from_secs(3);

/// Largest replies we will take. These are what the job needs with room
/// to spare: a few lines of text, pixmaps of up to 256 pixels a side, a
/// menu of a couple of hundred rows. A reply over its limit is not
/// unpacked, and that property is not asked of that connection again:
/// asking once has already cost us the whole reply, and it would cost
/// that on every change. The item stays, without the part.
const TEXT_LIMIT: usize = 16 * 1024;
const PIXMAP_LIMIT: usize = 2 * 1024 * 1024;
const MENU_LIMIT: usize = 512 * 1024;

/// Messages from this size up are followed by handing freed memory back
/// to the system.
const LARGE_REPLY: usize = 256 * 1024;

/// Allocations from this size up get a mapping of their own, which goes
/// back to the system the moment it is freed. Above anything the bar
/// allocates in ordinary running, drawing included; below the replies we
/// refuse.
const OWN_MAPPING: usize = 1024 * 1024;

/// Threads that read items, and threads that act on them.
const READERS: usize = 3;
const ACTORS: usize = 2;

/// Activations and menu requests that may wait for a worker. The host
/// allows each item one of each at a time, so this is never reached by
/// one app alone.
const MAX_QUEUED_ACTIONS: usize = 8;

/// Longest line a menu helper may say.
const MAX_HELPER_LINE: u64 = 64;

/// Menus that may wait to be written to a helper.
const MAX_QUEUED_MENUS: usize = 2;

enum Job {
    Refresh(Box<Refresh>),
    Activate(Call),
    Menu(MenuRequest),
    Adopt,
}

/// Where the tray leaves what the bars should show, and how it says that
/// something is there.
pub struct Outbox {
    pub views: Latest<Vec<View>>,
    pub wake: sengi_layer::Sender<()>,
}

/// Starts the tray. Events for it go into the returned sender; what the
/// bars should show comes back through `outbox`. If there is no session
/// bus the bar simply has no tray.
pub fn start(outbox: Outbox, pixels: Vec<u32>) -> Sender<Event> {
    let (events, inbox) = mpsc::channel();
    let sender = events.clone();
    let spawned = thread::Builder::new().name("tray".into()).spawn(move || {
        if let Err(error) = run(events, inbox, outbox, pixels) {
            eprintln!("sengi-bar: no system tray: {error}");
        }
    });
    if let Err(error) = spawned {
        eprintln!("sengi-bar: no system tray: {error}");
    }
    sender
}

/// The unique name of whoever owns the watcher name when it is not us.
/// Only that connection is believed about which items exist.
type WatcherOwner = Arc<Mutex<Option<String>>>;

fn run(
    events: Sender<Event>,
    inbox: Receiver<Event>,
    outbox: Outbox,
    pixels: Vec<u32>,
) -> Result<(), Box<dyn std::error::Error>> {
    pin_mmap_threshold();
    let registry = Arc::new(Mutex::new(Registry::default()));
    let shared = Shared {
        registry: registry.clone(),
        events: events.clone(),
    };
    let connection = session()?
        .method_timeout(CALL_TIMEOUT)
        .serve_at(WATCHER_PATH, KdeWatcher(shared.clone()))?
        .serve_at(WATCHER_PATH, FreedesktopWatcher(shared))?
        .build()?;
    let unique = connection
        .unique_name()
        .map(|name| name.to_string())
        .unwrap_or_default();
    let watcher: WatcherOwner = Arc::default();

    // Listening starts before anything is asked for, so nothing is missed.
    let signals = MessageIterator::from(&connection);
    {
        let (events, unique, watcher) = (events.clone(), unique.clone(), watcher.clone());
        thread::Builder::new()
            .name("tray-signals".into())
            .spawn(move || forward_signals(signals, &unique, &watcher, &events))?;
    }
    set_match(&connection, true, &owner_rule(KDE_WATCHER));
    set_match(
        &connection,
        true,
        &format!("type='signal',interface='{KDE_WATCHER}'"),
    );

    let icons = Arc::new(IconLoader::default());
    let (reads, read_jobs) = mpsc::channel();
    let (actions, action_jobs) = mpsc::sync_channel(MAX_QUEUED_ACTIONS);
    for (name, count, jobs) in [
        ("tray-reads", READERS, read_jobs),
        ("tray-actions", ACTORS, action_jobs),
    ] {
        let jobs = Arc::new(Mutex::new(jobs));
        for _ in 0..count {
            let worker = Worker {
                connection: connection.clone(),
                events: events.clone(),
                icons: icons.clone(),
                watcher: watcher.clone(),
            };
            let jobs = jobs.clone();
            thread::Builder::new()
                .name(name.into())
                .spawn(move || worker.run(&jobs))?;
        }
    }

    // With no flags at all: we do not push another watcher out, nobody
    // pushes us out, and if the name is taken we wait in line for it, so
    // the bus hands it to us when its owner leaves. (The default set of
    // flags is not the empty one.)
    let ours = matches!(
        connection.request_name_with_flags(KDE_WATCHER, BitFlags::empty()),
        Ok(RequestNameReply::PrimaryOwner)
    );
    let _ = connection.request_name_with_flags(FDO_WATCHER, BitFlags::empty());
    let host_name = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    let _ = connection.request_name_with_flags(
        host_name.as_str(),
        zbus::fdo::RequestNameFlags::DoNotQueue.into(),
    );

    let backend = Bus {
        connection,
        reads,
        actions,
        outbox,
        events,
        helper: None,
    };
    let mut host = Host::new(backend, registry, pixels);
    if !ours {
        host.handle(Event::WatcherOwner { ours }, Instant::now());
    }
    loop {
        let event = match host.deadline() {
            None => inbox.recv().ok(),
            Some(deadline) => {
                match inbox.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            }
        };
        let now = Instant::now();
        match event {
            Some(event) => host.handle(event, now),
            None if host.deadline().is_none() => return Ok(()),
            None => {}
        }
        host.tick(now);
    }
}

/// Opens the socket to the session bus ourselves.
///
/// Left to zbus, the connect runs on a thread of the `blocking` crate's
/// pool, and with blocking 1.7.0 that thread never ends: it wakes twice a
/// second for as long as the process lives. A socket that is already
/// connected keeps the pool from ever starting. (sengi-notify has the same
/// function; it belongs in a shared crate.)
fn session_socket() -> zbus::Result<UnixStream> {
    let address = zbus::Address::session()?;
    let Transport::Unix(unix) = address.transport() else {
        return Err(zbus::Error::Unsupported);
    };
    let stream = match unix.path() {
        UnixSocket::File(path) => UnixStream::connect(path)?,
        UnixSocket::Abstract(name) => {
            UnixStream::connect_addr(&SocketAddr::from_abstract_name(name.as_encoded_bytes())?)?
        }
        _ => return Err(zbus::Error::Unsupported),
    };
    Ok(stream)
}

/// A builder for a connection to the session bus.
fn session() -> zbus::Result<connection::Builder<'static>> {
    match session_socket() {
        Ok(socket) => Ok(connection::Builder::async_io_unix_stream(socket)),
        // A bus that is not on a unix socket, or not there at all: let
        // zbus try, and report the error in its own words if it fails.
        Err(_) => connection::Builder::session(),
    }
}

fn owner_rule(name: &str) -> String {
    format!(
        "type='signal',sender='{DBUS}',interface='{DBUS}',member='NameOwnerChanged',arg0='{name}'"
    )
}

/// Adds or removes a match rule. Names and paths in rules have been
/// validated, so they cannot break out of the quotes.
fn set_match(connection: &Connection, add: bool, rule: &str) {
    let method = if add { "AddMatch" } else { "RemoveMatch" };
    let _ = connection.call_method(Some(DBUS), DBUS_PATH, Some(DBUS), method, &rule);
}

/// Calls a method and waits for the answer, for no longer than `limit`.
fn call<B: Serialize + DynamicType>(
    connection: &Connection,
    (bus, path, interface): (&str, &str, &str),
    method: &str,
    body: &B,
    limit: Duration,
) -> zbus::Result<zbus::Message> {
    let answer = connection
        .inner()
        .call_method(Some(bus), path, Some(interface), method, body);
    let clock = async {
        async_io::Timer::after(limit).await;
        Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into())
    };
    async_io::block_on(answer.or(clock))
}

/// Calls a method and does not wait: the app is told not to answer, and
/// whether it is alive to hear makes no difference to us.
fn send<B: Serialize + DynamicType>(
    connection: &Connection,
    (bus, path, interface): (&str, &str, &str),
    method: &str,
    body: &B,
) {
    let message = zbus::Message::method_call(path, method)
        .and_then(|message| message.destination(bus))
        .and_then(|message| message.interface(interface))
        .and_then(|message| message.with_flags(Flags::NoReplyExpected))
        .and_then(|message| message.build(body));
    if let Ok(message) = message {
        let _ = connection.send(&message);
    }
}

/// Tells a menu about something that happened to one of its rows. The
/// data and the timestamp have no use for the events we send.
fn menu_event(connection: &Connection, request: &MenuRequest, id: i32, name: &str) {
    let menu = (request.bus.as_str(), request.path.as_str(), DBUSMENU);
    send(
        connection,
        menu,
        "Event",
        &(id, name, Value::from(0i32), 0u32),
    );
}

/// Turns the signals we subscribed to into events. Bodies are tiny or are
/// not looked at.
fn forward_signals(
    signals: MessageIterator,
    unique: &str,
    watcher: &WatcherOwner,
    events: &Sender<Event>,
) {
    let mut large = false;
    for message in signals {
        // Every message passes through here, replies included, and this
        // is where the last hold on one is often let go.
        if std::mem::take(&mut large) {
            trim_heap();
        }
        let Ok(message) = message else {
            return;
        };
        let body = message.body();
        large = body.len() > LARGE_REPLY;
        let header = message.header();
        // Megabytes nobody asked for: a signal, or a call to us. (An
        // answer that is too big is the business of whoever asked, and
        // costs the app that property, not its place in the tray.)
        let reply = matches!(
            header.message_type(),
            zbus::message::Type::MethodReturn | zbus::message::Type::Error
        );
        if body.len() > PIXMAP_LIMIT && !reply {
            if let Some(owner) = header.sender().filter(|name| name.as_str() != DBUS) {
                let owner = owner.to_string();
                if events.send(Event::Misbehaved { owner }).is_err() {
                    return;
                }
            }
            continue;
        }
        if header.message_type() != zbus::message::Type::Signal {
            continue;
        }
        let (Some(interface), Some(member), Some(path)) =
            (header.interface(), header.member(), header.path())
        else {
            continue;
        };
        let sender = header
            .sender()
            .map(|name| name.as_str())
            .unwrap_or_default();
        let small = body.len() <= TEXT_LIMIT;
        let text = || small.then(|| body.deserialize::<String>().ok()).flatten();

        let event = match (interface.as_str(), member.as_str()) {
            (DBUS, "NameOwnerChanged") if sender == DBUS && small => {
                let Ok((name, _old, new)) = body.deserialize::<(String, String, String)>() else {
                    continue;
                };
                if name == KDE_WATCHER {
                    let ours = new == unique;
                    if let Ok(mut owner) = watcher.lock() {
                        *owner = (!ours && !new.is_empty()).then(|| new.clone());
                    }
                    if new.is_empty() {
                        continue;
                    }
                    Event::WatcherOwner { ours }
                } else if new.is_empty() {
                    Event::OwnerLost(name)
                } else {
                    Event::OwnerChanged { name, owner: new }
                }
            }
            (KDE_ITEM | FDO_ITEM, member) => Event::Signal {
                sender: sender.to_string(),
                path: path.to_string(),
                member: member.to_string(),
                menu: false,
            },
            (DBUSMENU, member) => Event::Signal {
                sender: sender.to_string(),
                path: path.to_string(),
                member: member.to_string(),
                menu: true,
            },
            // Anyone can emit a signal that looks like a watcher's. Only
            // the connection that owns the watcher name is believed.
            (KDE_WATCHER, member) if believed(watcher, sender) => {
                let Some(service) = text() else {
                    continue;
                };
                match member {
                    "StatusNotifierItemRegistered" => Event::Announced(service),
                    "StatusNotifierItemUnregistered" => Event::Withdrawn(service),
                    _ => continue,
                }
            }
            _ => continue,
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

/// Whether `sender` is the watcher we are a guest of.
fn believed(watcher: &WatcherOwner, sender: &str) -> bool {
    !sender.is_empty()
        && watcher
            .lock()
            .is_ok_and(|owner| owner.as_deref() == Some(sender))
}

/// Whether an error means the item is gone or stuck. None is an ordinary
/// refusal: no such property, no such method.
fn failure(error: &zbus::Error) -> Option<Failure> {
    let zbus::Error::MethodError(name, _, _) = error else {
        return Some(Failure::Unreachable);
    };
    classify(name.as_str())
}

fn classify(error_name: &str) -> Option<Failure> {
    match error_name.strip_prefix("org.freedesktop.DBus.Error.") {
        Some("ServiceUnknown" | "NameHasNoOwner") => Some(Failure::Gone),
        Some("NoReply" | "Timeout" | "TimedOut" | "Disconnected") => Some(Failure::Unreachable),
        _ => None,
    }
}

/// An `IconPixmap` array, of which only the first few entries that could
/// be drawn are kept. The rest are passed over where they lie in the
/// reply; nothing is copied or collected for them.
struct PixmapList(Vec<Pixmap>);

impl Type for PixmapList {
    const SIGNATURE: &'static Signature = <Vec<(i32, i32, Vec<u8>)> as Type>::SIGNATURE;
}

impl<'de> Deserialize<'de> for PixmapList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;

        impl<'de> Visitor<'de> for Entries {
            type Value = PixmapList;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array of pixmaps")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut entries: A) -> Result<PixmapList, A::Error> {
                let mut kept = Vec::new();
                let mut seen = 0;
                while let Some((width, height, data)) =
                    entries.next_element::<(i32, i32, &'de [u8])>()?
                {
                    seen += 1;
                    if seen <= MAX_PIXMAPS && pixmap::plausible(width, height, data.len()) {
                        kept.push(Pixmap {
                            width,
                            height,
                            data: data.to_vec(),
                        });
                    }
                }
                Ok(PixmapList(kept))
            }
        }

        deserializer.deserialize_seq(Entries)
    }
}

type ToolTip = (String, PixmapList, String, String);

/// Reads an item's properties one at a time. `GetAll` would be one call
/// instead of several, but then a single over-sized or mistyped property
/// would cost all the others, and everything would be fetched when one
/// thing changed.
struct Reader<'a> {
    connection: &'a Connection,
    address: &'a Address,
    interface: &'static str,
    icons: &'a IconLoader,
    budget: Budget,
    /// The unique name the answers came from.
    owner: Option<String>,
    /// Whether any answer was big enough to be worth tidying up after.
    large: bool,
    /// What this connection is not asked for any more.
    refused: Refused,
    /// Answers in this read that were over their limit.
    over: Refused,
    /// Pixmaps fetched during this read, so two sizes cost one fetch.
    fetched: [Option<Vec<Pixmap>>; 2],
}

impl Reader<'_> {
    /// The reply to `Get`. None is a property the item does not have.
    fn reply(&mut self, property: &str, limit: usize) -> Result<Option<zbus::Message>, Failure> {
        let allowed = self
            .budget
            .next(Instant::now(), CALL_TIMEOUT)
            .ok_or(Failure::Unreachable)?;
        let item = (
            self.address.bus.as_str(),
            self.address.path.as_str(),
            PROPERTIES,
        );
        let reply = call(
            self.connection,
            item,
            "Get",
            &(self.interface, property),
            allowed,
        );
        let message = match reply {
            Ok(message) => message,
            Err(error) => return failure(&error).map_or(Ok(None), Err),
        };
        if self.owner.is_none() {
            self.owner = message.header().sender().map(|name| name.to_string());
        }
        let size = message.body().len();
        self.large |= size > LARGE_REPLY;
        if size > limit {
            // Dropped unread, and noted for the host to hold against the
            // connection.
            self.over |= Refused::for_property(property);
            return Ok(None);
        }
        Ok(Some(message))
    }

    fn get<T>(&mut self, property: &str, limit: usize) -> Result<Option<T>, Failure>
    where
        T: DeserializeOwned + Type,
    {
        let Some(message) = self.reply(property, limit)? else {
            return Ok(None);
        };
        Ok(message
            .body()
            .deserialize::<DeserializeValue<'_, T>>()
            .ok()
            .map(|value| value.0))
    }

    fn text(&mut self, property: &str) -> Result<Option<String>, Failure> {
        self.get(property, TEXT_LIMIT)
    }
}

impl IconSource for Reader<'_> {
    type Error = Failure;

    fn named(
        &mut self,
        name: &str,
        theme_path: &str,
        pixels: u32,
    ) -> Result<Option<Pixels>, Failure> {
        self.icons.named(name, theme_path, pixels, &self.budget)
    }

    fn pixmaps(&mut self, which: Which) -> Result<Vec<Pixmap>, Failure> {
        let (slot, property) = match which {
            Which::Icon => (0, "IconPixmap"),
            Which::Attention => (1, "AttentionIconPixmap"),
        };
        if self.refused.contains(Refused::for_property(property)) {
            return Ok(Vec::new());
        }
        if self.fetched[slot].is_none() {
            let list = match self.reply(property, PIXMAP_LIMIT)? {
                Some(message) => message
                    .body()
                    .deserialize::<DeserializeValue<'_, PixmapList>>()
                    .map_or_else(|_| Vec::new(), |list| list.0 .0),
                None => Vec::new(),
            };
            self.fetched[slot] = Some(list);
        }
        Ok(self.fetched[slot].clone().unwrap_or_default())
    }
}

fn read_item(
    connection: &Connection,
    request: &Refresh,
    icons: &IconLoader,
) -> Result<Snapshot, Failure> {
    let previous = request.previous.as_ref();
    let mut reader = Reader {
        connection,
        address: &request.address,
        interface: previous.map_or(KDE_ITEM, |snapshot| snapshot.interface),
        icons,
        budget: Budget::starting(Instant::now(), READ_BUDGET),
        owner: None,
        large: false,
        refused: request.refused,
        over: Refused::NONE,
        fetched: [None, None],
    };
    let snapshot = read_parts(&mut reader, request);
    if reader.large {
        trim_heap();
    }
    snapshot
}

fn read_parts(reader: &mut Reader<'_>, request: &Refresh) -> Result<Snapshot, Failure> {
    let previous = request.previous.as_ref();
    let parts = if previous.is_some() {
        request.parts
    } else {
        Parts::ALL
    };
    let mut raw = Raw::default();
    if parts.contains(Parts::IDENTITY) {
        raw.id = reader.text("Id")?;
        if raw.id.is_none() && previous.is_none() {
            // Perhaps it speaks the freedesktop draft's interface instead.
            reader.interface = FDO_ITEM;
            raw.id = reader.text("Id")?;
            if raw.id.is_none() {
                reader.interface = KDE_ITEM;
            }
        }
        raw.is_menu = reader.get("ItemIsMenu", TEXT_LIMIT)?;
        raw.menu = reader
            .get::<OwnedObjectPath>("Menu", TEXT_LIMIT)?
            .map(|path| path.to_string());
    }
    if parts.contains(Parts::STATUS) {
        raw.status = reader.text("Status")?;
    }
    if parts.contains(Parts::TITLE) {
        raw.title = reader.text("Title")?;
    }
    if parts.contains(Parts::TOOLTIP) && !reader.refused.contains(Refused::TOOLTIP) {
        // A tooltip may carry an icon of its own, which is not used.
        raw.tooltip = reader
            .get::<ToolTip>("ToolTip", PIXMAP_LIMIT)?
            .map(|(_icon, _pixmaps, title, body)| (title, body));
    }
    if parts.contains(Parts::ICON) {
        raw.icon_theme_path = reader.text("IconThemePath")?;
        raw.icon_name = reader.text("IconName")?;
    }
    if parts.contains(Parts::ATTENTION_ICON) {
        raw.attention_icon_name = reader.text("AttentionIconName")?;
    }
    if parts.contains(Parts::OVERLAY_ICON) {
        raw.overlay_icon_name = reader.text("OverlayIconName")?;
    }
    if previous.is_none() && raw.is_empty() {
        return Err(Failure::Unreachable);
    }

    let mut properties = previous
        .map(|snapshot| snapshot.properties.clone())
        .unwrap_or_default();
    let attention = |status| status == Status::NeedsAttention;
    let was_attention = attention(properties.status);
    properties.update(parts, raw);

    // The picture is drawn again only if something that goes into it
    // changed: an icon, or which icon the status picks. The host asks for
    // the icons again when the sizes change.
    let held = previous.map(|snapshot| &snapshot.icons);
    let redraw = parts.intersects(Parts::ICONS) || attention(properties.status) != was_attention;
    let icons = match held {
        Some(held) if !redraw => held.clone(),
        _ => {
            let mut icons = Vec::new();
            for pixels in &request.pixels {
                if let Some(icon) = item::icon(&properties, *pixels, reader)? {
                    icons.push((*pixels, Arc::new(icon)));
                }
            }
            icons
        }
    };
    let owner = reader.owner.take();
    Ok(Snapshot {
        owner: owner
            .or_else(|| previous.map(|snapshot| snapshot.owner.clone()))
            .unwrap_or_else(|| reader.address.bus.clone()),
        interface: reader.interface,
        properties,
        icons,
        over: reader.over,
    })
}

/// Sends `Activate` and waits to learn whether the item has one.
fn activate(connection: &Connection, request: Call) -> Event {
    let item = (
        request.bus.as_str(),
        request.path.as_str(),
        request.interface,
    );
    // Where the click was on screen is not ours to know under Wayland.
    let result = call(connection, item, "Activate", &(0i32, 0i32), CALL_TIMEOUT);
    let refused = result.is_err_and(|error| failure(&error).is_none());
    let fallback = match request.method {
        Method::Activate(place) if refused => Some(place),
        _ => None,
    };
    Event::Activated {
        id: request.id,
        fallback,
    }
}

/// Tells the app a row is about to show, if asked to, and reads the
/// menu's layout.
fn read_menu(connection: &Connection, request: &MenuRequest, events: &Sender<Event>) -> Event {
    let budget = Budget::starting(Instant::now(), MENU_BUDGET);
    let menu = (request.bus.as_str(), request.path.as_str(), DBUSMENU);
    let allowed = || budget.next(Instant::now(), CALL_TIMEOUT);
    if let (MenuAction::Show(id), Some(limit)) = (request.action, allowed()) {
        let _ = call(connection, menu, "AboutToShow", &id, limit);
        if id != 0 {
            menu_event(connection, request, id, "opened");
        }
    }
    let wanted = (0i32, menu::MAX_DEPTH as i32, menu::PROPERTIES.as_slice());
    let layout =
        allowed().and_then(|limit| call(connection, menu, "GetLayout", &wanted, limit).ok());
    let entries = layout.and_then(|message| {
        let body = message.body();
        if body.len() > MENU_LIMIT {
            // Unpacking it would cost many times its size.
            if let Some(owner) = message.header().sender() {
                let _ = events.send(Event::Oversized {
                    owner: owner.to_string(),
                    what: Refused::MENU,
                });
            }
            return None;
        }
        let reply: Structure<'_> = body.deserialize().ok()?;
        Some(menu::parse(reply.fields().get(1)?))
    });
    trim_heap();
    Event::MenuLayout {
        serial: request.serial,
        entries,
    }
}

/// Unpacking a big reply leaves the allocator holding freed memory; give
/// it back, so one huge reply does not stay on the bar's books for good.
fn trim_heap() {
    #[cfg(target_env = "gnu")]
    // SAFETY: malloc_trim takes no pointers and may be called at any time.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Keeps the biggest buffers out of the heap. A reply is held in one
/// allocation while it is looked at, and zbus has taken it whole before we
/// can refuse it. glibc gives a big allocation a mapping of its own and
/// returns it when freed, but it also raises the size from which it does
/// so each time one is freed, up to 32 MB, and from then on keeps them: a
/// 16 MB reply stayed on the bar's books for good, trimming or not.
/// Naming the size switches that adjustment off.
fn pin_mmap_threshold() {
    #[cfg(target_env = "gnu")]
    // SAFETY: mallopt takes two integers and may be called at any time.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, OWN_MAPPING as libc::c_int);
    }
}

/// Registers as host with the watcher that is already there and asks it
/// who it is and what it has.
fn adopt(connection: &Connection, watcher: &WatcherOwner, events: &Sender<Event>) {
    let bus = (DBUS, DBUS_PATH, DBUS);
    let owner = call(connection, bus, "GetNameOwner", &KDE_WATCHER, CALL_TIMEOUT)
        .ok()
        .and_then(|reply| reply.body().deserialize::<String>().ok());
    let Some(owner) = owner else {
        return;
    };
    let ours = connection
        .unique_name()
        .map(|name| name.to_string())
        .unwrap_or_default();
    if owner == ours {
        return;
    }
    if let Ok(mut held) = watcher.lock() {
        *held = Some(owner.clone());
    }
    // Asked of the owner by its unique name, so the answer is its own.
    let there = (owner.as_str(), WATCHER_PATH, KDE_WATCHER);
    let _ = call(
        connection,
        there,
        "RegisterStatusNotifierHost",
        &ours,
        CALL_TIMEOUT,
    );
    let listed = call(
        connection,
        (owner.as_str(), WATCHER_PATH, PROPERTIES),
        "Get",
        &(KDE_WATCHER, "RegisteredStatusNotifierItems"),
        CALL_TIMEOUT,
    );
    let Ok(message) = listed else {
        return;
    };
    let body = message.body();
    if body.len() > MENU_LIMIT {
        return;
    }
    let Ok(services) = body.deserialize::<DeserializeValue<'_, Vec<String>>>() else {
        return;
    };
    for service in services.0.into_iter().take(MAX_ITEMS * 2) {
        let _ = events.send(Event::Announced(service));
    }
}

/// Finds icon files and has them decoded in a child process, remembering
/// the answers for a while. Shared by the readers.
#[derive(Default)]
struct IconLoader {
    decoder: Decoder,
    cache: Mutex<Cache>,
}

impl IconLoader {
    fn named(
        &self,
        name: &str,
        theme_path: &str,
        pixels: u32,
        budget: &Budget,
    ) -> Result<Option<Pixels>, Failure> {
        let now = Instant::now();
        // The cache is asked before anything else: an item that goes back
        // and forth between two icons costs nothing after the first lap.
        if let Some(known) = self
            .cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(name, theme_path, pixels, now))
        {
            return Ok(known);
        }
        let allowed = budget
            .next(now, DECODE_TIMEOUT)
            .ok_or(Failure::Unreachable)?;
        let icon = self.decoder.decode(name, theme_path, pixels, allowed);
        if let Ok(mut cache) = self.cache.lock() {
            cache.put(name, theme_path, pixels, icon.clone(), Instant::now());
        }
        Ok(icon)
    }
}

/// One of the threads that wait for apps.
struct Worker {
    connection: Connection,
    events: Sender<Event>,
    icons: Arc<IconLoader>,
    watcher: WatcherOwner,
}

impl Worker {
    fn run(&self, jobs: &Mutex<Receiver<Job>>) {
        loop {
            // The lock is held only while waiting for a job, not while
            // doing one.
            let job = match jobs.lock() {
                Ok(jobs) => jobs.recv(),
                Err(_) => return,
            };
            let event = match job {
                Ok(Job::Refresh(request)) => {
                    let outcome = read_item(&self.connection, &request, &self.icons);
                    Event::Refreshed {
                        address: request.address,
                        outcome: outcome.map(Box::new),
                    }
                }
                Ok(Job::Activate(call)) => activate(&self.connection, call),
                Ok(Job::Menu(request)) => read_menu(&self.connection, &request, &self.events),
                Ok(Job::Adopt) => {
                    adopt(&self.connection, &self.watcher, &self.events);
                    continue;
                }
                Err(_) => return,
            };
            if self.events.send(event).is_err() {
                return;
            }
        }
    }
}

/// The process drawing the menu that is on screen.
struct Helper {
    child: Child,
    /// Menus on their way to the helper's standard input.
    menus: SyncSender<String>,
}

struct Bus {
    connection: Connection,
    reads: Sender<Job>,
    actions: SyncSender<Job>,
    outbox: Outbox,
    events: Sender<Event>,
    helper: Option<Helper>,
}

impl Bus {
    fn item_rule(address: &Address) -> String {
        format!(
            "type='signal',sender='{}',path='{}'",
            address.bus, address.path
        )
    }

    fn menu_rule(bus: &str, path: &str) -> String {
        format!("type='signal',sender='{bus}',path='{path}',interface='{DBUSMENU}'")
    }

    fn spawn_helper(&self, serial: u64, place: &Place) -> std::io::Result<Helper> {
        let mut child = Command::new(THIS_PROGRAM)
            .arg(menu_view::FLAG)
            .arg(&place.output)
            .arg(place.x.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::ErrorKind::BrokenPipe.into());
        };

        // A menu can be bigger than a pipe holds and a helper can stop
        // reading, so writing has a thread of its own that may wait.
        let (menus, queued) = mpsc::sync_channel::<String>(MAX_QUEUED_MENUS);
        let writer = thread::Builder::new()
            .name("tray-menu-out".into())
            .spawn(move || {
                for text in queued {
                    if stdin.write_all(text.as_bytes()).is_err() {
                        return;
                    }
                }
            });

        let events = self.events.clone();
        let reader = thread::Builder::new()
            .name("tray-menu-in".into())
            .spawn(move || {
                let mut reports = BufReader::new(stdout);
                loop {
                    // The length of a line is capped, not how many there
                    // are: a menu may stay open for as long as it likes.
                    let mut line = String::new();
                    match (&mut reports).take(MAX_HELPER_LINE).read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let event = match menu_view::parse_report(line.trim_end()) {
                        Some(menu_view::Report::Clicked(id)) => Event::MenuClicked { serial, id },
                        Some(menu_view::Report::Opened(id)) => Event::MenuOpened { serial, id },
                        None => continue,
                    };
                    let _ = events.send(event);
                }
                let _ = events.send(Event::MenuClosed { serial });
            });
        // A helper nobody is writing to or listening to is not left
        // running. Killing it ends whichever thread did start.
        if let Err(error) = writer.and(reader) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Helper { child, menus })
    }
}

impl Backend for Bus {
    fn watch(&mut self, address: &Address) {
        set_match(&self.connection, true, &owner_rule(&address.bus));
        set_match(&self.connection, true, &Self::item_rule(address));
    }

    fn unwatch(&mut self, address: &Address) {
        set_match(&self.connection, false, &owner_rule(&address.bus));
        set_match(&self.connection, false, &Self::item_rule(address));
    }

    fn watch_owner(&mut self, name: &str) {
        set_match(&self.connection, true, &owner_rule(name));
    }

    fn unwatch_owner(&mut self, name: &str) {
        set_match(&self.connection, false, &owner_rule(name));
    }

    fn refresh(&mut self, request: Refresh) {
        let _ = self.reads.send(Job::Refresh(Box::new(request)));
    }

    fn call(&mut self, call: Call) {
        let item = (call.bus.as_str(), call.path.as_str(), call.interface);
        let origin = (0i32, 0i32);
        match &call.method {
            Method::SecondaryActivate => send(&self.connection, item, "SecondaryActivate", &origin),
            Method::ContextMenu => send(&self.connection, item, "ContextMenu", &origin),
            Method::Scroll { delta, horizontal } => {
                let orientation = if *horizontal {
                    "horizontal"
                } else {
                    "vertical"
                };
                send(&self.connection, item, "Scroll", &(*delta, orientation));
            }
            Method::Activate(_) => {
                let id = call.id;
                if self.actions.try_send(Job::Activate(call)).is_err() {
                    // The host is waiting to hear that this one is over.
                    let _ = self.events.send(Event::Activated { id, fallback: None });
                }
            }
        }
    }

    fn unregistered(&mut self, address: &Address) {
        let service = address.service();
        for watcher in [KDE_WATCHER, FDO_WATCHER] {
            let _ = self.connection.emit_signal(
                None::<&str>,
                WATCHER_PATH,
                watcher,
                "StatusNotifierItemUnregistered",
                &service,
            );
        }
    }

    fn adopt_watcher(&mut self) {
        let _ = self.reads.send(Job::Adopt);
    }

    fn publish(&mut self, views: Vec<View>) {
        if self.outbox.views.put(views) {
            let _ = self.outbox.wake.send(());
        }
    }

    fn watch_menu(&mut self, bus: &str, path: &str) {
        set_match(&self.connection, true, &Self::menu_rule(bus, path));
    }

    fn unwatch_menu(&mut self, bus: &str, path: &str) {
        set_match(&self.connection, false, &Self::menu_rule(bus, path));
    }

    fn menu(&mut self, request: MenuRequest) {
        match request.action {
            MenuAction::Click(id) => menu_event(&self.connection, &request, id, "clicked"),
            MenuAction::Closed => menu_event(&self.connection, &request, 0, "closed"),
            MenuAction::Show(_) | MenuAction::Reload => {
                let serial = request.serial;
                if self.actions.try_send(Job::Menu(request)).is_err() {
                    // The host is waiting for an answer to this one.
                    let _ = self.events.send(Event::MenuLayout {
                        serial,
                        entries: None,
                    });
                }
            }
        }
    }

    fn show_menu(&mut self, serial: u64, place: &Place, entries: &[Entry]) {
        self.close_menu();
        match self.spawn_helper(serial, place) {
            Ok(helper) => {
                self.helper = Some(helper);
                self.update_menu(entries);
            }
            Err(error) => {
                eprintln!("sengi-bar: could not open the menu: {error}");
                let _ = self.events.send(Event::MenuClosed { serial });
            }
        }
    }

    fn update_menu(&mut self, entries: &[Entry]) {
        let Some(helper) = &mut self.helper else {
            return;
        };
        // A helper that has stopped taking menus is closed. Its exit is
        // reported as usual.
        if helper.menus.try_send(menu::encode(entries)).is_err() {
            self.close_menu();
        }
    }

    fn close_menu(&mut self) {
        if let Some(mut helper) = self.helper.take() {
            let _ = helper.child.kill();
            let _ = helper.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::serialized::Context;
    use zbus::zvariant::{to_bytes, SerializeValue, LE};

    #[test]
    fn errors_are_told_apart() {
        let dbus = |name: &str| classify(&format!("org.freedesktop.DBus.Error.{name}"));
        assert_eq!(dbus("ServiceUnknown"), Some(Failure::Gone));
        assert_eq!(dbus("NameHasNoOwner"), Some(Failure::Gone));
        assert_eq!(dbus("NoReply"), Some(Failure::Unreachable));
        assert_eq!(dbus("Timeout"), Some(Failure::Unreachable));
        // No such property or method is an answer, not a failure.
        assert_eq!(dbus("UnknownProperty"), None);
        assert_eq!(dbus("UnknownMethod"), None);
        assert_eq!(dbus("InvalidArgs"), None);
        assert_eq!(classify("org.example.Error.Custom"), None);
        // A call that never came back, or a broken connection.
        let timed_out = zbus::Error::from(std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert_eq!(failure(&timed_out), Some(Failure::Unreachable));
    }

    fn pixmaps(list: &[(i32, i32, Vec<u8>)]) -> Vec<Pixmap> {
        let context = Context::new_dbus(LE, 0);
        let bytes = to_bytes(context, &SerializeValue(&list.to_vec())).unwrap();
        let list: DeserializeValue<'_, PixmapList> = bytes.deserialize().unwrap().0;
        list.0 .0
    }

    #[test]
    fn only_drawable_pixmaps_are_copied_out_of_a_reply() {
        let good = |side: i32| (side, side, vec![7u8; (side * side * 4) as usize]);
        let kept = pixmaps(&[
            // Claims a size it does not have the data for.
            (100_000, 100_000, vec![0; 4]),
            good(16),
            // Honest, and far too big for a tray.
            good(300),
            good(32),
        ]);
        let sides: Vec<i32> = kept.iter().map(|pixmap| pixmap.width).collect();
        assert_eq!(sides, [16, 32]);
        assert_eq!(kept[0].data.len(), 16 * 16 * 4);
        assert!(pixmaps(&[]).is_empty());
    }

    #[test]
    fn entries_past_the_cap_are_not_collected() {
        let many: Vec<_> = (0..MAX_PIXMAPS + 50)
            .map(|_| (1, 1, vec![9u8; 4]))
            .collect();
        assert_eq!(pixmaps(&many).len(), MAX_PIXMAPS);
        // Junk in front does not use up the places of real ones behind
        // it, but the search for them stops at the cap all the same.
        let mut mixed: Vec<_> = (0..MAX_PIXMAPS).map(|_| (5, 5, vec![0u8; 1])).collect();
        mixed.push((1, 1, vec![9u8; 4]));
        assert!(pixmaps(&mixed).is_empty());
    }

    #[test]
    fn something_that_is_not_a_pixmap_array_is_refused() {
        let context = Context::new_dbus(LE, 0);
        let bytes = to_bytes(context, &SerializeValue(&"not pixmaps")).unwrap();
        assert!(bytes
            .deserialize::<DeserializeValue<'_, PixmapList>>()
            .is_err());
    }

    #[test]
    fn only_the_watcher_we_are_a_guest_of_is_believed() {
        let watcher: WatcherOwner = Arc::default();
        // While the name is ours, nobody else speaks for it.
        assert!(!believed(&watcher, ":1.9"));
        *watcher.lock().unwrap() = Some(":1.7".into());
        assert!(believed(&watcher, ":1.7"));
        assert!(!believed(&watcher, ":1.9"));
        assert!(!believed(&watcher, ""));
    }

    #[test]
    fn match_rules_name_what_they_watch() {
        let address = Address {
            bus: ":1.42".into(),
            path: "/StatusNotifierItem".into(),
        };
        assert_eq!(
            Bus::item_rule(&address),
            "type='signal',sender=':1.42',path='/StatusNotifierItem'"
        );
        assert!(owner_rule(":1.42").ends_with("member='NameOwnerChanged',arg0=':1.42'"));
        assert!(Bus::menu_rule(":1.42", "/MenuBar").ends_with("interface='com.canonical.dbusmenu'"));
    }
}
