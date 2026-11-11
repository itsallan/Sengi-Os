//! The bus side: `org.freedesktop.Notifications` for clients, and a small
//! interface of our own for the bar, the quick settings and the command
//! line.
//!
//! All of it runs away from the UI thread. Method calls are checked,
//! bounded and cleaned here, then handed to the UI. Signals travel the
//! other way through an ordinary channel that this module's thread sleeps
//! on.

use std::fmt;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use zbus::address::transport::{Transport, UnixSocket};
use zbus::blocking::connection::Builder;
use zbus::blocking::Connection;
use zbus::fdo::RequestNameFlags;
use zbus::message::Header;
use zbus::names::BusName;
use zbus::object_server::SignalEmitter;

use crate::cli::{Command, Dnd};
use crate::intake::Intake;
use crate::notification::{Notification, Request, CAPABILITIES};
use crate::rate::{Allowance, RateLimiter};
use crate::wire::{Actions, WireHints};

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";
pub const CONTROL_PATH: &str = "/org/sengios/Notify";
pub const CONTROL_INTERFACE: &str = "org.sengios.Notify";

/// Messages on their way to the UI at once. Past this, calls are refused
/// until the UI has caught up, so a flood cannot pile up in memory.
const PENDING_MAX: usize = 256;

/// What the bus thread tells the UI.
pub enum ToUi {
    Notify(Notification),
    Close(u32),
    Command(Command),
    /// The name was taken away; there is nothing left to answer for.
    NameLost,
    /// The session bus went away.
    BusClosed,
}

/// What the UI wants announced on the bus.
pub enum Signal {
    Closed {
        id: u32,
        reason: u32,
        sender: String,
        token: u64,
    },
    Action {
        id: u32,
        key: String,
        sender: String,
    },
    /// What the properties of our own interface now are.
    State { dnd: bool, unread: u32 },
}

/// Counts messages sent to the UI and not yet handled there.
#[derive(Clone, Default)]
pub struct Pending(Arc<AtomicUsize>);

impl Pending {
    /// The UI has taken one message off the channel.
    pub fn handled(&self) {
        // Saturating: a stray extra call must not wrap around to "full".
        let _ = self
            .0
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
    }

    fn sent(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    fn full(&self) -> bool {
        self.0.load(Ordering::Relaxed) >= PENDING_MAX
    }
}

/// Passes a message to the UI. False once the UI is gone.
pub type Deliver = Arc<dyn Fn(ToUi) -> bool + Send + Sync>;

/// The way into the UI loop, shared by both interfaces.
#[derive(Clone)]
pub struct Link {
    deliver: Deliver,
    pending: Pending,
}

impl Link {
    pub fn new(deliver: Deliver, pending: Pending) -> Link {
        Link { deliver, pending }
    }

    fn send(&self, message: ToUi) -> zbus::fdo::Result<()> {
        // Counted first: the UI may handle it before `deliver` returns.
        self.pending.sent();
        if (self.deliver)(message) {
            Ok(())
        } else {
            self.pending.handled();
            Err(zbus::fdo::Error::Failed(
                "the service is shutting down".into(),
            ))
        }
    }

    fn busy(&self) -> zbus::fdo::Result<()> {
        if self.pending.full() {
            return Err(too_many());
        }
        Ok(())
    }
}

fn too_many() -> zbus::fdo::Error {
    zbus::fdo::Error::LimitsExceeded("too many requests at once".into())
}

struct Notifications {
    link: Link,
    intake: Intake,
    /// What each sender may post, and what all of them may post together.
    /// The second is what a client that reconnects for every notification
    /// runs into.
    per_sender: RateLimiter,
    everyone: RateLimiter,
    started: Instant,
    default_timeout: u64,
}

/// The one name the limiter for everyone counts under.
const EVERYONE: &str = "";

impl Notifications {
    fn new(link: Link, default_timeout: u64) -> Notifications {
        Notifications {
            link,
            intake: Intake::default(),
            per_sender: RateLimiter::new(Allowance::per_sender()),
            everyone: RateLimiter::new(Allowance::everyone()),
            started: Instant::now(),
            default_timeout,
        }
    }

    /// `Notify`, with the sender's unique name already taken from the
    /// message. `now` is in milliseconds.
    fn accept(
        &mut self,
        sender: &str,
        replaces_id: u32,
        request: Request<'_>,
        now: u64,
    ) -> zbus::fdo::Result<u32> {
        self.link.busy()?;
        if !self.per_sender.allow(sender, now) || !self.everyone.allow(EVERYONE, now) {
            return Err(too_many());
        }
        let admitted = self.intake.admit(sender, replaces_id);
        let mut note = request.into_notification(admitted.id, self.default_timeout);
        note.sender = sender.to_string();
        note.token = admitted.token;
        if let Err(error) = self.link.send(ToUi::Notify(note)) {
            self.intake.withdraw(admitted);
            return Err(error);
        }
        Ok(admitted.id)
    }
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &mut self,
        #[zbus(header)] header: Header<'_>,
        app_name: &str,
        replaces_id: u32,
        _app_icon: &str,
        summary: &str,
        body: &str,
        actions: Actions<'_>,
        hints: WireHints,
        expire_timeout: i32,
    ) -> zbus::fdo::Result<u32> {
        // Every message that comes through a bus carries its sender. One
        // without could not be told apart from any other, so it is refused.
        let Some(sender) = header.sender() else {
            return Err(zbus::fdo::Error::AccessDenied(
                "the call has no sender".into(),
            ));
        };
        let request = Request {
            app_name,
            summary,
            body,
            actions: actions.0,
            hints: hints.0,
            expire_timeout,
        };
        let now = self.started.elapsed().as_millis() as u64;
        self.accept(sender.as_str(), replaces_id, request, now)
    }

    fn close_notification(&self, id: u32) -> zbus::fdo::Result<()> {
        self.link.busy()?;
        self.link.send(ToUi::Close(id))
    }

    fn get_capabilities(&self) -> Vec<&'static str> {
        CAPABILITIES.to_vec()
    }

    /// Name, vendor, version, and the version of the spec.
    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("sengi-notify", "Sengi OS", env!("CARGO_PKG_VERSION"), "1.2")
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

/// What the bar's bell, the quick settings and `sengi-notify --...` talk to.
struct Control {
    link: Link,
    limiter: RateLimiter,
    started: Instant,
    dnd: bool,
    unread: u32,
}

impl Control {
    fn new(link: Link) -> Control {
        Control {
            link,
            limiter: RateLimiter::new(Allowance::control()),
            started: Instant::now(),
            dnd: false,
            unread: 0,
        }
    }

    fn command(&mut self, command: Command) -> zbus::fdo::Result<()> {
        let now = self.started.elapsed().as_millis() as u64;
        self.order(command, now)
    }

    /// Passes a command on, unless they are coming faster than anyone
    /// could click.
    fn order(&mut self, command: Command, now: u64) -> zbus::fdo::Result<()> {
        self.link.busy()?;
        if !self.limiter.allow(EVERYONE, now) {
            return Err(too_many());
        }
        self.link.send(ToUi::Command(command))
    }
}

#[zbus::interface(name = "org.sengios.Notify")]
impl Control {
    fn toggle_centre(&mut self) -> zbus::fdo::Result<()> {
        self.command(Command::ToggleCentre)
    }

    fn dismiss_all(&mut self) -> zbus::fdo::Result<()> {
        self.command(Command::DismissAll)
    }

    fn set_do_not_disturb(&mut self, enabled: bool) -> zbus::fdo::Result<()> {
        let dnd = if enabled { Dnd::On } else { Dnd::Off };
        self.command(Command::Dnd(dnd))
    }

    fn toggle_do_not_disturb(&mut self) -> zbus::fdo::Result<()> {
        self.command(Command::Dnd(Dnd::Toggle))
    }

    /// Whether do-not-disturb is on.
    #[zbus(property)]
    fn do_not_disturb(&self) -> bool {
        self.dnd
    }

    /// Notifications in the centre that came in since it was last opened.
    #[zbus(property)]
    fn unread(&self) -> u32 {
        self.unread
    }
}

/// Why the service could not start.
#[derive(Debug)]
pub enum StartError {
    /// Something else already answers for notifications.
    NameTaken,
    Bus(zbus::Error),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::NameTaken => write!(f, "{NAME} is already owned"),
            StartError::Bus(error) => write!(f, "session bus: {error}"),
        }
    }
}

impl From<zbus::Error> for StartError {
    fn from(error: zbus::Error) -> StartError {
        StartError::Bus(error)
    }
}

/// Opens the socket to the session bus ourselves.
///
/// Left to zbus, the connect runs on a thread of the `blocking` crate's
/// pool, and with blocking 1.7.0 that thread never ends: it wakes twice a
/// second for as long as the process lives. A socket that is already
/// connected keeps the pool from ever starting.
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
fn session() -> zbus::Result<Builder<'static>> {
    match session_socket() {
        Ok(socket) => Ok(Builder::async_io_unix_stream(socket)),
        // A bus that is not on a unix socket, or not there at all: let
        // zbus try, and report the error in its own words if it fails.
        Err(_) => Builder::session(),
    }
}

fn connect(link: Link, default_timeout: u64) -> Result<Connection, StartError> {
    let connection = session()?
        .serve_at(PATH, Notifications::new(link.clone(), default_timeout))?
        .serve_at(CONTROL_PATH, Control::new(link))?
        .build()?;
    // Never wait in line for the name, and never hand it over: whoever
    // holds it sees every notification of the session.
    match connection.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(_) => Ok(connection),
        Err(zbus::Error::NameTaken) => Err(StartError::NameTaken),
        Err(error) => Err(StartError::Bus(error)),
    }
}

/// Tells the UI when the name is taken away or the bus is gone. Without
/// either there is nobody left to serve, and the process should end
/// rather than sit there looking alive.
fn watch(connection: &Connection, link: &Link) -> zbus::Result<()> {
    let proxy = zbus::blocking::fdo::DBusProxy::new(connection)?;
    let lost = proxy.receive_name_lost()?;
    let link = link.clone();
    thread::Builder::new()
        .name("bus-watch".into())
        .spawn(move || {
            for signal in lost {
                let ours = signal.args().is_ok_and(|args| args.name.as_str() == NAME);
                if ours {
                    let _ = link.send(ToUi::NameLost);
                    return;
                }
            }
            // The stream of signals only ends with the connection.
            let _ = link.send(ToUi::BusClosed);
        })
        .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    Ok(())
}

/// Sends the signals for one request from the UI.
///
/// Each one first takes the lock of the interface it belongs to. A method
/// call holds that lock until its reply has been written, so a client is
/// never told that a notification closed before it has been told the id.
async fn announce(connection: &Connection, signal: Signal) -> zbus::Result<()> {
    let server = connection.inner().object_server();
    match signal {
        Signal::Closed {
            id,
            reason,
            sender,
            token,
        } => {
            let interface = server.interface::<_, Notifications>(PATH).await?;
            let mut notifications = interface.get_mut().await;
            notifications.intake.release(id, token);
            let emitter = to_sender(connection, &sender)?;
            Notifications::notification_closed(&emitter, id, reason).await
        }
        Signal::Action { id, key, sender } => {
            let interface = server.interface::<_, Notifications>(PATH).await?;
            let _held = interface.get_mut().await;
            let emitter = to_sender(connection, &sender)?;
            Notifications::action_invoked(&emitter, id, &key).await
        }
        Signal::State { dnd, unread } => {
            let interface = server.interface::<_, Control>(CONTROL_PATH).await?;
            let mut control = interface.get_mut().await;
            let emitter = interface.signal_emitter();
            if control.dnd != dnd {
                control.dnd = dnd;
                control.do_not_disturb_changed(emitter).await?;
            }
            if control.unread != unread {
                control.unread = unread;
                control.unread_changed(emitter).await?;
            }
            Ok(())
        }
    }
}

/// An emitter for signals about a notification, addressed to the client
/// that sent it. The spec does not say whether these signals are broadcast
/// or directed; directed is what dunst does, every client library listens
/// on the connection it called `Notify` from, and it keeps one program
/// from following what the user does with another's notifications.
fn to_sender<'a>(connection: &'a Connection, sender: &str) -> zbus::Result<SignalEmitter<'a>> {
    let emitter = SignalEmitter::new(connection.inner(), PATH)?;
    match BusName::try_from(sender.to_string()) {
        Ok(name) => Ok(emitter.set_destination(name)),
        Err(_) => Ok(emitter),
    }
}

/// Runs the bus side on the calling thread: takes the name, reports on
/// `ready` whether that worked, then sends out the UI's signals until the
/// UI is gone.
pub fn serve(
    link: Link,
    signals: Receiver<Signal>,
    default_timeout: u64,
    ready: Sender<Result<(), StartError>>,
) {
    let started = connect(link.clone(), default_timeout).and_then(|connection| {
        watch(&connection, &link)?;
        Ok(connection)
    });
    let connection = match started {
        Ok(connection) => {
            let _ = ready.send(Ok(()));
            connection
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    for signal in signals {
        if let Err(error) = zbus::block_on(announce(&connection, signal)) {
            eprintln!("sengi-notify: could not send a signal: {error}");
        }
    }
}

fn call(connection: &Connection, method: &str) -> zbus::Result<()> {
    connection
        .call_method(
            Some(NAME),
            CONTROL_PATH,
            Some(CONTROL_INTERFACE),
            method,
            &(),
        )
        .map(drop)
}

/// Asks the running service to carry out `command`.
pub fn send(command: Command) -> zbus::Result<()> {
    let connection = session()?.build()?;
    match command {
        Command::ToggleCentre => call(&connection, "ToggleCentre"),
        Command::DismissAll => call(&connection, "DismissAll"),
        Command::Dnd(Dnd::Toggle) => call(&connection, "ToggleDoNotDisturb"),
        Command::Dnd(state) => connection
            .call_method(
                Some(NAME),
                CONTROL_PATH,
                Some(CONTROL_INTERFACE),
                "SetDoNotDisturb",
                &(state == Dnd::On),
            )
            .map(drop),
    }
}

/// Asks the running service whether do-not-disturb is on.
pub fn dnd_status() -> zbus::Result<bool> {
    let connection = session()?.build()?;
    let reply = connection.call_method(
        Some(NAME),
        CONTROL_PATH,
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &(CONTROL_INTERFACE, "DoNotDisturb"),
    )?;
    let value: zbus::zvariant::OwnedValue = reply.body().deserialize()?;
    Ok(bool::try_from(value)?)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::Mutex;

    use super::*;
    use crate::notification::Hints;

    /// A link whose far end is a plain channel.
    fn link() -> (Link, mpsc::Receiver<ToUi>, Pending) {
        let (sender, receiver) = mpsc::channel();
        let sender = Mutex::new(sender);
        let pending = Pending::default();
        let deliver: Deliver = Arc::new(move |message| {
            sender
                .lock()
                .is_ok_and(|sender| sender.send(message).is_ok())
        });
        (Link::new(deliver, pending.clone()), receiver, pending)
    }

    fn request<'a>(summary: &'a str, actions: Vec<&'a str>) -> Request<'a> {
        Request {
            app_name: "Test",
            summary,
            body: "",
            actions,
            hints: Hints::default(),
            expire_timeout: -1,
        }
    }

    fn notes(receiver: &mpsc::Receiver<ToUi>) -> Vec<Notification> {
        receiver
            .try_iter()
            .filter_map(|message| match message {
                ToUi::Notify(note) => Some(note),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn notify_hands_the_ui_a_cleaned_notification_with_its_sender() {
        let (link, receiver, _) = link();
        let mut service = Notifications::new(link, 5_000);
        let id = service
            .accept(
                ":1.5",
                0,
                request("Hello\u{202e}", vec!["default", "Open"]),
                0,
            )
            .expect("the call is accepted");
        assert_eq!(id, 1);
        let notes = notes(&receiver);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, 1);
        assert_eq!(notes[0].sender, ":1.5");
        assert_eq!(notes[0].summary, "Hello");
        assert_eq!(notes[0].timeout, Some(5_000));
        assert!(notes[0].has_default);
    }

    #[test]
    fn only_the_sender_of_a_notification_can_replace_it() {
        let (link, receiver, _) = link();
        let mut service = Notifications::new(link, 5_000);
        let pairing = service
            .accept(
                ":1.5",
                0,
                request("Pair?", vec!["allow", "Allow", "deny", "Deny"]),
                0,
            )
            .expect("accepted");

        // Another client dresses the same id up as something harmless.
        let intruder = service
            .accept(
                ":1.66",
                pairing,
                request("Update ready", vec!["allow", "Later"]),
                1,
            )
            .expect("accepted");
        assert_ne!(intruder, pairing);

        // The owner still can.
        let again = service
            .accept(":1.5", pairing, request("Pair? (2)", vec![]), 2)
            .expect("accepted");
        assert_eq!(again, pairing);

        let notes = notes(&receiver);
        let seen: Vec<(u32, &str, &str)> = notes
            .iter()
            .map(|note| (note.id, note.sender.as_str(), note.summary.as_str()))
            .collect();
        assert_eq!(
            seen,
            [
                (pairing, ":1.5", "Pair?"),
                (intruder, ":1.66", "Update ready"),
                (pairing, ":1.5", "Pair? (2)"),
            ]
        );
        // Each call has its own token, the replacement included.
        assert_ne!(notes[0].token, notes[2].token);
    }

    #[test]
    fn an_id_is_free_again_once_its_notification_has_closed() {
        let (link, receiver, _) = link();
        let mut service = Notifications::new(link, 5_000);
        let id = service
            .accept(":1.5", 0, request("a", vec![]), 0)
            .expect("accepted");
        let token = notes(&receiver)[0].token;
        service.intake.release(id, token);
        let reused = service
            .accept(":1.6", id, request("b", vec![]), 1)
            .expect("accepted");
        assert_eq!(reused, id);
    }

    #[test]
    fn one_sender_is_stopped_after_its_burst() {
        let (link, receiver, pending) = link();
        let mut service = Notifications::new(link, 5_000);
        let burst = Allowance::per_sender().burst;
        for _ in 0..burst {
            service
                .accept(":1.5", 0, request("x", vec![]), 0)
                .expect("accepted");
            pending.handled();
        }
        let refused = service.accept(":1.5", 0, request("x", vec![]), 0);
        assert!(matches!(refused, Err(zbus::fdo::Error::LimitsExceeded(_))));
        assert_eq!(notes(&receiver).len(), burst as usize);
        // A refused call holds no id.
        assert_eq!(service.intake.open(), burst as usize);
        // Someone else is not affected.
        assert!(service.accept(":1.6", 0, request("x", vec![]), 0).is_ok());
    }

    #[test]
    fn reconnecting_for_every_call_runs_into_the_limit_for_everyone() {
        let (link, _receiver, pending) = link();
        let mut service = Notifications::new(link, 5_000);
        let mut accepted = 0;
        for connection in 0..1_000 {
            let sender = format!(":1.{connection}");
            if service.accept(&sender, 0, request("x", vec![]), 0).is_ok() {
                accepted += 1;
                pending.handled();
            }
        }
        assert_eq!(accepted, Allowance::everyone().burst);
        // It refills with time.
        let later = Allowance::everyone().refill * 3;
        assert!(service
            .accept(":1.5000", 0, request("x", vec![]), later)
            .is_ok());
    }

    #[test]
    fn calls_are_refused_while_the_ui_is_behind() {
        let (link, receiver, pending) = link();
        let mut service = Notifications::new(link, 5_000);
        for _ in 0..PENDING_MAX {
            pending.sent();
        }
        let refused = service.accept(":1.5", 0, request("x", vec![]), 0);
        assert!(matches!(refused, Err(zbus::fdo::Error::LimitsExceeded(_))));
        assert!(service.close_notification(1).is_err());
        assert!(notes(&receiver).is_empty());
        pending.handled();
        assert!(service.accept(":1.5", 0, request("x", vec![]), 0).is_ok());
    }

    #[test]
    fn a_call_that_cannot_reach_the_ui_holds_no_id() {
        let (link, receiver, pending) = link();
        drop(receiver);
        let mut service = Notifications::new(link, 5_000);
        assert!(service.accept(":1.5", 0, request("x", vec![]), 0).is_err());
        assert_eq!(service.intake.open(), 0);
        assert!(!pending.full());
    }

    #[test]
    fn commands_are_passed_on_until_they_come_too_fast() {
        let (link, receiver, pending) = link();
        let mut control = Control::new(link);
        let burst = Allowance::control().burst;
        for _ in 0..burst {
            control.order(Command::ToggleCentre, 0).expect("accepted");
            pending.handled();
        }
        assert!(control.order(Command::DismissAll, 0).is_err());
        assert_eq!(receiver.try_iter().count(), burst as usize);
        let later = Allowance::control().refill;
        assert!(control.order(Command::Dnd(Dnd::Toggle), later).is_ok());
        assert!(matches!(
            receiver.try_recv(),
            Ok(ToUi::Command(Command::Dnd(Dnd::Toggle)))
        ));
    }

    #[test]
    fn commands_are_refused_while_the_ui_is_behind() {
        let (link, _receiver, pending) = link();
        let mut control = Control::new(link);
        for _ in 0..PENDING_MAX {
            pending.sent();
        }
        assert!(control.order(Command::ToggleCentre, 0).is_err());
    }

    #[test]
    fn the_pending_count_never_goes_below_zero() {
        let pending = Pending::default();
        pending.handled();
        assert!(!pending.full());
        for _ in 0..PENDING_MAX {
            pending.sent();
        }
        assert!(pending.full());
        pending.handled();
        assert!(!pending.full());
    }
}
