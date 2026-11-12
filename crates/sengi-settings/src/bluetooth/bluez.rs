//! Bluetooth through BlueZ on the system bus.
//!
//! Reading the adapter and switching it are sengi-quick's
//! `system/bluez.rs`. The rest is new here: searching, pairing with an
//! `org.bluez.Agent1` for the questions BlueZ asks on the way, connecting,
//! and removing a device.
//!
//! The agent is registered when the first pairing starts and is not made
//! the default one, so BlueZ only turns to it for pairings begun from
//! this connection. Even so every call is checked: it must come from the
//! bus name that owned `org.bluez` when the agent was registered, or it is
//! refused without the user ever seeing it. A PIN typed by the user goes
//! straight into the reply; see sengi-system's `secret.rs` for what happens to the copies
//! in memory.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use zbus::export::serde::{Serialize, Serializer};
use zbus::zvariant::{ObjectPath, Signature, Type};

use super::{
    arrange, Answer, Bluetooth, BluetoothService, Device, Kind, PairError, Prompt, Prompter,
    Question, Reading, Reply,
};
use crate::bus::{self, Bus, Listing, Object};
use crate::network::Refusal;
use crate::secret::Secret;
use crate::text;
use crate::worker::Notify;

const SERVICE: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const AGENT_MANAGER: &str = "org.bluez.AgentManager1";
const MANAGER_PATH: &str = "/org/bluez";
const AGENT_PATH: &str = "/org/sengios/settings/bluetooth";
/// Asks for confirmation, a PIN or a passkey as the device requires.
const CAPABILITY: &str = "KeyboardDisplay";

/// The first adapter's path, whether it is powered and whether it is
/// looking for devices.
pub fn adapter(objects: &[Object]) -> Option<(&str, bool, bool)> {
    objects.iter().find_map(|object| {
        let props = object.interface(ADAPTER)?;
        Some((
            object.path.as_str(),
            bus::flag(props, "Powered").unwrap_or(false),
            bus::flag(props, "Discovering").unwrap_or(false),
        ))
    })
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

pub fn reading(objects: &[Object]) -> Bluetooth {
    let Some((adapter, powered, searching)) = adapter(objects) else {
        return Bluetooth::Absent;
    };
    if !powered {
        return Bluetooth::Off;
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
                name: text::name(name, "Unnamed device"),
                kind: Kind::from_icon(bus::text(props, "Icon").unwrap_or_default()),
                paired,
                connected: bus::flag(props, "Connected").unwrap_or(false),
                gone: false,
            })
        })
        .collect();
    Bluetooth::On {
        searching,
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

/// What the agent shares with the service: who may call it, and where
/// questions go.
#[derive(Default)]
struct Desk {
    /// The unique bus name of the BlueZ the agent was registered with.
    bluez: Option<String>,
    prompter: Option<Prompter>,
    /// The question BlueZ is waiting on, to be withdrawn if it cancels.
    open: Option<Reply>,
}

type Shared = Arc<Mutex<Desk>>;

fn desk(shared: &Shared) -> std::sync::MutexGuard<'_, Desk> {
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
struct Pin(Secret);

impl Type for Pin {
    const SIGNATURE: &'static Signature = <str as Type>::SIGNATURE;
}

impl Serialize for Pin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.expose())
    }
}

/// A PIN is 1 to 16 characters; a passkey is at most six digits.
pub fn pin_is_valid(pin: &str) -> bool {
    (1..=16).contains(&pin.len()) && pin.bytes().all(|b| b.is_ascii_graphic())
}

pub fn parse_passkey(typed: &str) -> Option<u32> {
    let typed = typed.trim();
    (!typed.is_empty() && typed.len() <= 6 && typed.bytes().all(|b| b.is_ascii_digit()))
        .then(|| typed.parse().ok())
        .flatten()
}

impl Agent {
    /// Whether the call is from the BlueZ this agent belongs to.
    fn is_bluez(&self, header: &zbus::message::Header<'_>) -> bool {
        let sender = header.sender().map(|name| name.as_str().to_string());
        let trusted = desk(&self.shared).bluez.clone();
        sender.is_some() && sender == trusted
    }

    /// Put `question` to the user and wait, without holding up the bus
    /// connection, for the answer.
    async fn ask(&self, trusted: bool, device: String, question: Question) -> Answer {
        if !trusted {
            return Answer::No;
        }
        let reply = Reply::new();
        let prompter = {
            let mut desk = desk(&self.shared);
            if let Some(old) = desk.open.replace(reply.clone()) {
                old.answer(Answer::Cancelled);
            }
            desk.prompter.clone()
        };
        let Some(prompter) = prompter else {
            return Answer::No;
        };
        let waits = question.wants_answer();
        prompter(Some(Prompt {
            device,
            question,
            reply: reply.clone(),
        }));
        if waits {
            reply.wait().await
        } else {
            Answer::Yes
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
    ) -> Result<Pin, AgentError> {
        match self
            .ask(
                self.is_bluez(&header),
                device.to_string(),
                Question::EnterPin,
            )
            .await
        {
            Answer::Typed(pin) if pin_is_valid(pin.expose()) => Ok(Pin(pin)),
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
        let shown = text::clean(pincode, 16);
        verdict(
            self.ask(
                self.is_bluez(&header),
                device.to_string(),
                Question::ShowPin(shown),
            )
            .await,
        )
    }

    async fn request_passkey(
        &self,
        device: ObjectPath<'_>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<u32, AgentError> {
        match self
            .ask(
                self.is_bluez(&header),
                device.to_string(),
                Question::EnterPasskey,
            )
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
        let _ = self
            .ask(
                self.is_bluez(&header),
                device.to_string(),
                Question::ShowPasskey(passkey),
            )
            .await;
    }

    async fn request_confirmation(
        &self,
        device: ObjectPath<'_>,
        passkey: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<(), AgentError> {
        let question = Question::ConfirmPasskey(passkey);
        verdict(
            self.ask(self.is_bluez(&header), device.to_string(), question)
                .await,
        )
    }

    async fn request_authorization(
        &self,
        device: ObjectPath<'_>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<(), AgentError> {
        verdict(
            self.ask(
                self.is_bluez(&header),
                device.to_string(),
                Question::Authorize,
            )
            .await,
        )
    }

    /// A device wants to use a service of this machine. That is not part
    /// of a pairing the user started here, so nobody is asked.
    fn authorize_service(&self, _device: ObjectPath<'_>, _uuid: &str) -> Result<(), AgentError> {
        rejected()
    }

    fn cancel(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        if !self.is_bluez(&header) {
            return;
        }
        let (open, prompter) = {
            let mut desk = desk(&self.shared);
            (desk.open.take(), desk.prompter.clone())
        };
        if let Some(open) = open {
            open.answer(Answer::Cancelled);
        }
        if let Some(prompter) = prompter {
            prompter(None);
        }
    }

    fn release(&self) {}
}

fn refusal(error: &zbus::Error) -> Refusal {
    if bus::denied(error) {
        Refusal::Denied
    } else {
        Refusal::Failed
    }
}

pub struct Bluez {
    bus: Arc<Bus>,
    shared: Shared,
    /// `/sys/class`, or in a debug build a directory laid out like it.
    class: PathBuf,
}

impl Bluez {
    pub fn new(bus: Arc<Bus>, class: PathBuf) -> Bluez {
        Bluez {
            bus,
            shared: Shared::default(),
            class,
        }
    }

    /// The adapter's path, if `id` is one of its devices.
    fn adapter_of(&self, id: &str) -> Option<String> {
        let Listing::Objects(objects) = self.bus.objects(SERVICE, &[ADAPTER]) else {
            return None;
        };
        let (adapter, _, _) = adapter(&objects)?;
        device_of(adapter, id).then(|| adapter.to_string())
    }

    fn adapter_path(&self) -> Option<String> {
        let Listing::Objects(objects) = self.bus.objects(SERVICE, &[ADAPTER]) else {
            return None;
        };
        adapter(&objects).map(|(path, _, _)| path.to_string())
    }

    /// Export the agent and tell BlueZ about it. BlueZ forgets an agent
    /// when its owner leaves the bus, so there is nothing to undo on exit.
    fn register_agent(&self) -> zbus::Result<()> {
        let connection = self
            .bus
            .connection()
            .ok_or_else(|| zbus::Error::Failure("no system bus".to_string()))?;
        let owner = self
            .bus
            .owner(SERVICE)
            .ok_or_else(|| zbus::Error::Failure("BlueZ is not running".to_string()))?;
        // Set before BlueZ can call: a restarted BlueZ has a new name, and
        // the old one is no longer to be believed.
        desk(&self.shared).bluez = Some(owner);
        connection.object_server().at(
            AGENT_PATH,
            Agent {
                shared: self.shared.clone(),
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

impl BluetoothService for Bluez {
    fn read(&self) -> Reading {
        let bluetooth = match self.bus.objects(SERVICE, &[ADAPTER, DEVICE]) {
            Listing::Objects(objects) => reading(&objects),
            Listing::Denied => Bluetooth::Denied,
            Listing::Absent => Bluetooth::Absent,
        };
        let hardware = std::fs::read_dir(self.class.join("bluetooth"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with("hci"));
        Reading {
            bluetooth,
            hardware,
            block: sengi_system::machine::block(&self.class.join("rfkill"), "bluetooth"),
        }
    }

    fn set_powered(&self, on: bool) -> Result<(), Refusal> {
        let path = self.adapter_path().ok_or(Refusal::Failed)?;
        // Fails when the radio is blocked by a hardware switch or rfkill.
        self.bus
            .set_flag(SERVICE, &path, ADAPTER, "Powered", on)
            .map_err(|error| {
                eprintln!("sengi-settings: switching Bluetooth: {error}");
                refusal(&error)
            })
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
        if let Err(error) = self.register_agent() {
            eprintln!("sengi-settings: registering with BlueZ: {error}");
            return Err(if bus::denied(&error) {
                PairError::Denied
            } else {
                PairError::Unavailable
            });
        }
        let result: zbus::Result<()> = self.bus.call(SERVICE, id, DEVICE, "Pair", &());
        if let Some(open) = desk(&self.shared).open.take() {
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
            eprintln!("sengi-settings: trusting the device: {error}");
        }
        Ok(())
    }

    fn cancel_pairing(&self, id: &str) {
        let _ = self.device_call(id, "CancelPairing");
    }

    fn connect(&self, id: &str) -> Result<(), Refusal> {
        self.device_call(id, "Connect")
            .map_err(|error| refusal(&error))
    }

    fn disconnect(&self, id: &str) -> Result<(), Refusal> {
        self.device_call(id, "Disconnect")
            .map_err(|error| refusal(&error))
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
            other => other.map_err(|error| refusal(&error)),
        }
    }

    fn watch(&self, changed: Notify) {
        self.bus.watch(SERVICE, changed);
    }

    fn on_prompt(&self, prompter: Prompter) {
        desk(&self.shared).prompter = Some(prompter);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::MAX_DEVICES;
    use crate::bus::testing::{object, path, text};
    use crate::bus::Prop;

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

    fn devices(bluetooth: Bluetooth) -> Vec<Device> {
        match bluetooth {
            Bluetooth::On { devices, .. } => devices,
            other => panic!("expected Bluetooth on, got {other:?}"),
        }
    }

    #[test]
    fn no_adapter_and_adapter_off() {
        assert_eq!(reading(&[]), Bluetooth::Absent);
        let root = object("/org/bluez", "org.bluez.AgentManager1", &[]);
        assert_eq!(reading(&[root]), Bluetooth::Absent);
        let objects = [
            hci(0, false, false),
            device(0, "AA", Some("Mouse"), true, false),
        ];
        assert_eq!(reading(&objects), Bluetooth::Off);
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
            Bluetooth::On {
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
}
