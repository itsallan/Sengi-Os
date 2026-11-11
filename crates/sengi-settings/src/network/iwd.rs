//! Wi-Fi through iwd on the system bus.
//!
//! iwd asks for passphrases through an agent: an object the client exports
//! and registers, which iwd calls back in the middle of `Network.Connect`.
//! The passphrase typed into the dialog waits in a slot for that one call
//! and is handed over once, to the iwd it was meant for and to nobody
//! else. iwd keeps what it needs to rejoin the network; this program
//! stores nothing. See iwd's `doc/agent-api.txt`, and `secret.rs` for what
//! happens to the copies in memory.
//!
//! Reading the device and the networks, switching the radio, scanning and
//! joining with the agent are sengi-quick's `system/iwd.rs` and are to
//! come from sengi-system. Leaving a network, forgetting one, and the
//! interface's name are what this file adds.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use zbus::export::serde::{Serialize, Serializer};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, Signature, Type};

use super::links;
use super::{
    arrange, JoinError, Link, Network, NetworkService, Reading, Refusal, Request, Security,
    Station, Wifi,
};
use crate::bus::{self, Bus, Listing, Object};
use crate::secret::Secret;
use crate::text;
use crate::worker::Notify;

const SERVICE: &str = "net.connman.iwd";
const ADAPTER: &str = "net.connman.iwd.Adapter";
const DEVICE: &str = "net.connman.iwd.Device";
const STATION: &str = "net.connman.iwd.Station";
const NETWORK: &str = "net.connman.iwd.Network";
const KNOWN: &str = "net.connman.iwd.KnownNetwork";
const AGENT_MANAGER: &str = "net.connman.iwd.AgentManager";
const MANAGER_PATH: &str = "/net/connman/iwd";
const AGENT_PATH: &str = "/org/sengios/settings/agent";
/// Every object of iwd's lives under this.
const PREFIX: &str = "/net/connman/iwd/";

/// Networks taken from one scan before they are sorted and cut down.
const MAX_SCANNED: usize = 256;

/// The device the page controls and where it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub path: String,
    pub adapter: Option<String>,
    pub powered: bool,
    /// The kernel's name for it, such as `wlan0`.
    pub name: String,
    /// Present when the device is up and in station mode.
    pub station: Option<(Link, bool)>,
}

/// The first device that is, or could be, a station. Access points and
/// ad-hoc devices are someone else's business.
pub fn device(objects: &[Object]) -> Option<Device> {
    let adapter_on = |path: &str| {
        objects
            .iter()
            .find(|object| object.path == path)
            .and_then(|object| object.interface(ADAPTER))
            .and_then(|props| bus::flag(props, "Powered"))
            .unwrap_or(true)
    };
    objects
        .iter()
        .filter_map(|object| {
            let props = object.interface(DEVICE)?;
            if bus::text(props, "Mode") != Some("station") {
                return None;
            }
            let adapter = bus::path(props, "Adapter").map(str::to_string);
            let powered = bus::flag(props, "Powered").unwrap_or(false)
                && adapter.as_deref().is_none_or(adapter_on);
            let station = object.interface(STATION).map(|station| {
                let link = match bus::text(station, "State") {
                    Some("connected" | "roaming") => Link::Connected,
                    Some("connecting") => Link::Connecting,
                    _ => Link::Disconnected,
                };
                (link, bus::flag(station, "Scanning").unwrap_or(false))
            });
            Some(Device {
                path: object.path.clone(),
                adapter,
                powered,
                name: bus::text(props, "Name").unwrap_or_default().to_string(),
                station,
            })
        })
        // Stable sort: one with a station first, otherwise the first path.
        .min_by_key(|device| device.station.is_none())
}

/// What the page shows for `device`, given the networks its station
/// listed (`ordered`, as from `GetOrderedNetworks`).
pub fn wifi(objects: &[Object], device: &Device, ordered: &[(String, i16)]) -> Wifi {
    let Some((link, scanning)) = device.station.filter(|_| device.powered) else {
        return Wifi::Off;
    };
    let networks = ordered
        .iter()
        .take(MAX_SCANNED)
        .filter_map(|(path, signal)| {
            let props = objects
                .iter()
                .find(|object| object.path == *path)?
                .interface(NETWORK)?;
            Some(Network {
                id: path.clone(),
                name: text::name(bus::text(props, "Name")?, "Unnamed network"),
                security: Security::from_iwd(bus::text(props, "Type").unwrap_or_default()),
                known: bus::path(props, "KnownNetwork").is_some(),
                connected: bus::flag(props, "Connected").unwrap_or(false),
                signal: *signal,
                gone: false,
            })
        })
        .collect();
    Wifi::On(Station {
        link,
        scanning,
        networks: arrange(networks),
        interface: if links::plain_name(&device.name) {
            device.name.clone()
        } else {
            String::new()
        },
        addresses: Vec::new(),
    })
}

/// The stored network behind the network at `id`, if iwd has one. Only a
/// path of iwd's own is ever handed back to it.
pub fn known_path<'a>(objects: &'a [Object], id: &str) -> Option<&'a str> {
    objects
        .iter()
        .find(|object| object.path == id)?
        .interface(NETWORK)
        .and_then(|props| bus::path(props, "KnownNetwork"))
        .filter(|path| ours(path))
}

fn ours(path: &str) -> bool {
    path.starts_with(PREFIX) && ObjectPath::try_from(path).is_ok()
}

/// iwd's errors for a failed `Connect`.
pub fn join_error(name: Option<&str>) -> JoinError {
    match name.and_then(|name| name.strip_prefix("net.connman.iwd.")) {
        // InvalidFormat is a passphrase iwd would not even try.
        Some("Failed" | "InvalidFormat") => JoinError::Failed,
        // iwd wanted to ask for a passphrase and had nobody to ask.
        Some("NoAgent") => JoinError::NeedsPassphrase,
        Some("Aborted" | "Busy" | "InProgress") => JoinError::Aborted,
        Some("Timeout") => JoinError::Timeout,
        Some("NotConfigured") => JoinError::NotConfigured,
        Some("NotSupported") => JoinError::NotSupported,
        _ => JoinError::Unavailable,
    }
}

/// A passphrase waiting to be asked for.
struct Waiting {
    /// Which call to `put` this is from.
    ticket: u64,
    network: String,
    /// The bus name of the iwd the request went to. Nobody else gets it.
    asker: String,
    passphrase: Secret,
}

/// What the agent has to hand out. At most one passphrase, for one
/// network, to one caller, once.
#[derive(Default)]
struct Desk {
    waiting: Option<Waiting>,
    tickets: u64,
    /// The network iwd last asked about when there was nothing to give.
    /// iwd reports that to the caller only as "aborted", which could be
    /// anything; this says the join needs a passphrase typed.
    unanswered: Option<String>,
}

impl Desk {
    /// Leave a passphrase to be asked for. The ticket is for taking it
    /// back if nobody did.
    fn put(&mut self, network: &str, asker: &str, passphrase: Secret) -> u64 {
        self.tickets += 1;
        self.waiting = Some(Waiting {
            ticket: self.tickets,
            network: network.to_string(),
            asker: asker.to_string(),
            passphrase,
        });
        self.tickets
    }

    /// The join that left a passphrase is over. Whatever a later join has
    /// left since is not this one's to remove.
    fn take_back(&mut self, ticket: u64) {
        self.waiting.take_if(|waiting| waiting.ticket == ticket);
    }

    /// The passphrase, if `sender` is the iwd it was meant for and
    /// `network` the one it was typed for. Anyone else asking gets
    /// nothing and leaves it in place.
    fn answer(&mut self, network: &str, sender: Option<&str>) -> Option<Secret> {
        if self.waiting.is_none() {
            self.unanswered = Some(network.to_string());
        }
        self.waiting
            .take_if(|waiting| waiting.network == network && Some(waiting.asker.as_str()) == sender)
            .map(|waiting| waiting.passphrase)
    }

    /// Whether a passphrase for `network` was asked for in vain since the
    /// last time this was called.
    fn asked_in_vain(&mut self, network: &str) -> bool {
        self.unanswered.take_if(|asked| asked == network).is_some()
    }

    /// iwd has given up on the request, or is done with the agent.
    fn cancel(&mut self) {
        self.waiting = None;
    }
}

type Slot = Arc<Mutex<Desk>>;

fn slot(slot: &Slot) -> MutexGuard<'_, Desk> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Agent {
    slot: Slot,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "net.connman.iwd.Agent.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Canceled(String),
}

/// A passphrase as the string of a D-Bus reply. zbus serialises it
/// straight from here into the message, so no `String` of it is made on
/// the way, and this copy is wiped when the reply has been built. The
/// message buffer itself belongs to zbus and is only freed.
struct Reply(Secret);

impl Type for Reply {
    const SIGNATURE: &'static Signature = <str as Type>::SIGNATURE;
}

impl Serialize for Reply {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.expose())
    }
}

fn declined<T>() -> Result<T, AgentError> {
    Err(AgentError::Canceled("nothing to offer".to_string()))
}

#[zbus::interface(name = "net.connman.iwd.Agent")]
impl Agent {
    fn request_passphrase(
        &self,
        network: ObjectPath<'_>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<Reply, AgentError> {
        let sender = header.sender().map(|name| name.as_str());
        match slot(&self.slot).answer(network.as_str(), sender) {
            Some(passphrase) => Ok(Reply(passphrase)),
            None => declined(),
        }
    }

    // Enterprise credentials are set up in a provisioning file, not typed
    // into a dialog.
    fn request_private_key_passphrase(
        &self,
        _network: ObjectPath<'_>,
    ) -> Result<String, AgentError> {
        declined()
    }

    fn request_user_name_and_password(
        &self,
        _network: ObjectPath<'_>,
    ) -> Result<(String, String), AgentError> {
        declined()
    }

    fn request_user_password(
        &self,
        _network: ObjectPath<'_>,
        _user: &str,
    ) -> Result<String, AgentError> {
        declined()
    }

    fn cancel(&self, _reason: &str) {
        slot(&self.slot).cancel();
    }

    fn release(&self) {
        slot(&self.slot).cancel();
    }
}

fn refusal(error: &zbus::Error) -> Refusal {
    if bus::denied(error) {
        Refusal::Denied
    } else {
        Refusal::Failed
    }
}

pub struct Iwd {
    bus: Arc<Bus>,
    slot: Slot,
    /// `/sys/class`, or in a debug build a directory laid out like it.
    class: PathBuf,
}

impl Iwd {
    pub fn new(bus: Arc<Bus>, class: PathBuf) -> Iwd {
        Iwd {
            bus,
            slot: Slot::default(),
            class,
        }
    }

    fn device(&self) -> Option<(Vec<Object>, Device)> {
        let Listing::Objects(objects) = self.objects() else {
            return None;
        };
        let device = device(&objects)?;
        Some((objects, device))
    }

    fn objects(&self) -> Listing {
        self.bus
            .objects(SERVICE, &[ADAPTER, DEVICE, STATION, NETWORK])
    }

    /// Export the agent and tell iwd about it. iwd forgets an agent when
    /// its owner leaves the bus, so there is nothing to undo on exit.
    fn register_agent(&self) -> zbus::Result<()> {
        let connection = self
            .bus
            .connection()
            .ok_or_else(|| zbus::Error::Failure("no system bus".to_string()))?;
        connection.object_server().at(
            AGENT_PATH,
            Agent {
                slot: self.slot.clone(),
            },
        )?;
        let path = ObjectPath::try_from(AGENT_PATH)?;
        let registered: zbus::Result<()> = self.bus.call(
            SERVICE,
            MANAGER_PATH,
            AGENT_MANAGER,
            "RegisterAgent",
            &(path,),
        );
        match registered {
            // Ours, from an earlier join.
            Err(error) if bus::error_name(&error) == Some("net.connman.iwd.AlreadyExists") => {
                Ok(())
            }
            other => other,
        }
    }

    fn wifi(&self) -> Wifi {
        let objects = match self.objects() {
            Listing::Objects(objects) => objects,
            Listing::Denied => return Wifi::Denied,
            Listing::Absent => return Wifi::Absent,
        };
        let Some(device) = device(&objects) else {
            return Wifi::Absent;
        };
        let ordered: Vec<(OwnedObjectPath, i16)> = match &device.station {
            Some(_) => self
                .bus
                .call(SERVICE, &device.path, STATION, "GetOrderedNetworks", &())
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let ordered: Vec<(String, i16)> = ordered
            .into_iter()
            .map(|(path, signal)| (path.as_str().to_string(), signal))
            .collect();
        wifi(&objects, &device, &ordered)
    }
}

impl NetworkService for Iwd {
    fn read(&self) -> Reading {
        let all = links::all_addresses();
        let (wired, hardware) = links::read(&self.class.join("net"), &all);
        let mut wifi = self.wifi();
        if let Wifi::On(station) = &mut wifi {
            if station.link == Link::Connected {
                station.addresses = links::addresses_of(&all, &station.interface);
            }
        }
        Reading {
            wifi,
            hardware,
            block: links::block(&self.class.join("rfkill"), "wlan"),
            wired,
        }
    }

    fn set_powered(&self, on: bool) -> Result<(), Refusal> {
        let (_, device) = self.device().ok_or(Refusal::Failed)?;
        let mut result = match &device.adapter {
            Some(adapter) => self.bus.set_flag(SERVICE, adapter, ADAPTER, "Powered", on),
            None => Ok(()),
        };
        // An adapter that is on can still have its device down.
        if on && result.is_ok() {
            result = self
                .bus
                .set_flag(SERVICE, &device.path, DEVICE, "Powered", true);
        }
        result.map_err(|error| {
            eprintln!("sengi-settings: switching Wi-Fi: {error}");
            refusal(&error)
        })
    }

    fn scan(&self) {
        if let Some((_, device)) = self.device() {
            // Busy means a scan is already under way, which will do.
            let _: zbus::Result<()> = self.bus.call(SERVICE, &device.path, STATION, "Scan", &());
        }
    }

    fn join(&self, request: Request) -> Result<(), JoinError> {
        let asker = self.bus.owner(SERVICE).ok_or(JoinError::Unavailable)?;
        // The id came from iwd by way of the UI. Only its own paths go back.
        if !ours(&request.id) {
            return Err(JoinError::Unavailable);
        }
        // The agent is there for every join, typed passphrase or not. iwd
        // asks it about a network it already knows when the stored
        // passphrase has stopped working, and with no agent at all the
        // only way out would be to forget the network by hand.
        let registered = self.register_agent();
        if let Err(error) = &registered {
            eprintln!("sengi-settings: registering with iwd: {error}");
        }
        slot(&self.slot).asked_in_vain(&request.id);
        let mut ticket = None;
        if let Some(passphrase) = request.passphrase {
            if let Err(error) = &registered {
                return Err(if bus::denied(error) {
                    JoinError::Denied
                } else {
                    JoinError::Unavailable
                });
            }
            ticket = Some(slot(&self.slot).put(&request.id, &asker, passphrase));
        }
        let result: zbus::Result<()> = self.bus.call(SERVICE, &request.id, NETWORK, "Connect", &());
        // Whether or not iwd asked for it. Another join may have started
        // while this one was waiting; its passphrase stays.
        if let Some(ticket) = ticket {
            slot(&self.slot).take_back(ticket);
        }
        result.map_err(|error| {
            if slot(&self.slot).asked_in_vain(&request.id) {
                JoinError::NeedsPassphrase
            } else if bus::denied(&error) {
                JoinError::Denied
            } else {
                join_error(bus::error_name(&error))
            }
        })
    }

    fn disconnect(&self) -> Result<(), Refusal> {
        let (_, device) = self.device().ok_or(Refusal::Failed)?;
        let result: zbus::Result<()> =
            self.bus
                .call(SERVICE, &device.path, STATION, "Disconnect", &());
        match result {
            // Nothing was connected: the same end by another road.
            Err(error) if bus::error_name(&error) == Some("net.connman.iwd.NotConnected") => Ok(()),
            other => other.map_err(|error| refusal(&error)),
        }
    }

    fn forget(&self, id: &str) -> Result<(), Refusal> {
        let (objects, _) = self.device().ok_or(Refusal::Failed)?;
        // Already forgotten is forgotten.
        let Some(known) = known_path(&objects, id) else {
            return Ok(());
        };
        let result: zbus::Result<()> = self.bus.call(SERVICE, known, KNOWN, "Forget", &());
        result.map_err(|error| {
            eprintln!("sengi-settings: forgetting a network: {error}");
            refusal(&error)
        })
    }

    fn watch(&self, changed: Notify) {
        self.bus.watch(SERVICE, changed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::testing::{merged, object, path, text};
    use crate::bus::Prop;
    use crate::network::MAX_NETWORKS;

    const WLAN: &str = "/net/connman/iwd/0/4";

    fn adapter(powered: bool) -> Object {
        object(
            "/net/connman/iwd/0",
            ADAPTER,
            &[("Powered", Prop::Bool(powered))],
        )
    }

    fn wlan(powered: bool) -> Object {
        object(
            WLAN,
            DEVICE,
            &[
                ("Name", text("wlan0")),
                ("Powered", Prop::Bool(powered)),
                ("Mode", text("station")),
                ("Adapter", path("/net/connman/iwd/0")),
            ],
        )
    }

    fn station(state: &str, scanning: bool) -> Object {
        merged(
            wlan(true),
            object(
                WLAN,
                STATION,
                &[("State", text(state)), ("Scanning", Prop::Bool(scanning))],
            ),
        )
    }

    fn net(suffix: &str, name: &str, kind: &str, known: bool, connected: bool) -> Object {
        let mut props = vec![
            ("Name", text(name)),
            ("Type", text(kind)),
            ("Connected", Prop::Bool(connected)),
            ("Device", path(WLAN)),
        ];
        if known {
            props.push(("KnownNetwork", path("/net/connman/iwd/486f6d65_psk")));
        }
        object(&format!("{WLAN}/{suffix}"), NETWORK, &props)
    }

    fn ordered(entries: &[(&str, i16)]) -> Vec<(String, i16)> {
        entries
            .iter()
            .map(|(suffix, signal)| (format!("{WLAN}/{suffix}"), *signal))
            .collect()
    }

    fn secret(text: &str) -> Secret {
        Secret::new(text.to_string())
    }

    fn on(wifi: Wifi) -> Station {
        match wifi {
            Wifi::On(station) => station,
            other => panic!("expected Wi-Fi to be on, got {other:?}"),
        }
    }

    const IWD: Option<&str> = Some(":1.7");

    #[test]
    fn the_agent_answers_iwd_about_the_right_network_once() {
        let mut desk = Desk::default();
        desk.put("/net/a_psk", ":1.7", secret("correct horse"));
        let answer = desk.answer("/net/a_psk", IWD).unwrap();
        assert_eq!(answer.expose(), "correct horse");
        // It is gone: a second request gets nothing.
        assert!(desk.answer("/net/a_psk", IWD).is_none());
    }

    #[test]
    fn the_agent_tells_nobody_but_the_iwd_it_was_meant_for() {
        let mut desk = Desk::default();
        desk.put("/net/a_psk", ":1.7", secret("correct horse"));
        assert!(desk.answer("/net/a_psk", Some(":1.99")).is_none());
        assert!(desk.answer("/net/a_psk", None).is_none());
        // And the stranger's asking did not use it up.
        assert!(desk.answer("/net/a_psk", IWD).is_some());
    }

    #[test]
    fn the_agent_gives_a_passphrase_only_to_its_own_network() {
        let mut desk = Desk::default();
        desk.put("/net/a_psk", ":1.7", secret("correct horse"));
        assert!(desk.answer("/net/b_psk", IWD).is_none());
        assert!(desk.answer("/net/a_psk", IWD).is_some());
    }

    #[test]
    fn a_join_that_ends_late_leaves_the_next_ones_passphrase_alone() {
        let mut desk = Desk::default();
        let first = desk.put("/net/a_psk", ":1.7", secret("for network a"));
        let second = desk.put("/net/b_psk", ":1.7", secret("for network b"));
        desk.take_back(first);
        assert_eq!(
            desk.answer("/net/b_psk", IWD).unwrap().expose(),
            "for network b"
        );
        let third = desk.put("/net/b_psk", ":1.7", secret("second try"));
        desk.take_back(second);
        assert!(desk.waiting.is_some());
        desk.take_back(third);
        assert!(desk.waiting.is_none());
    }

    #[test]
    fn the_agent_remembers_being_asked_with_nothing_to_give() {
        let mut desk = Desk::default();
        assert!(!desk.asked_in_vain("/net/a_psk"));
        assert!(desk.answer("/net/a_psk", IWD).is_none());
        assert!(!desk.asked_in_vain("/net/b_psk"));
        assert!(desk.asked_in_vain("/net/a_psk"));
        assert!(!desk.asked_in_vain("/net/a_psk"));
        desk.put("/net/a_psk", ":1.7", secret("correct horse"));
        assert!(desk.answer("/net/a_psk", IWD).is_some());
        assert!(!desk.asked_in_vain("/net/a_psk"));
    }

    #[test]
    fn cancel_clears_the_passphrase() {
        let mut desk = Desk::default();
        desk.put("/net/a_psk", ":1.7", secret("correct horse"));
        desk.cancel();
        assert!(desk.answer("/net/a_psk", IWD).is_none());
    }

    #[test]
    fn no_device_no_wifi() {
        assert_eq!(device(&[]), None);
        assert_eq!(device(&[adapter(true)]), None);
    }

    #[test]
    fn a_powered_off_adapter_is_wifi_off() {
        let objects = [adapter(false), wlan(false)];
        let found = device(&objects).unwrap();
        assert_eq!(found.path, WLAN);
        assert_eq!(found.name, "wlan0");
        assert!(!found.powered);
        assert_eq!(wifi(&objects, &found, &[]), Wifi::Off);
        // Powered device under an adapter that is off: still off.
        let objects = [adapter(false), station("disconnected", false)];
        let found = device(&objects).unwrap();
        assert_eq!(wifi(&objects, &found, &[]), Wifi::Off);
    }

    #[test]
    fn an_access_point_device_is_not_ours() {
        let ap = object(
            "/net/connman/iwd/0/3",
            DEVICE,
            &[("Powered", Prop::Bool(true)), ("Mode", text("ap"))],
        );
        assert_eq!(device(std::slice::from_ref(&ap)), None);
        let objects = [adapter(true), ap, station("disconnected", false)];
        assert_eq!(device(&objects).unwrap().path, WLAN);
    }

    #[test]
    fn networks_are_read_in_order_with_their_details() {
        let objects = [
            adapter(true),
            station("connected", true),
            net("486f6d65_psk", "Home", "psk", true, true),
            net("43616665_open", "Cafe", "open", false, false),
            net("4c6162_8021x", "Lab", "8021x", false, false),
        ];
        let found = device(&objects).unwrap();
        let listed = ordered(&[
            ("43616665_open", -7100),
            ("486f6d65_psk", -5200),
            ("4c6162_8021x", -6000),
            // Listed by the station but gone from the object list.
            ("676f6e65_psk", -4000),
        ]);
        let station = on(wifi(&objects, &found, &listed));
        assert_eq!(station.link, Link::Connected);
        assert!(station.scanning);
        assert_eq!(station.interface, "wlan0");
        let seen: Vec<_> = station
            .networks
            .iter()
            .map(|n| (n.name.as_str(), n.security, n.known, n.connected, n.signal))
            .collect();
        assert_eq!(
            seen,
            [
                ("Home", Security::Passphrase, true, true, -5200),
                ("Lab", Security::Enterprise, false, false, -6000),
                ("Cafe", Security::Open, false, false, -7100),
            ]
        );
    }

    #[test]
    fn station_states() {
        let link = |state: &str| {
            let objects = [station(state, false)];
            device(&objects).unwrap().station.unwrap().0
        };
        assert_eq!(link("connected"), Link::Connected);
        assert_eq!(link("roaming"), Link::Connected);
        assert_eq!(link("connecting"), Link::Connecting);
        assert_eq!(link("disconnecting"), Link::Disconnected);
        assert_eq!(link("something new"), Link::Disconnected);
    }

    #[test]
    fn hostile_names_are_cleaned() {
        let odd_device = merged(
            object(
                WLAN,
                DEVICE,
                &[
                    ("Name", text("../../etc\u{202e}")),
                    ("Powered", Prop::Bool(true)),
                    ("Mode", text("station")),
                ],
            ),
            object(WLAN, STATION, &[("State", text("disconnected"))]),
        );
        let objects = [
            odd_device,
            net("a_open", "Free\u{202e}iFiW\u{7}\n", "open", false, false),
            net("b_open", "\u{200b}\u{0}", "open", false, false),
            net("c_psk", &"long ".repeat(100), "psk", false, false),
        ];
        let found = device(&objects).unwrap();
        let listed = ordered(&[("a_open", -5000), ("b_open", -5100), ("c_psk", -5200)]);
        let station = on(wifi(&objects, &found, &listed));
        // An interface name that is not one is not used to look anything up.
        assert_eq!(station.interface, "");
        assert_eq!(station.networks[0].name, "FreeiFiW");
        assert_eq!(station.networks[1].name, "Unnamed network");
        assert_eq!(
            station.networks[2].name.chars().count(),
            text::MAX_NAME_CHARS
        );
    }

    #[test]
    fn a_flood_of_networks_is_cut_down() {
        let mut objects = vec![station("disconnected", false)];
        let mut listed = Vec::new();
        for i in 0..2000 {
            let suffix = format!("{i:04x}_open");
            objects.push(net(&suffix, &format!("net {i}"), "open", false, false));
            listed.push((format!("{WLAN}/{suffix}"), -5000));
        }
        let found = device(&objects).unwrap();
        assert_eq!(
            on(wifi(&objects, &found, &listed)).networks.len(),
            MAX_NETWORKS
        );
    }

    #[test]
    fn properties_of_the_wrong_type_are_ignored() {
        let odd = object(
            WLAN,
            DEVICE,
            &[("Powered", text("yes")), ("Mode", Prop::Other)],
        );
        assert_eq!(device(&[odd]), None);
        let objects = [
            station("disconnected", false),
            object(
                &format!("{WLAN}/x_open"),
                NETWORK,
                &[("Name", Prop::Bool(true))],
            ),
        ];
        let found = device(&objects).unwrap();
        let listed = ordered(&[("x_open", -5000)]);
        assert!(on(wifi(&objects, &found, &listed)).networks.is_empty());
    }

    #[test]
    fn only_a_stored_network_of_iwds_own_can_be_forgotten() {
        let stranger = object(
            &format!("{WLAN}/evil_psk"),
            NETWORK,
            &[("KnownNetwork", path("/org/freedesktop/login1"))],
        );
        let objects = [
            net("486f6d65_psk", "Home", "psk", true, false),
            net("43616665_open", "Cafe", "open", false, false),
            stranger,
        ];
        assert_eq!(
            known_path(&objects, &format!("{WLAN}/486f6d65_psk")),
            Some("/net/connman/iwd/486f6d65_psk")
        );
        assert_eq!(known_path(&objects, &format!("{WLAN}/43616665_open")), None);
        assert_eq!(known_path(&objects, &format!("{WLAN}/evil_psk")), None);
        assert_eq!(known_path(&objects, "/somewhere/else"), None);
        assert!(!ours("/net/connman/iwd/../x y"));
        assert!(!ours("/org/bluez/hci0"));
    }

    #[test]
    fn connect_errors() {
        let error = |name: &str| join_error(Some(name));
        assert_eq!(error("net.connman.iwd.Failed"), JoinError::Failed);
        assert_eq!(error("net.connman.iwd.InvalidFormat"), JoinError::Failed);
        assert_eq!(error("net.connman.iwd.Aborted"), JoinError::Aborted);
        assert_eq!(error("net.connman.iwd.NoAgent"), JoinError::NeedsPassphrase);
        assert_eq!(error("net.connman.iwd.Timeout"), JoinError::Timeout);
        assert_eq!(
            error("net.connman.iwd.NotConfigured"),
            JoinError::NotConfigured
        );
        assert_eq!(
            error("net.connman.iwd.NotSupported"),
            JoinError::NotSupported
        );
        assert_eq!(
            error("org.freedesktop.DBus.Error.ServiceUnknown"),
            JoinError::Unavailable
        );
        assert_eq!(join_error(None), JoinError::Unavailable);
    }
}
