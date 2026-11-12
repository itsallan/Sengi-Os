//! The devices of a Bluetooth adapter one by one: listing them, searching
//! for new ones, pairing with an `org.bluez.Agent1` for the questions
//! BlueZ asks on the way, connecting, and removing.
//!
//! The agent is registered when the first pairing starts and is not made
//! the default one, so BlueZ only turns to it for pairings begun from
//! this connection. Even so every call is checked as the Wi-Fi agent's
//! are: it must come from the bus name that owned `org.bluez` when the
//! pairing was started, and be about the device that pairing is with.
//! Anything else is refused without the user ever seeing it, and nothing
//! is ever agreed to without the user having been asked. A PIN typed by
//! the user goes straight into the reply; see `secret.rs` for what
//! happens to the copies in memory.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use zbus::export::serde::{Serialize, Serializer};
use zbus::zvariant::{ObjectPath, Signature, Type};

use crate::bluez::{Bluez, ADAPTER, DEVICE, SERVICE};
use crate::bus::{self, Listing, Object};
use crate::names;
use crate::secret::Secret;
use crate::services::{BluetoothService, Refusal};

const AGENT_MANAGER: &str = "org.bluez.AgentManager1";
const MANAGER_PATH: &str = "/org/bluez";
const AGENT_PATH: &str = "/org/sengios/bluetooth/agent";
/// Asks for confirmation, a PIN or a passkey as the device requires.
const CAPABILITY: &str = "KeyboardDisplay";

/// How many devices that are not paired a list holds. A train carriage
/// has hundreds in range.
pub const MAX_DEVICES: usize = 32;

/// What kind of thing a device says it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Headphones, a speaker, a headset.
    Audio,
    /// A keyboard.
    Keyboard,
    /// A mouse or a tablet.
    Mouse,
    /// Another computer.
    Computer,
    /// Anything else.
    Other,
}

impl Kind {
    /// From BlueZ's `Icon` property, which is a freedesktop icon name.
    pub fn from_icon(icon: &str) -> Kind {
        match icon {
            name if name.starts_with("audio-") => Kind::Audio,
            "input-keyboard" => Kind::Keyboard,
            "input-mouse" | "input-tablet" => Kind::Mouse,
            "computer" => Kind::Computer,
            _ => Kind::Other,
        }
    }
}

/// One device the adapter knows of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Opaque to everything but the service: BlueZ's object path.
    pub id: String,
    /// Already cleaned for display.
    pub name: String,
    /// What it says it is.
    pub kind: Kind,
    /// It has been paired with.
    pub paired: bool,
    /// It is connected now.
    pub connected: bool,
}

/// The adapter and its devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adapter {
    /// BlueZ is not running or has no adapter.
    Absent,
    /// BlueZ is running and the bus policy keeps this user out.
    Denied,
    /// There is an adapter and it is switched off.
    Off,
    /// Switched on.
    On {
        /// The adapter is looking for devices.
        searching: bool,
        /// Arranged: see [`arrange`].
        devices: Vec<Device>,
    },
}

/// Paired devices first, each group by name, bounded.
pub fn arrange(mut devices: Vec<Device>) -> Vec<Device> {
    devices.sort_by(|a, b| (!a.paired, &a.name, &a.id).cmp(&(!b.paired, &b.name, &b.id)));
    // The paired ones are the user's own and are never cut; what is cut
    // is the crowd of strangers.
    let paired = devices.iter().filter(|device| device.paired).count();
    devices.truncate(paired + MAX_DEVICES);
    devices
}

/// Why pairing failed, in BlueZ's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PairError {
    /// The PIN or passkey did not match, or the device said no.
    AuthenticationFailed,
    /// Called off, here or on the device.
    Canceled,
    /// The device did not answer in time.
    Timeout,
    /// The device could not be reached.
    Unreachable,
    /// The bus policy does not let this user pair devices.
    Denied,
    /// The service went away or would not take the request.
    Unavailable,
}

/// What BlueZ wants from the user in the middle of pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// Both sides show a number; the user says whether they match.
    ConfirmPasskey(u32),
    /// The device has a fixed PIN, or shows one, to be typed here.
    EnterPin,
    /// A number to be typed here.
    EnterPasskey,
    /// A PIN to be typed on the device.
    ShowPin(String),
    /// A number to be typed on the device.
    ShowPasskey(u32),
    /// Nothing to compare; just whether to go ahead.
    Authorize,
}

impl Question {
    /// Whether BlueZ is waiting for an answer, or only wants it shown.
    pub fn wants_answer(&self) -> bool {
        !matches!(self, Question::ShowPin(_) | Question::ShowPasskey(_))
    }
}

/// What the user said to a [`Question`].
pub enum Answer {
    /// Yes: the numbers match, go ahead.
    Yes,
    /// No.
    No,
    /// A PIN, or a passkey as its digits.
    Typed(Secret),
    /// The question was withdrawn.
    Cancelled,
}

impl std::fmt::Debug for Answer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Answer::Yes => "Yes",
            Answer::No => "No",
            Answer::Typed(_) => "Typed(..)",
            Answer::Cancelled => "Cancelled",
        })
    }
}

#[derive(Default)]
struct Waiting {
    answer: Option<Answer>,
    waker: Option<Waker>,
}

/// Where the answer to one question is left. Whoever asked waits on it,
/// asleep, either as a future or on a thread; the program answers once.
#[derive(Clone, Default)]
pub struct Reply {
    shared: Arc<(Mutex<Waiting>, Condvar)>,
}

impl Reply {
    /// A reply nobody has given yet.
    pub fn new() -> Reply {
        Reply::default()
    }

    fn lock(&self) -> MutexGuard<'_, Waiting> {
        self.shared.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Give the answer. Only the first one counts.
    pub fn answer(&self, answer: Answer) {
        let mut waiting = self.lock();
        if waiting.answer.is_none() {
            waiting.answer = Some(answer);
        }
        if let Some(waker) = waiting.waker.take() {
            waker.wake();
        }
        self.shared.1.notify_all();
    }

    /// Wait for the answer without blocking the thread.
    pub fn wait(&self) -> impl Future<Output = Answer> + Send {
        struct Wait(Reply);
        impl Future for Wait {
            type Output = Answer;
            fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Answer> {
                let mut waiting = self.0.lock();
                match waiting.answer.take() {
                    Some(answer) => Poll::Ready(answer),
                    None => {
                        waiting.waker = Some(context.waker().clone());
                        Poll::Pending
                    }
                }
            }
        }
        Wait(self.clone())
    }

    /// Wait for the answer on this thread, for at most `limit`.
    pub fn wait_blocking(&self, limit: Duration) -> Answer {
        let mut waiting = self.lock();
        let deadline = Instant::now() + limit;
        loop {
            if let Some(answer) = waiting.answer.take() {
                return answer;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Answer::Cancelled;
            }
            waiting = self
                .shared
                .1
                .wait_timeout(waiting, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

/// A question on its way to the user.
#[derive(Clone)]
pub struct Prompt {
    /// The device it is about.
    pub device: String,
    /// What is asked.
    pub question: Question,
    /// Where the answer goes.
    pub reply: Reply,
}

/// Called by the service, from any thread, when BlueZ asks something.
/// `None` withdraws whatever was being asked.
pub type Prompter = Arc<dyn Fn(Option<Prompt>) + Send + Sync>;

/// Bluetooth for a program that manages the devices, on top of the
/// switch every program gets from [`BluetoothService`].
pub trait DeviceService: BluetoothService {
    /// The adapter and its devices as they are now. Never starts the
    /// daemon: one that is not running reads as absent.
    fn devices(&self) -> Adapter;
    /// Start or stop looking for devices. Returns at once.
    fn search(&self, on: bool);
    /// Returns when the device is paired or the attempt has failed, which
    /// takes as long as the user does. Questions arrive through the
    /// prompter meanwhile.
    fn pair(&self, id: &str) -> Result<(), PairError>;
    /// Call off the pairing with `id`.
    fn cancel_pairing(&self, id: &str);
    /// May take ten seconds or more to fail.
    fn connect(&self, id: &str) -> Result<(), Refusal>;
    /// Drop the connection to `id`.
    fn disconnect(&self, id: &str) -> Result<(), Refusal>;
    /// Unpair the device and drop what BlueZ stored about it.
    fn remove(&self, id: &str) -> Result<(), Refusal>;
    /// Where the questions of a pairing go.
    fn on_prompt(&self, prompter: Prompter);
}

/// Whether the first adapter is looking for devices.
fn searching(objects: &[Object]) -> bool {
    objects
        .iter()
        .find_map(|object| object.interface(ADAPTER))
        .and_then(|props| bus::flag(props, "Discovering"))
        .unwrap_or(false)
}

/// Whether `path` is a device of `adapter`, by BlueZ's own naming. Only
/// such a path is ever handed back to BlueZ.
pub fn device_of(adapter: &str, path: &str) -> bool {
    path.strip_prefix(adapter)
        .and_then(|rest| rest.strip_prefix("/dev_"))
        .is_some_and(|address| {
            !address.is_empty()
                && address.len() <= 32
                && address
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// The adapter and its devices, out of these BlueZ objects.
pub fn reading(objects: &[Object]) -> Adapter {
    let Some((adapter, powered)) = crate::bluez::adapter(objects) else {
        return Adapter::Absent;
    };
    if !powered {
        return Adapter::Off;
    }
    let devices = objects
        .iter()
        .filter_map(|object| {
            let props = object.interface(DEVICE)?;
            if !device_of(adapter, &object.path) {
                return None;
            }
            let paired = bus::flag(props, "Paired").unwrap_or(false);
            // The alias is the name the user gave it, or else its own. A
            // device that announces no name at all is one of the dozens
            // of beacons in any room; it is listed only if it is paired.
            let own = bus::text(props, "Name");
            if !paired && own.is_none() {
                return None;
            }
            let name = bus::text(props, "Alias").or(own).unwrap_or_default();
            Some(Device {
                id: object.path.clone(),
                name: names::device_name(name, "Unnamed device"),
                kind: Kind::from_icon(bus::text(props, "Icon").unwrap_or_default()),
                paired,
                connected: bus::flag(props, "Connected").unwrap_or(false),
            })
        })
        .collect();
    Adapter::On {
        searching: searching(objects),
        devices: arrange(devices),
    }
}

/// BlueZ's errors for a failed `Pair`.
pub fn pair_error(name: Option<&str>) -> PairError {
    match name.and_then(|name| name.strip_prefix("org.bluez.Error.")) {
        Some("AuthenticationFailed" | "AuthenticationRejected") => PairError::AuthenticationFailed,
        Some("AuthenticationCanceled") => PairError::Canceled,
        Some("AuthenticationTimeout") => PairError::Timeout,
        Some("ConnectionAttemptFailed" | "Failed" | "InProgress" | "NotReady") => {
            PairError::Unreachable
        }
        _ => PairError::Unavailable,
    }
}

/// A PIN is 1 to 16 characters; a passkey is at most six digits.
pub fn pin_is_valid(pin: &str) -> bool {
    (1..=16).contains(&pin.len()) && pin.bytes().all(|b| b.is_ascii_graphic())
}

/// The passkey typed, if it is one.
pub fn parse_passkey(typed: &str) -> Option<u32> {
    let typed = typed.trim();
    (!typed.is_empty() && typed.len() <= 6 && typed.bytes().all(|b| b.is_ascii_digit()))
        .then(|| typed.parse().ok())
        .flatten()
}

/// A pairing the user started.
struct Started {
    /// Which call to `begin` this is from.
    ticket: u64,
    /// The device it is with. Questions about any other are not its.
    device: String,
    /// The bus name of the BlueZ the request went to. Nobody else is
    /// believed.
    bluez: String,
}

/// What the agent shares with the service: the pairing under way, and
/// where its questions go.
#[derive(Default)]
pub(crate) struct Desk {
    started: Option<Started>,
    tickets: u64,
    prompter: Option<Prompter>,
    /// The question BlueZ is waiting on, to be withdrawn if it cancels.
    open: Option<Reply>,
}

impl Desk {
    /// A pairing with `device` is about to be asked of the BlueZ at
    /// `bluez`. The ticket is for ending it.
    fn begin(&mut self, device: &str, bluez: &str) -> u64 {
        self.tickets += 1;
        self.started = Some(Started {
            ticket: self.tickets,
            device: device.to_string(),
            bluez: bluez.to_string(),
        });
        self.tickets
    }

    /// The pairing with this ticket is over. A later one that has begun
    /// since is not this one's to end. Returns the question left open,
    /// which is now nobody's.
    fn end(&mut self, ticket: u64) -> Option<Reply> {
        self.started
            .take_if(|started| started.ticket == ticket)
            .and_then(|_| self.open.take())
    }

    /// Whether `sender` is the BlueZ of the pairing under way.
    fn is_bluez(&self, sender: Option<&str>) -> bool {
        self.started
            .as_ref()
            .is_some_and(|started| Some(started.bluez.as_str()) == sender)
    }

    /// Whether a request about `device` from `sender` belongs to the
    /// pairing under way.
    fn admits(&self, device: &str, sender: Option<&str>) -> bool {
        self.is_bluez(sender)
            && self
                .started
                .as_ref()
                .is_some_and(|started| started.device == device)
    }
}

pub(crate) type Shared = Arc<Mutex<Desk>>;

fn desk(shared: &Shared) -> MutexGuard<'_, Desk> {
    shared.lock().unwrap_or_else(|p| p.into_inner())
}

struct Agent {
    shared: Shared,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
    Canceled(String),
}

fn rejected<T>() -> Result<T, AgentError> {
    Err(AgentError::Rejected("declined".to_string()))
}

/// A PIN as the string of a D-Bus reply, wiped once the reply is built.
/// As for the iwd agent, zbus's own message buffer is only freed.
struct PinCode(Secret);

impl Type for PinCode {
    const SIGNATURE: &'static Signature = <str as Type>::SIGNATURE;
}

impl Serialize for PinCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.expose())
    }
}

fn sender<'a>(header: &'a zbus::message::Header<'_>) -> Option<&'a str> {
    header.sender().map(|name| name.as_str())
}

impl Agent {
    /// Put `question` about `device` to the user and wait, without
    /// holding up the bus connection, for the answer. A question that is
    /// not from the BlueZ of the pairing under way, or not about its
    /// device, gets a no and is never shown.
    async fn ask(&self, device: &str, sender: Option<&str>, question: Question) -> Answer {
        let reply = Reply::new();
        let prompter = {
            let mut desk = desk(&self.shared);
            if !desk.admits(device, sender) {
                return Answer::No;
            }
            if let Some(old) = desk.open.replace(reply.clone()) {
                old.answer(Answer::Cancelled);
            }
            desk.prompter.clone()
        };
        // Nobody to ask is a no, never a yes.
        let Some(prompter) = prompter else {
            return Answer::No;
        };
        let waits = question.wants_answer();
        prompter(Some(Prompt {
            device: device.to_string(),
            question,
            reply: reply.clone(),
        }));
        if waits {
            reply.wait().await
        } else {
            Answer::Yes
        }
    }

    /// BlueZ has given up on what it was asking.
    fn withdraw(&self, sender: Option<&str>) {
        let (open, prompter) = {
            let mut desk = desk(&self.shared);
            if !desk.is_bluez(sender) {
                return;
            }
            (desk.open.take(), desk.prompter.clone())
        };
        if let Some(open) = open {
            open.answer(Answer::Cancelled);
        }
        if let Some(prompter) = prompter {
            prompter(None);
        }
    }
}

fn verdict(answer: Answer) -> Result<(), AgentError> {
    match answer {
        Answer::Yes => Ok(()),
        Answer::Cancelled => Err(AgentError::Canceled("cancelled".to_string())),
        Answer::No | Answer::Typed(_) => rejected(),
    }
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn request_pin_code(
        &self,
        device: ObjectPath<'_>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<PinCode, AgentError> {
        match self
            .ask(device.as_str(), sender(&header), Question::EnterPin)
            .await
        {
            Answer::Typed(pin) if pin_is_valid(pin.expose()) => Ok(PinCode(pin)),
            Answer::Cancelled => Err(AgentError::Canceled("cancelled".to_string())),
            _ => rejected(),
        }
    }

    async fn display_pin_code(
        &self,
        device: ObjectPath<'_>,
        pincode: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<(), AgentError> {
        let shown = Question::ShowPin(sengi_text::line(pincode, 16));
        verdict(self.ask(device.as_str(), sender(&header), shown).await)
    }

    async fn request_passkey(
        &self,
        device: ObjectPath<'_>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<u32, AgentError> {
        match self
            .ask(device.as_str(), sender(&header), Question::EnterPasskey)
            .await
        {
            Answer::Typed(typed) => match parse_passkey(typed.expose()) {
                Some(passkey) => Ok(passkey),
                None => rejected(),
            },
            Answer::Cancelled => Err(AgentError::Canceled("cancelled".to_string())),
            _ => rejected(),
        }
    }

    async fn display_passkey(
        &self,
        device: ObjectPath<'_>,
        passkey: u32,
        _entered: u16,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        let shown = Question::ShowPasskey(passkey);
        let _ = self.ask(device.as_str(), sender(&header), shown).await;
    }

    async fn request_confirmation(
        &self,
        device: ObjectPath<'_>,
        passkey: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<(), AgentError> {
        let question = Question::ConfirmPasskey(passkey);
        verdict(self.ask(device.as_str(), sender(&header), question).await)
    }

    async fn request_authorization(
        &self,
        device: ObjectPath<'_>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<(), AgentError> {
        verdict(
            self.ask(device.as_str(), sender(&header), Question::Authorize)
                .await,
        )
    }

    /// A device wants to use a service of this machine. That is not part
    /// of a pairing the user started here, so nobody is asked.
    fn authorize_service(&self, _device: ObjectPath<'_>, _uuid: &str) -> Result<(), AgentError> {
        rejected()
    }

    fn cancel(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        self.withdraw(sender(&header));
    }

    fn release(&self) {}
}

impl Bluez {
    /// The first adapter's path.
    fn adapter_path(&self) -> Option<String> {
        let Listing::Objects(objects) = self.bus.objects(SERVICE, &[ADAPTER]) else {
            return None;
        };
        crate::bluez::adapter(&objects).map(|(path, _)| path.to_string())
    }

    /// The adapter's path, if `id` is one of its devices.
    fn adapter_of(&self, id: &str) -> Option<String> {
        self.adapter_path().filter(|adapter| device_of(adapter, id))
    }

    /// Export the agent and tell BlueZ about it. BlueZ forgets an agent
    /// when its owner leaves the bus, so there is nothing to undo on exit.
    fn register_agent(&self) -> zbus::Result<()> {
        let connection = self
            .bus
            .connection()
            .ok_or_else(|| zbus::Error::Failure("no system bus".to_string()))?;
        connection.object_server().at(
            AGENT_PATH,
            Agent {
                shared: self.desk.clone(),
            },
        )?;
        let path = ObjectPath::try_from(AGENT_PATH)?;
        let registered: zbus::Result<()> = self.bus.call(
            SERVICE,
            MANAGER_PATH,
            AGENT_MANAGER,
            "RegisterAgent",
            &(path, CAPABILITY),
        );
        match registered {
            // Ours, from an earlier pairing.
            Err(error) if bus::error_name(&error) == Some("org.bluez.Error.AlreadyExists") => {
                Ok(())
            }
            other => other,
        }
    }

    fn device_call(&self, id: &str, method: &str) -> zbus::Result<()> {
        if self.adapter_of(id).is_none() {
            return Err(zbus::Error::Failure("not a device".to_string()));
        }
        self.bus.call(SERVICE, id, DEVICE, method, &())
    }
}

impl DeviceService for Bluez {
    fn devices(&self) -> Adapter {
        match self.bus.objects(SERVICE, &[ADAPTER, DEVICE]) {
            Listing::Objects(objects) => reading(&objects),
            Listing::Denied => Adapter::Denied,
            Listing::Absent => Adapter::Absent,
        }
    }

    fn search(&self, on: bool) {
        let Some(path) = self.adapter_path() else {
            return;
        };
        let method = if on {
            "StartDiscovery"
        } else {
            "StopDiscovery"
        };
        // Already searching, or not searching to begin with: both fine.
        let _: zbus::Result<()> = self.bus.call(SERVICE, &path, ADAPTER, method, &());
    }

    fn pair(&self, id: &str) -> Result<(), PairError> {
        if self.adapter_of(id).is_none() {
            return Err(PairError::Unavailable);
        }
        // Whoever owns the name now is the one this pairing is asked of.
        // A BlueZ that has restarted since the last one has a new name,
        // and the old one is no longer believed.
        let bluez = self.bus.owner(SERVICE).ok_or(PairError::Unavailable)?;
        if let Err(error) = self.register_agent() {
            eprintln!("{}: registering with BlueZ: {error}", crate::program_name());
            return Err(if bus::denied(&error) {
                PairError::Denied
            } else {
                PairError::Unavailable
            });
        }
        let ticket = desk(&self.desk).begin(id, &bluez);
        let result: zbus::Result<()> = self.bus.call(SERVICE, id, DEVICE, "Pair", &());
        let open = desk(&self.desk).end(ticket);
        if let Some(open) = open {
            open.answer(Answer::Cancelled);
        }
        match result {
            Ok(()) => {}
            Err(error) if bus::error_name(&error) == Some("org.bluez.Error.AlreadyExists") => {}
            Err(error) if bus::denied(&error) => return Err(PairError::Denied),
            Err(error) => return Err(pair_error(bus::error_name(&error))),
        }
        // Trusted, so that it may reconnect by itself later without the
        // agent being asked, which by then will be gone.
        if let Err(error) = self.bus.set_flag(SERVICE, id, DEVICE, "Trusted", true) {
            eprintln!("{}: trusting the device: {error}", crate::program_name());
        }
        Ok(())
    }

    fn cancel_pairing(&self, id: &str) {
        let _ = self.device_call(id, "CancelPairing");
    }

    fn connect(&self, id: &str) -> Result<(), Refusal> {
        self.device_call(id, "Connect")
            .map_err(|error| Refusal::of(&error))
    }

    fn disconnect(&self, id: &str) -> Result<(), Refusal> {
        self.device_call(id, "Disconnect")
            .map_err(|error| Refusal::of(&error))
    }

    fn remove(&self, id: &str) -> Result<(), Refusal> {
        let adapter = self.adapter_of(id).ok_or(Refusal::Failed)?;
        let path = ObjectPath::try_from(id).map_err(|_| Refusal::Failed)?;
        let result: zbus::Result<()> =
            self.bus
                .call(SERVICE, &adapter, ADAPTER, "RemoveDevice", &(path,));
        match result {
            // Already gone is gone.
            Err(error) if bus::error_name(&error) == Some("org.bluez.Error.DoesNotExist") => Ok(()),
            other => other.map_err(|error| Refusal::of(&error)),
        }
    }

    fn on_prompt(&self, prompter: Prompter) {
        desk(&self.desk).prompter = Some(prompter);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::testing::{object, path, text};
    use crate::bus::Prop;
    use std::sync::mpsc;

    fn hci(index: u32, powered: bool, discovering: bool) -> Object {
        object(
            &format!("/org/bluez/hci{index}"),
            ADAPTER,
            &[
                ("Powered", Prop::Bool(powered)),
                ("Discovering", Prop::Bool(discovering)),
            ],
        )
    }

    fn device(adapter: u32, id: &str, name: Option<&str>, paired: bool, connected: bool) -> Object {
        let mut props = vec![
            ("Paired", Prop::Bool(paired)),
            ("Connected", Prop::Bool(connected)),
            ("Adapter", path(&format!("/org/bluez/hci{adapter}"))),
            ("Icon", text("input-mouse")),
        ];
        if let Some(name) = name {
            props.push(("Name", text(name)));
            props.push(("Alias", text(name)));
        } else {
            // BlueZ makes an alias out of the address.
            props.push(("Alias", text("AA-BB-CC-DD-EE-FF")));
        }
        object(&format!("/org/bluez/hci{adapter}/dev_{id}"), DEVICE, &props)
    }

    fn devices(adapter: Adapter) -> Vec<Device> {
        match adapter {
            Adapter::On { devices, .. } => devices,
            other => panic!("expected Bluetooth on, got {other:?}"),
        }
    }

    #[test]
    fn no_adapter_and_adapter_off() {
        assert_eq!(reading(&[]), Adapter::Absent);
        let root = object("/org/bluez", "org.bluez.AgentManager1", &[]);
        assert_eq!(reading(&[root]), Adapter::Absent);
        let objects = [
            hci(0, false, false),
            device(0, "AA", Some("Mouse"), true, false),
        ];
        assert_eq!(reading(&objects), Adapter::Off);
    }

    #[test]
    fn devices_are_read_with_their_state() {
        let objects = [
            hci(0, true, true),
            device(0, "CC", Some("Keyboard"), true, true),
            device(0, "AA", Some("Mouse"), false, false),
            // A beacon with no name: left out. Paired, it would be kept.
            device(0, "DD", None, false, false),
            device(0, "EE", None, true, false),
            // On the other adapter.
            hci(1, true, false),
            device(1, "BB", Some("On the dongle"), true, true),
        ];
        let read = reading(&objects);
        assert!(matches!(
            read,
            Adapter::On {
                searching: true,
                ..
            }
        ));
        let seen: Vec<_> = devices(read)
            .into_iter()
            .map(|d| (d.name, d.paired, d.connected, d.kind))
            .collect();
        assert_eq!(
            seen,
            [
                ("AA-BB-CC-DD-EE-FF".to_string(), true, false, Kind::Mouse),
                ("Keyboard".to_string(), true, true, Kind::Mouse),
                ("Mouse".to_string(), false, false, Kind::Mouse),
            ]
        );
    }

    #[test]
    fn device_names_are_cleaned_and_the_crowd_is_cut() {
        let mut objects = vec![hci(0, true, false)];
        objects.push(device(
            0,
            "00",
            Some("Evil\u{202e}\u{1b}[2J\nname"),
            false,
            false,
        ));
        assert_eq!(devices(reading(&objects))[0].name, "Evil[2J name");
        for i in 1..500 {
            objects.push(device(
                0,
                &format!("{i:04}"),
                Some(&format!("Thing {i}")),
                false,
                false,
            ));
        }
        assert_eq!(devices(reading(&objects)).len(), MAX_DEVICES);
    }

    #[test]
    fn a_crowd_of_strangers_is_cut_and_paired_devices_never_are() {
        let plain = |id: String, name: String, paired: bool| Device {
            id,
            name,
            kind: Kind::Other,
            paired,
            connected: false,
        };
        let mut many: Vec<Device> = (0..200)
            .map(|i| plain(format!("/s/{i:03}"), format!("Stranger {i:03}"), false))
            .collect();
        for i in 0..40 {
            many.push(plain(format!("/p/{i}"), format!("Mine {i:02}"), true));
        }
        let arranged = arrange(many);
        assert_eq!(arranged.iter().filter(|d| d.paired).count(), 40);
        assert_eq!(arranged.len(), 40 + MAX_DEVICES);
    }

    #[test]
    fn kinds_by_icon() {
        assert_eq!(Kind::from_icon("audio-headset"), Kind::Audio);
        assert_eq!(Kind::from_icon("audio-card"), Kind::Audio);
        assert_eq!(Kind::from_icon("input-keyboard"), Kind::Keyboard);
        assert_eq!(Kind::from_icon("input-mouse"), Kind::Mouse);
        assert_eq!(Kind::from_icon("phone"), Kind::Other);
        assert_eq!(Kind::from_icon(""), Kind::Other);
    }

    #[test]
    fn only_paths_that_are_the_adapters_devices_go_back_to_bluez() {
        let hci = "/org/bluez/hci0";
        assert!(device_of(hci, "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF"));
        assert!(!device_of(hci, "/org/bluez/hci1/dev_AA_BB_CC_DD_EE_FF"));
        assert!(!device_of(hci, "/org/bluez/hci0"));
        assert!(!device_of(hci, "/org/bluez/hci0/dev_"));
        assert!(!device_of(hci, "/org/bluez/hci0/dev_AA/../../x"));
        assert!(!device_of(hci, "/org/bluez/hci0/dev_AA/service0001"));
        assert!(!device_of(hci, "/org/freedesktop/login1"));
        // And a device object that claims a path elsewhere is not listed.
        let stray = object(
            "/somewhere/else",
            DEVICE,
            &[("Name", text("Stray")), ("Paired", Prop::Bool(true))],
        );
        assert!(devices(reading(&[self::hci(0, true, false), stray])).is_empty());
    }

    #[test]
    fn pair_errors() {
        let error = |name: &str| pair_error(Some(&format!("org.bluez.Error.{name}")));
        assert_eq!(
            error("AuthenticationFailed"),
            PairError::AuthenticationFailed
        );
        assert_eq!(
            error("AuthenticationRejected"),
            PairError::AuthenticationFailed
        );
        assert_eq!(error("AuthenticationCanceled"), PairError::Canceled);
        assert_eq!(error("AuthenticationTimeout"), PairError::Timeout);
        assert_eq!(error("ConnectionAttemptFailed"), PairError::Unreachable);
        assert_eq!(error("SomethingNew"), PairError::Unavailable);
        assert_eq!(pair_error(None), PairError::Unavailable);
    }

    #[test]
    fn pins_and_passkeys() {
        assert!(pin_is_valid("0000"));
        assert!(pin_is_valid("a1B2c3D4e5F6g7H8"));
        assert!(!pin_is_valid(""));
        assert!(!pin_is_valid("12345678901234567"));
        assert!(!pin_is_valid("12 34"));
        assert!(!pin_is_valid("12\n34"));
        assert_eq!(parse_passkey("123456"), Some(123456));
        assert_eq!(parse_passkey(" 000042 "), Some(42));
        assert_eq!(parse_passkey("1234567"), None);
        assert_eq!(parse_passkey("12a456"), None);
        assert_eq!(parse_passkey("-1"), None);
        assert_eq!(parse_passkey(""), None);
    }

    #[test]
    fn answers_become_the_right_replies() {
        assert!(verdict(Answer::Yes).is_ok());
        assert!(matches!(verdict(Answer::No), Err(AgentError::Rejected(_))));
        assert!(matches!(
            verdict(Answer::Cancelled),
            Err(AgentError::Canceled(_))
        ));
        // Something typed where yes or no was wanted is a no.
        let typed = Answer::Typed(Secret::new("1".to_string()));
        assert!(matches!(verdict(typed), Err(AgentError::Rejected(_))));
    }

    #[test]
    fn a_reply_is_given_once_and_can_be_waited_for() {
        let reply = Reply::new();
        reply.answer(Answer::Yes);
        reply.answer(Answer::No);
        assert!(matches!(
            reply.wait_blocking(Duration::from_millis(1)),
            Answer::Yes
        ));
        // Nothing left, and nobody answering: it gives up.
        assert!(matches!(
            reply.wait_blocking(Duration::from_millis(1)),
            Answer::Cancelled
        ));

        // As a future, woken from another thread.
        let reply = Reply::new();
        let waiter = std::thread::spawn({
            let reply = reply.clone();
            move || zbus::block_on(reply.wait())
        });
        std::thread::sleep(Duration::from_millis(20));
        reply.answer(Answer::Typed(Secret::new("0000".to_string())));
        assert!(matches!(waiter.join().unwrap(), Answer::Typed(pin) if pin.expose() == "0000"));
    }

    const BLUEZ: Option<&str> = Some(":1.5");
    const PHONE: &str = "/org/bluez/hci0/dev_AA";
    const OTHER: &str = "/org/bluez/hci0/dev_BB";

    /// An agent, and the questions that reach the user.
    fn agent() -> (Agent, mpsc::Receiver<Option<Prompt>>) {
        let shared = Shared::default();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        desk(&shared).prompter = Some(Arc::new(move |prompt| {
            let _ = tx.lock().unwrap().send(prompt);
        }));
        (Agent { shared }, rx)
    }

    /// Run a request as far as it goes without an answer.
    fn poll<T>(future: &mut Pin<Box<impl Future<Output = T> + ?Sized>>) -> Poll<T> {
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
    }

    /// Every kind of request the agent can be sent about `device`.
    fn questions() -> Vec<Question> {
        vec![
            Question::ConfirmPasskey(123456),
            Question::Authorize,
            Question::EnterPin,
            Question::EnterPasskey,
            Question::ShowPin("0000".to_string()),
            Question::ShowPasskey(123456),
        ]
    }

    #[test]
    fn nothing_is_asked_and_nothing_agreed_to_when_no_pairing_was_started() {
        let (agent, shown) = agent();
        for question in questions() {
            let mut asked = Box::pin(agent.ask(PHONE, BLUEZ, question));
            assert!(matches!(poll(&mut asked), Poll::Ready(Answer::No)));
        }
        assert!(shown.try_recv().is_err());
        // What a no comes to on the bus: a refusal, never an empty reply.
        assert!(matches!(verdict(Answer::No), Err(AgentError::Rejected(_))));
    }

    #[test]
    fn a_question_about_another_device_is_refused_unseen() {
        let (agent, shown) = agent();
        desk(&agent.shared).begin(PHONE, ":1.5");
        for question in questions() {
            let mut asked = Box::pin(agent.ask(OTHER, BLUEZ, question));
            assert!(matches!(poll(&mut asked), Poll::Ready(Answer::No)));
        }
        assert!(shown.try_recv().is_err());
    }

    #[test]
    fn nobody_but_the_bluez_the_pairing_was_asked_of_is_believed() {
        let (agent, shown) = agent();
        desk(&agent.shared).begin(PHONE, ":1.5");
        for stranger in [Some(":1.99"), None] {
            for question in questions() {
                let mut asked = Box::pin(agent.ask(PHONE, stranger, question));
                assert!(matches!(poll(&mut asked), Poll::Ready(Answer::No)));
            }
            // Nor can a stranger withdraw what BlueZ may be asking.
            agent.withdraw(stranger);
        }
        assert!(shown.try_recv().is_err());
    }

    #[test]
    fn the_question_of_the_pairing_under_way_reaches_the_user_and_waits() {
        let (agent, shown) = agent();
        desk(&agent.shared).begin(PHONE, ":1.5");
        let mut asked = Box::pin(agent.ask(PHONE, BLUEZ, Question::ConfirmPasskey(482913)));
        assert!(poll(&mut asked).is_pending());
        let prompt = shown.try_recv().unwrap().unwrap();
        assert_eq!(prompt.device, PHONE);
        assert_eq!(prompt.question, Question::ConfirmPasskey(482913));
        // Still waiting: showing it is not agreeing to it.
        assert!(poll(&mut asked).is_pending());
        prompt.reply.answer(Answer::Yes);
        assert!(matches!(poll(&mut asked), Poll::Ready(Answer::Yes)));
    }

    #[test]
    fn with_nobody_to_ask_the_answer_is_no() {
        let agent = Agent {
            shared: Shared::default(),
        };
        desk(&agent.shared).begin(PHONE, ":1.5");
        let mut asked = Box::pin(agent.ask(PHONE, BLUEZ, Question::Authorize));
        assert!(matches!(poll(&mut asked), Poll::Ready(Answer::No)));
    }

    #[test]
    fn once_the_pairing_is_over_its_questions_are_refused_again() {
        let (agent, shown) = agent();
        let ticket = desk(&agent.shared).begin(PHONE, ":1.5");
        let mut asked = Box::pin(agent.ask(PHONE, BLUEZ, Question::EnterPin));
        assert!(poll(&mut asked).is_pending());
        assert!(shown.try_recv().unwrap().is_some());
        // The call to pair returns: the question left open is handed back
        // to be withdrawn.
        let open = desk(&agent.shared).end(ticket).unwrap();
        open.answer(Answer::Cancelled);
        assert!(matches!(poll(&mut asked), Poll::Ready(Answer::Cancelled)));
        let mut late = Box::pin(agent.ask(PHONE, BLUEZ, Question::Authorize));
        assert!(matches!(poll(&mut late), Poll::Ready(Answer::No)));
        assert!(shown.try_recv().is_err());
    }

    #[test]
    fn a_pairing_that_ends_late_leaves_the_next_one_alone() {
        let (agent, _shown) = agent();
        let first = desk(&agent.shared).begin(PHONE, ":1.5");
        // A restarted BlueZ, and a second pairing begun before the first
        // call has returned.
        desk(&agent.shared).begin(OTHER, ":1.8");
        assert!(desk(&agent.shared).end(first).is_none());
        let desk = desk(&agent.shared);
        assert!(desk.admits(OTHER, Some(":1.8")));
        // The old BlueZ's name is no longer believed, for either device.
        assert!(!desk.admits(OTHER, BLUEZ));
        assert!(!desk.admits(PHONE, BLUEZ));
    }

    #[test]
    fn bluez_can_withdraw_its_question() {
        let (agent, shown) = agent();
        desk(&agent.shared).begin(PHONE, ":1.5");
        let mut asked = Box::pin(agent.ask(PHONE, BLUEZ, Question::Authorize));
        assert!(poll(&mut asked).is_pending());
        assert!(shown.try_recv().unwrap().is_some());
        agent.withdraw(BLUEZ);
        assert!(shown.try_recv().unwrap().is_none());
        assert!(matches!(poll(&mut asked), Poll::Ready(Answer::Cancelled)));
    }

    #[test]
    fn a_number_that_is_only_shown_is_not_waited_for() {
        let (agent, shown) = agent();
        desk(&agent.shared).begin(PHONE, ":1.5");
        let mut asked = Box::pin(agent.ask(PHONE, BLUEZ, Question::ShowPasskey(271828)));
        assert!(matches!(poll(&mut asked), Poll::Ready(Answer::Yes)));
        assert!(shown.try_recv().unwrap().is_some());
    }
}
