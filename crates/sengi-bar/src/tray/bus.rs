//! The tray's side of the session bus.
//!
//! Four threads, none of them the UI's. The tray thread owns the [`Host`]
//! and sleeps until an event arrives. A second forwards bus signals to it.
//! Two workers make the calls that can be slow, because the other end is
//! some app that may be busy or stuck: one reads items, the other carries
//! out clicks and menu requests, so that a flood of reads cannot delay
//! what the user asked for. Every call has a timeout.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use enumflags2::BitFlags;
use zbus::address::transport::{Transport, UnixSocket};
use zbus::blocking::{connection, Connection, MessageIterator};
use zbus::export::serde::de::DeserializeOwned;
use zbus::export::serde::Serialize;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::zvariant::{DeserializeValue, DynamicType, OwnedObjectPath, Structure, Type, Value};

use super::host::{
    Backend, Call, Event, Failure, Host, MenuAction, MenuRequest, Method, Place, Snapshot, View,
};
use super::icons::{self, Decoder, IconDirs};
use super::item::{self, IconFiles, Properties, Raw, Status};
use super::menu::{self, Entry};
use super::menu_view;
use super::pixmap::{self, Pixels, Pixmap, MAX_PIXMAPS};
use super::watcher::{Address, FreedesktopWatcher, KdeWatcher, Registry, Shared};

const KDE_ITEM: &str = "org.kde.StatusNotifierItem";
const FDO_ITEM: &str = "org.freedesktop.StatusNotifierItem";
const KDE_WATCHER: &str = "org.kde.StatusNotifierWatcher";
const FDO_WATCHER: &str = "org.freedesktop.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const DBUSMENU: &str = "com.canonical.dbusmenu";
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// How long any one call to an app may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// Largest replies we will unpack. A reply over its limit is dropped
/// whole, as if the property were not there.
const TEXT_LIMIT: usize = 16 * 1024;
const TOOLTIP_LIMIT: usize = 1024 * 1024;
const PIXMAP_LIMIT: usize = 8 * 1024 * 1024;
const MENU_LIMIT: usize = 4 * 1024 * 1024;

/// Replies from this size up are followed by handing freed memory back.
const LARGE_REPLY: usize = 256 * 1024;

/// Clicks and scroll steps that may wait for the worker. Beyond this the
/// app is not keeping up and more would only pile on.
const MAX_QUEUED_ACTIONS: usize = 32;

/// Most a menu helper may say before it is taken to have gone wrong.
const MAX_HELPER_OUTPUT: u64 = 4096;

/// Icons decoded from files that are kept in case they are asked for
/// again; items flip between a few.
const ICON_CACHE: usize = 16;

enum Job {
    Refresh {
        address: Address,
        interface: Option<&'static str>,
        pixels: u32,
    },
    Call(Call),
    Menu(MenuRequest),
    Adopt,
}

/// Starts the tray. Events for it go into the returned sender; what the
/// bar should show comes back through `views`. If there is no session bus
/// the bar simply has no tray.
pub fn start(views: sengi_layer::Sender<Vec<View>>, pixels: u32) -> Sender<Event> {
    let (events, inbox) = mpsc::channel();
    let sender = events.clone();
    let spawned = thread::Builder::new().name("tray".into()).spawn(move || {
        if let Err(error) = run(events, inbox, views, pixels) {
            eprintln!("sengi-bar: no system tray: {error}");
        }
    });
    if let Err(error) = spawned {
        eprintln!("sengi-bar: no system tray: {error}");
    }
    sender
}

fn run(
    events: Sender<Event>,
    inbox: Receiver<Event>,
    views: sengi_layer::Sender<Vec<View>>,
    pixels: u32,
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

    // Listening starts before anything is asked for, so nothing is missed.
    let signals = MessageIterator::from(&connection);
    {
        let (events, unique) = (events.clone(), unique.clone());
        thread::Builder::new()
            .name("tray-signals".into())
            .spawn(move || forward_signals(signals, &unique, &events))?;
    }
    set_match(&connection, true, &owner_rule(KDE_WATCHER));
    for watcher in [KDE_WATCHER, FDO_WATCHER] {
        set_match(
            &connection,
            true,
            &format!("type='signal',interface='{watcher}'"),
        );
    }

    let (reads, read_jobs) = mpsc::channel();
    let (actions, action_jobs) = mpsc::sync_channel(MAX_QUEUED_ACTIONS);
    for (name, jobs) in [("tray-reads", read_jobs), ("tray-actions", action_jobs)] {
        let (connection, events) = (connection.clone(), events.clone());
        thread::Builder::new()
            .name(name.into())
            .spawn(move || work(&connection, jobs, &events))?;
    }

    // With no flags at all: we do not push another watcher out, nobody
    // pushes us out, and if the name is taken we wait in line for it, so
    // the bus hands it to us when its owner leaves. (The default set of
    // flags is not the empty one.)
    let ours = matches!(
        connection.request_name_with_flags(KDE_WATCHER, BitFlags::empty()),
        Ok(RequestNameReply::PrimaryOwner)
    );
    let once = RequestNameFlags::DoNotQueue.into();
    let _ = connection.request_name_with_flags(FDO_WATCHER, once);
    let host_name = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    let _ = connection.request_name_with_flags(host_name.as_str(), once);

    let backend = Bus {
        connection,
        reads,
        actions,
        views,
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

/// Turns the signals we subscribed to into events. Bodies are tiny or are
/// not looked at.
fn forward_signals(signals: MessageIterator, unique: &str, events: &Sender<Event>) {
    for message in signals {
        let Ok(message) = message else {
            return;
        };
        let header = message.header();
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
        let body = message.body();
        let small = body.len() <= TEXT_LIMIT;
        let text = || small.then(|| body.deserialize::<String>().ok()).flatten();

        let event = match (interface.as_str(), member.as_str()) {
            (DBUS, "NameOwnerChanged") if sender == DBUS && small => {
                let Ok((name, _old, new)) = body.deserialize::<(String, String, String)>() else {
                    continue;
                };
                if name == KDE_WATCHER {
                    if new.is_empty() {
                        continue;
                    }
                    Event::WatcherOwner {
                        ours: new == unique,
                    }
                } else if new.is_empty() {
                    Event::OwnerLost(name)
                } else {
                    continue;
                }
            }
            (KDE_ITEM | FDO_ITEM, member) => Event::Signal {
                sender: sender.to_string(),
                path: path.to_string(),
                member: member.to_string(),
                status: (member == "NewStatus").then(text).flatten(),
                menu: false,
            },
            (DBUSMENU, member) => Event::Signal {
                sender: sender.to_string(),
                path: path.to_string(),
                member: member.to_string(),
                status: None,
                menu: true,
            },
            // Another watcher's announcements; our own come back to us too.
            (KDE_WATCHER | FDO_WATCHER, member) if sender != unique => {
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

fn work(connection: &Connection, jobs: Receiver<Job>, events: &Sender<Event>) {
    let icons = IconLoader::new();
    for job in jobs {
        let event = match job {
            Job::Refresh {
                address,
                interface,
                pixels,
            } => {
                let outcome = read_item(connection, &address, interface, pixels, &icons);
                Some(Event::Refreshed {
                    address,
                    outcome: outcome.map(Box::new),
                })
            }
            Job::Call(call) => call_item(connection, call),
            Job::Menu(request) => menu_request(connection, &request),
            Job::Adopt => {
                adopt(connection, events);
                None
            }
        };
        if let Some(event) = event {
            if events.send(event).is_err() {
                return;
            }
        }
    }
}

/// Whether an error means the item is gone or stuck. None is an ordinary
/// refusal: no such property, no such method.
fn failure(error: &zbus::Error) -> Option<Failure> {
    let zbus::Error::MethodError(name, _, _) = error else {
        return Some(Failure::Unreachable);
    };
    match name.as_str().strip_prefix("org.freedesktop.DBus.Error.") {
        Some("ServiceUnknown" | "NameHasNoOwner") => Some(Failure::Gone),
        Some("NoReply" | "Timeout" | "TimedOut" | "Disconnected") => Some(Failure::Unreachable),
        _ => None,
    }
}

/// Reads an item's properties one at a time. `GetAll` would be one call
/// instead of a dozen, but then a single over-sized or mistyped property
/// would cost all the others.
struct Reader<'a> {
    connection: &'a Connection,
    address: &'a Address,
    interface: &'static str,
    /// The unique name the answers came from.
    owner: Option<String>,
    /// Whether any answer was big enough to be worth tidying up after.
    large: bool,
}

impl Reader<'_> {
    /// The reply to `Get`, unless it is over `limit`.
    fn reply(&mut self, property: &str, limit: usize) -> Result<Option<zbus::Message>, Failure> {
        let reply = self.connection.call_method(
            Some(self.address.bus.as_str()),
            self.address.path.as_str(),
            Some(PROPERTIES),
            "Get",
            &(self.interface, property),
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
        Ok((size <= limit).then_some(message))
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

    /// Pixmaps are looked at where they lie in the reply, and only the
    /// ones that could be drawn are copied out of it.
    fn pixmaps(&mut self, property: &str) -> Result<Option<Vec<Pixmap>>, Failure> {
        let Some(message) = self.reply(property, PIXMAP_LIMIT)? else {
            return Ok(None);
        };
        let body = message.body();
        let Ok(list) = body.deserialize::<DeserializeValue<'_, Vec<(i32, i32, &[u8])>>>() else {
            return Ok(None);
        };
        Ok(Some(
            list.0
                .into_iter()
                .take(MAX_PIXMAPS)
                .filter(|(width, height, data)| pixmap::plausible(*width, *height, data.len()))
                .map(|(width, height, data)| Pixmap {
                    width,
                    height,
                    data: data.to_vec(),
                })
                .collect(),
        ))
    }
}

type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);

fn read_item(
    connection: &Connection,
    address: &Address,
    interface: Option<&'static str>,
    pixels: u32,
    icons: &IconLoader,
) -> Result<Snapshot, Failure> {
    let mut reader = Reader {
        connection,
        address,
        interface: interface.unwrap_or(KDE_ITEM),
        owner: None,
        large: false,
    };
    let snapshot = read_properties(&mut reader, interface, pixels, icons);
    if reader.large {
        trim_heap();
    }
    snapshot
}

fn read_properties(
    reader: &mut Reader<'_>,
    interface: Option<&'static str>,
    pixels: u32,
    icons: &IconLoader,
) -> Result<Snapshot, Failure> {
    let mut id = reader.text("Id")?;
    if id.is_none() && interface.is_none() {
        // Perhaps it speaks the freedesktop draft's interface instead.
        reader.interface = FDO_ITEM;
        id = reader.text("Id")?;
        if id.is_none() {
            reader.interface = KDE_ITEM;
        }
    }
    let status = reader.text("Status")?;
    let attention = status.as_deref().map(Status::parse) == Some(Status::NeedsAttention);
    let raw = Raw {
        id,
        title: reader.text("Title")?,
        tooltip: reader
            .get::<ToolTip>("ToolTip", TOOLTIP_LIMIT)?
            .map(|(_icon, _pixmaps, title, body)| (title, body)),
        is_menu: reader.get("ItemIsMenu", TEXT_LIMIT)?,
        menu: reader
            .get::<OwnedObjectPath>("Menu", TEXT_LIMIT)?
            .map(|path| path.to_string()),
        icon_theme_path: reader.text("IconThemePath")?,
        icon_name: reader.text("IconName")?,
        overlay_icon_name: reader.text("OverlayIconName")?,
        icon_pixmaps: reader.pixmaps("IconPixmap")?,
        // Only fetched when they would be shown.
        attention_icon_name: if attention {
            reader.text("AttentionIconName")?
        } else {
            None
        },
        attention_pixmaps: if attention {
            reader.pixmaps("AttentionIconPixmap")?
        } else {
            None
        },
        status,
    };
    if raw.is_empty() {
        return Err(Failure::Unreachable);
    }
    let properties = Properties::from(raw);
    let icon = item::icon(&properties, pixels, icons).map(Arc::new);
    Ok(Snapshot {
        owner: reader
            .owner
            .take()
            .unwrap_or_else(|| reader.address.bus.clone()),
        interface: reader.interface,
        properties: properties.without_pixmaps(),
        icon,
    })
}

fn call_item(connection: &Connection, call: Call) -> Option<Event> {
    let send = |method: &str, body: &dyn ErasedBody| {
        body.call(connection, &call.bus, &call.path, call.interface, method)
    };
    // Where the click was on screen is not ours to know under Wayland.
    let origin = (0i32, 0i32);
    let result = match &call.method {
        Method::Activate(_) => send("Activate", &origin),
        Method::SecondaryActivate => send("SecondaryActivate", &origin),
        Method::ContextMenu => send("ContextMenu", &origin),
        Method::Scroll { delta, horizontal } => {
            let orientation = if *horizontal {
                "horizontal"
            } else {
                "vertical"
            };
            send("Scroll", &(*delta, orientation))
        }
    };
    match (call.method, result) {
        (Method::Activate(place), Err(error)) if failure(&error).is_none() => {
            Some(Event::ActivateFailed { id: call.id, place })
        }
        _ => None,
    }
}

/// A call body of any type, so one closure can send several kinds.
trait ErasedBody {
    fn call(
        &self,
        connection: &Connection,
        bus: &str,
        path: &str,
        interface: &str,
        method: &str,
    ) -> zbus::Result<zbus::Message>;
}

impl<B: Serialize + DynamicType> ErasedBody for B {
    fn call(
        &self,
        connection: &Connection,
        bus: &str,
        path: &str,
        interface: &str,
        method: &str,
    ) -> zbus::Result<zbus::Message> {
        connection.call_method(Some(bus), path, Some(interface), method, self)
    }
}

fn menu_request(connection: &Connection, request: &MenuRequest) -> Option<Event> {
    let send = |method: &str, body: &dyn ErasedBody| {
        body.call(connection, &request.bus, &request.path, DBUSMENU, method)
    };
    let event = |id: i32, name: &str| {
        // The data and the timestamp have no use for these two events.
        let _ = send("Event", &(id, name, Value::from(0i32), 0u32));
    };
    match request.action {
        MenuAction::Click(id) => {
            event(id, "clicked");
            return None;
        }
        MenuAction::Show(id) => {
            let _ = send("AboutToShow", &id);
            if id != 0 {
                event(id, "opened");
            }
        }
        MenuAction::Reload => {}
    }
    let layout = send(
        "GetLayout",
        &(0i32, menu::MAX_DEPTH as i32, menu::PROPERTIES.as_slice()),
    );
    let entries = layout.ok().and_then(|message| {
        let body = message.body();
        if body.len() > MENU_LIMIT {
            return None;
        }
        let reply: Structure<'_> = body.deserialize().ok()?;
        Some(menu::parse(reply.fields().get(1)?))
    });
    trim_heap();
    Some(Event::MenuLayout {
        serial: request.serial,
        entries,
    })
}

/// Unpacking a big reply leaves the allocator holding freed memory; give
/// it back, so one huge menu does not stay on the bar's books for good.
fn trim_heap() {
    #[cfg(target_env = "gnu")]
    // SAFETY: malloc_trim takes no pointers and may be called at any time.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Keeps big buffers out of the heap. A reply of many megabytes is held
/// in one allocation while it is looked at. glibc gives such an allocation
/// a mapping of its own and returns it when freed, but it also raises the
/// size from which it does so each time one is freed, up to 32 MB, and from
/// then on keeps them. Naming the size switches that adjustment off.
fn pin_mmap_threshold() {
    #[cfg(target_env = "gnu")]
    // SAFETY: mallopt takes two integers and may be called at any time.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, LARGE_REPLY as libc::c_int);
    }
}

/// Registers as host with the watcher that is already there and asks it
/// what it has.
fn adopt(connection: &Connection, events: &Sender<Event>) {
    let host = connection
        .unique_name()
        .map(|name| name.to_string())
        .unwrap_or_default();
    let _ = connection.call_method(
        Some(KDE_WATCHER),
        WATCHER_PATH,
        Some(KDE_WATCHER),
        "RegisterStatusNotifierHost",
        &host,
    );
    let reply = connection.call_method(
        Some(KDE_WATCHER),
        WATCHER_PATH,
        Some(PROPERTIES),
        "Get",
        &(KDE_WATCHER, "RegisteredStatusNotifierItems"),
    );
    let Ok(message) = reply else {
        return;
    };
    let body = message.body();
    if body.len() > TOOLTIP_LIMIT {
        return;
    }
    let Ok(services) = body.deserialize::<DeserializeValue<'_, Vec<String>>>() else {
        return;
    };
    for service in services.0.into_iter().take(super::watcher::MAX_ITEMS * 2) {
        let _ = events.send(Event::Announced(service));
    }
}

type CacheKey = (PathBuf, Option<SystemTime>, u64, u32);

/// Finds icon files and has them decoded in a child process.
struct IconLoader {
    dirs: IconDirs,
    decoder: Option<Decoder>,
    cache: RefCell<Vec<(CacheKey, Pixels)>>,
}

impl IconLoader {
    fn new() -> Self {
        IconLoader {
            dirs: IconDirs::from_env(),
            decoder: Decoder::for_this_program(),
            cache: RefCell::default(),
        }
    }
}

impl IconFiles for IconLoader {
    fn named(&self, name: &str, theme_path: &str, pixels: u32) -> Option<Pixels> {
        let own = icons::theme_path(theme_path);
        let path = icons::find(&self.dirs, own.as_deref(), name, pixels)?;
        let meta = std::fs::metadata(&path).ok()?;
        let key = (path, meta.modified().ok(), meta.len(), pixels);
        let mut cache = self.cache.borrow_mut();
        if let Some((_, icon)) = cache.iter().find(|(held, _)| *held == key) {
            return Some(icon.clone());
        }
        let icon = self.decoder.as_ref()?.decode(&key.0, pixels)?;
        if cache.len() >= ICON_CACHE {
            cache.remove(0);
        }
        cache.push((key, icon.clone()));
        Some(icon)
    }
}

/// The process drawing the menu that is on screen.
struct Helper {
    child: Child,
    stdin: ChildStdin,
}

struct Bus {
    connection: Connection,
    reads: Sender<Job>,
    actions: SyncSender<Job>,
    views: sengi_layer::Sender<Vec<View>>,
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
        let mut child = Command::new(std::env::current_exe()?)
            .arg(menu_view::FLAG)
            .arg(&place.output)
            .arg(place.x.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::ErrorKind::BrokenPipe.into());
        };
        // A helper that stops reading must not stop the tray: a write that
        // would block is dropped instead.
        // SAFETY: fcntl on a descriptor we own, with no pointers involved.
        unsafe {
            let fd = stdin.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }

        let events = self.events.clone();
        thread::Builder::new()
            .name("tray-menu".into())
            .spawn(move || {
                let lines = BufReader::new(stdout.take(MAX_HELPER_OUTPUT)).lines();
                for line in lines.map_while(Result::ok) {
                    let event = match menu_view::parse_report(&line) {
                        Some(menu_view::Report::Clicked(id)) => Event::MenuClicked { serial, id },
                        Some(menu_view::Report::Opened(id)) => Event::MenuOpened { serial, id },
                        None => continue,
                    };
                    let _ = events.send(event);
                }
                let _ = events.send(Event::MenuClosed { serial });
            })?;
        Ok(Helper { child, stdin })
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

    fn refresh(&mut self, address: &Address, interface: Option<&'static str>, pixels: u32) {
        let _ = self.reads.send(Job::Refresh {
            address: address.clone(),
            interface,
            pixels,
        });
    }

    fn call(&mut self, call: Call) {
        let _ = self.actions.try_send(Job::Call(call));
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
        let _ = self.views.send(views);
    }

    fn watch_menu(&mut self, bus: &str, path: &str) {
        set_match(&self.connection, true, &Self::menu_rule(bus, path));
    }

    fn unwatch_menu(&mut self, bus: &str, path: &str) {
        set_match(&self.connection, false, &Self::menu_rule(bus, path));
    }

    fn menu(&mut self, request: MenuRequest) {
        let serial = request.serial;
        let reads = !matches!(request.action, MenuAction::Click(_));
        if self.actions.try_send(Job::Menu(request)).is_err() && reads {
            // The host is waiting for an answer to this one.
            let _ = self.events.send(Event::MenuLayout {
                serial,
                entries: None,
            });
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
        // Half a menu would garble the next one, so a helper that cannot
        // take a whole one is closed. Its exit is reported as usual.
        if helper
            .stdin
            .write_all(menu::encode(entries).as_bytes())
            .is_err()
        {
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
