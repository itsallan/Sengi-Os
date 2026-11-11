//! The bus side: `org.freedesktop.Notifications` for clients, and a small
//! interface of our own for the bar and the command line.
//!
//! All of it runs away from the UI thread. Method calls are checked,
//! bounded and cleaned here, then handed to the UI through sengi-layer's
//! channel. Signals travel the other way through an ordinary channel that
//! this module's thread sleeps on.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use zbus::blocking::connection::Builder;
use zbus::blocking::Connection;
use zbus::fdo::RequestNameFlags;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::Value;

use crate::cli::{Command, Dnd};
use crate::notification::{self, HintValue, Notification, Request, CAPABILITIES};
use crate::rate::{Allowance, RateLimiter};

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";
pub const CONTROL_PATH: &str = "/org/sengios/Notify";
pub const CONTROL_INTERFACE: &str = "org.sengios.Notify";

/// Messages on their way to the UI at once. Past this, `Notify` is refused
/// until the UI has caught up, so a flood cannot pile up in memory.
const PENDING_MAX: usize = 256;

/// What the bus thread tells the UI.
pub enum ToUi {
    Notify(Notification),
    Close(u32),
    Command(Command),
    /// Another service took the name; there is nothing left to do here.
    NameLost,
}

/// What the UI wants announced on the bus.
pub enum Signal {
    Closed { id: u32, reason: u32 },
    Action { id: u32, key: String },
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

/// The way into the UI loop, shared by both interfaces.
#[derive(Clone)]
struct Link {
    ui: Arc<Mutex<sengi_layer::Sender<ToUi>>>,
    pending: Pending,
}

impl Link {
    fn send(&self, message: ToUi) -> zbus::fdo::Result<()> {
        let ui = self
            .ui
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("the service is shutting down".into()))?;
        // Counted first: the UI may handle it before `send` even returns.
        self.pending.sent();
        ui.send(message).map_err(|_| {
            self.pending.handled();
            zbus::fdo::Error::Failed("the service is shutting down".into())
        })
    }
}

struct Notifications {
    link: Link,
    limiter: RateLimiter,
    started: Instant,
    last_id: u32,
    default_timeout: u64,
}

impl Notifications {
    /// Ids count up from 1 and skip 0 when they wrap, as the spec asks.
    fn next_id(&mut self) -> u32 {
        self.last_id = next_id(self.last_id);
        self.last_id
    }
}

fn next_id(last: u32) -> u32 {
    match last.wrapping_add(1) {
        0 => 1,
        id => id,
    }
}

fn hint_value<'a>(value: &'a Value<'a>) -> HintValue<'a> {
    match value {
        Value::Bool(value) => HintValue::Bool(*value),
        Value::U8(value) => HintValue::Int(i64::from(*value)),
        Value::I16(value) => HintValue::Int(i64::from(*value)),
        Value::U16(value) => HintValue::Int(i64::from(*value)),
        Value::I32(value) => HintValue::Int(i64::from(*value)),
        Value::U32(value) => HintValue::Int(i64::from(*value)),
        Value::I64(value) => HintValue::Int(*value),
        Value::U64(value) => HintValue::Int(i64::try_from(*value).unwrap_or(i64::MAX)),
        Value::Str(value) => HintValue::Str(value.as_str()),
        // Some bindings wrap every hint in one more variant.
        Value::Value(inner) => match &**inner {
            Value::Value(_) => HintValue::Other,
            inner => hint_value(inner),
        },
        _ => HintValue::Other,
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
        actions: Vec<&str>,
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::fdo::Result<u32> {
        let sender = header.sender().map(|name| name.as_str()).unwrap_or("");
        let now = self.started.elapsed().as_millis() as u64;
        if !self.limiter.allow(sender, now) || self.link.pending.full() {
            return Err(zbus::fdo::Error::LimitsExceeded(
                "too many notifications at once".into(),
            ));
        }

        // Only an id we have handed out can be replaced. Anything else
        // would let a client pick the ids of notifications still to come.
        let id = if replaces_id != 0 && replaces_id <= self.last_id {
            replaces_id
        } else {
            self.next_id()
        };
        let hints =
            notification::parse_hints(hints.iter().map(|(name, value)| (*name, hint_value(value))));
        let request = Request {
            app_name,
            summary,
            body,
            actions,
            hints,
            expire_timeout,
        };
        self.link.send(ToUi::Notify(
            request.into_notification(id, self.default_timeout),
        ))?;
        Ok(id)
    }

    fn close_notification(&self, id: u32) -> zbus::fdo::Result<()> {
        if self.link.pending.full() {
            return Err(zbus::fdo::Error::LimitsExceeded(
                "too many requests at once".into(),
            ));
        }
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

/// What the bar's bell and `sengi-notify --...` talk to.
struct Control {
    link: Link,
}

#[zbus::interface(name = "org.sengios.Notify")]
impl Control {
    fn toggle_centre(&self) -> zbus::fdo::Result<()> {
        self.link.send(ToUi::Command(Command::ToggleCentre))
    }

    fn dismiss_all(&self) -> zbus::fdo::Result<()> {
        self.link.send(ToUi::Command(Command::DismissAll))
    }

    fn set_do_not_disturb(&self, enabled: bool) -> zbus::fdo::Result<()> {
        let dnd = if enabled { Dnd::On } else { Dnd::Off };
        self.link.send(ToUi::Command(Command::Dnd(dnd)))
    }

    fn toggle_do_not_disturb(&self) -> zbus::fdo::Result<()> {
        self.link.send(ToUi::Command(Command::Dnd(Dnd::Toggle)))
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

fn connect(link: Link, default_timeout: u64) -> Result<Connection, StartError> {
    let notifications = Notifications {
        link: link.clone(),
        limiter: RateLimiter::new(Allowance::default()),
        started: Instant::now(),
        last_id: 0,
        default_timeout,
    };
    let connection = Builder::session()?
        .serve_at(PATH, notifications)?
        .serve_at(CONTROL_PATH, Control { link })?
        .build()?;
    // Never wait in line for the name, and give it up when another
    // notification service asks for it.
    let flags = RequestNameFlags::DoNotQueue | RequestNameFlags::AllowReplacement;
    match connection.request_name_with_flags(NAME, flags) {
        Ok(_) => Ok(connection),
        Err(zbus::Error::NameTaken) => Err(StartError::NameTaken),
        Err(error) => Err(StartError::Bus(error)),
    }
}

/// Tells the UI when the name is taken away.
fn watch_name(connection: &Connection, link: &Link) -> zbus::Result<()> {
    let proxy = zbus::blocking::fdo::DBusProxy::new(connection)?;
    let lost = proxy.receive_name_lost()?;
    let link = link.clone();
    thread::Builder::new()
        .name("name-watch".into())
        .spawn(move || {
            for signal in lost {
                let ours = signal.args().is_ok_and(|args| args.name.as_str() == NAME);
                if ours {
                    let _ = link.send(ToUi::NameLost);
                    return;
                }
            }
        })
        .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    Ok(())
}

/// Runs the bus side on the calling thread: takes the name, reports on
/// `ready` whether that worked, then sends out the UI's signals until the
/// UI is gone.
pub fn serve(
    ui: sengi_layer::Sender<ToUi>,
    pending: Pending,
    signals: Receiver<Signal>,
    default_timeout: u64,
    ready: Sender<Result<(), StartError>>,
) {
    let link = Link {
        ui: Arc::new(Mutex::new(ui)),
        pending,
    };
    let started = connect(link.clone(), default_timeout).and_then(|connection| {
        watch_name(&connection, &link)?;
        let emitter = SignalEmitter::new(connection.inner(), PATH)?.into_owned();
        Ok((connection, emitter))
    });
    let (_connection, emitter) = match started {
        Ok(started) => {
            let _ = ready.send(Ok(()));
            started
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    for signal in signals {
        let sent = match signal {
            Signal::Closed { id, reason } => {
                zbus::block_on(Notifications::notification_closed(&emitter, id, reason))
            }
            Signal::Action { id, key } => {
                zbus::block_on(Notifications::action_invoked(&emitter, id, &key))
            }
        };
        if let Err(error) = sent {
            eprintln!("sengi-notify: could not send a signal: {error}");
        }
    }
}

/// Asks the running service to carry out `command`.
pub fn send(command: Command) -> zbus::Result<()> {
    let connection = Connection::session()?;
    let call = |method: &str| {
        connection
            .call_method(
                Some(NAME),
                CONTROL_PATH,
                Some(CONTROL_INTERFACE),
                method,
                &(),
            )
            .map(drop)
    };
    match command {
        Command::ToggleCentre => call("ToggleCentre"),
        Command::DismissAll => call("DismissAll"),
        Command::Dnd(Dnd::Toggle) => call("ToggleDoNotDisturb"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_count_up_and_never_use_zero() {
        assert_eq!(next_id(0), 1);
        assert_eq!(next_id(1), 2);
        assert_eq!(next_id(u32::MAX), 1);
    }

    #[test]
    fn hints_of_any_integer_type_are_read() {
        assert_eq!(hint_value(&Value::U8(2)), HintValue::Int(2));
        assert_eq!(hint_value(&Value::I32(2)), HintValue::Int(2));
        assert_eq!(hint_value(&Value::U32(1)), HintValue::Int(1));
        assert_eq!(hint_value(&Value::U64(u64::MAX)), HintValue::Int(i64::MAX));
        assert_eq!(hint_value(&Value::Bool(true)), HintValue::Bool(true));
        assert_eq!(hint_value(&Value::from("im")), HintValue::Str("im"));
        assert_eq!(hint_value(&Value::F64(1.0)), HintValue::Other);
    }

    #[test]
    fn a_hint_wrapped_once_is_read_and_deeper_nesting_is_not_followed() {
        let once = Value::Value(Box::new(Value::U8(2)));
        assert_eq!(hint_value(&once), HintValue::Int(2));
        let twice = Value::Value(Box::new(Value::Value(Box::new(Value::U8(2)))));
        assert_eq!(hint_value(&twice), HintValue::Other);
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
