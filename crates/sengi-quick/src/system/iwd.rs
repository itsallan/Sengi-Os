//! Wi-Fi through iwd on the system bus.
//!
//! iwd asks for passphrases through an agent: an object the client exports
//! and registers, which iwd calls back in the middle of `Network.Connect`.
//! The passphrase typed into the panel waits in a slot for that one call
//! and is gone after it. iwd keeps what it needs to rejoin the network; the
//! panel stores nothing. See iwd's `doc/agent-api.txt`.

use std::sync::{Arc, Mutex, MutexGuard};

use zbus::zvariant::{ObjectPath, OwnedObjectPath};

use super::bus::{self, Bus, Object};
use crate::secret::Secret;
use crate::services::{Notify, WifiService};
use crate::text;
use crate::wifi::{self, JoinError, Link, Network, Request, Security, Wifi};

const SERVICE: &str = "net.connman.iwd";
const ADAPTER: &str = "net.connman.iwd.Adapter";
const DEVICE: &str = "net.connman.iwd.Device";
const STATION: &str = "net.connman.iwd.Station";
const NETWORK: &str = "net.connman.iwd.Network";
const AGENT_MANAGER: &str = "net.connman.iwd.AgentManager";
const MANAGER_PATH: &str = "/net/connman/iwd";
const AGENT_PATH: &str = "/org/sengios/quick/agent";

/// Networks taken from one scan before they are sorted and cut down.
const MAX_SCANNED: usize = 256;

/// The device the panel controls and where it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub path: String,
    pub adapter: Option<String>,
    pub powered: bool,
    /// Present when the device is up and in station mode.
    pub station: Option<Station>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Station {
    pub link: Link,
    pub scanning: bool,
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
            let station = object.interface(STATION).map(|station| Station {
                link: match bus::text(station, "State") {
                    Some("connected" | "roaming") => Link::Connected,
                    Some("connecting") => Link::Connecting,
                    _ => Link::Disconnected,
                },
                scanning: bus::flag(station, "Scanning").unwrap_or(false),
            });
            Some(Device {
                path: object.path.clone(),
                adapter,
                powered,
                station,
            })
        })
        // Stable sort: one with a station first, otherwise the first path.
        .min_by_key(|device| device.station.is_none())
}

/// What the panel shows for `device`, given the networks its station
/// listed (`ordered`, as from `GetOrderedNetworks`).
pub fn reading(objects: &[Object], device: &Device, ordered: &[(String, i16)]) -> Wifi {
    let Some(station) = device.station.as_ref().filter(|_| device.powered) else {
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
            })
        })
        .collect();
    Wifi::On {
        link: station.link,
        scanning: station.scanning,
        networks: wifi::arrange(networks),
    }
}

/// iwd's errors for a failed `Connect`.
pub fn join_error(name: Option<&str>) -> JoinError {
    match name.and_then(|name| name.strip_prefix("net.connman.iwd.")) {
        // InvalidFormat is a passphrase iwd would not even try.
        Some("Failed" | "InvalidFormat") => JoinError::Failed,
        Some("Aborted" | "Busy" | "InProgress") => JoinError::Aborted,
        Some("Timeout") => JoinError::Timeout,
        Some("NotConfigured") => JoinError::NotConfigured,
        Some("NotSupported") => JoinError::NotSupported,
        _ => JoinError::Unavailable,
    }
}

/// A passphrase waiting to be asked for.
struct Waiting {
    network: String,
    /// The bus name of the iwd the request went to. Nobody else gets it.
    asker: String,
    passphrase: Secret,
}

type Slot = Arc<Mutex<Option<Waiting>>>;

fn slot(slot: &Slot) -> MutexGuard<'_, Option<Waiting>> {
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

fn declined<T>() -> Result<T, AgentError> {
    Err(AgentError::Canceled("nothing to offer".to_string()))
}

#[zbus::interface(name = "net.connman.iwd.Agent")]
impl Agent {
    fn request_passphrase(
        &self,
        network: ObjectPath<'_>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<String, AgentError> {
        let mut slot = slot(&self.slot);
        let sender = header.sender().map(|name| name.as_str());
        let wanted = slot.as_ref().is_some_and(|waiting| {
            waiting.network == network.as_str() && Some(waiting.asker.as_str()) == sender
        });
        // Anyone else asking leaves it in place for iwd.
        match slot.take_if(|_| wanted) {
            Some(waiting) => Ok(waiting.passphrase.expose().to_string()),
            None => declined(),
        }
    }

    // Enterprise credentials are set up in a provisioning file, not typed
    // into the panel.
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
        slot(&self.slot).take();
    }

    fn release(&self) {
        slot(&self.slot).take();
    }
}

pub struct Iwd {
    bus: Arc<Bus>,
    slot: Slot,
}

impl Iwd {
    pub fn new(bus: Arc<Bus>) -> Iwd {
        Iwd {
            bus,
            slot: Slot::default(),
        }
    }

    fn device(&self) -> Option<(Vec<Object>, Device)> {
        let objects = self.bus.objects(SERVICE)?;
        let device = device(&objects)?;
        Some((objects, device))
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
}

impl WifiService for Iwd {
    fn read(&self) -> Wifi {
        let Some((objects, device)) = self.device() else {
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
        reading(&objects, &device, &ordered)
    }

    fn set_powered(&self, on: bool) {
        let Some((_, device)) = self.device() else {
            return;
        };
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
        if let Err(error) = result {
            eprintln!("sengi-quick: switching Wi-Fi: {error}");
        }
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
        if !request.id.starts_with("/net/connman/iwd/")
            || ObjectPath::try_from(request.id.as_str()).is_err()
        {
            return Err(JoinError::Unavailable);
        }
        if let Some(passphrase) = request.passphrase {
            if let Err(error) = self.register_agent() {
                eprintln!("sengi-quick: registering with iwd: {error}");
                return Err(JoinError::Unavailable);
            }
            *slot(&self.slot) = Some(Waiting {
                network: request.id.clone(),
                asker,
                passphrase,
            });
        }
        let result: zbus::Result<()> = self.bus.call(SERVICE, &request.id, NETWORK, "Connect", &());
        // Whether or not iwd asked for it.
        slot(&self.slot).take();
        result.map_err(|error| join_error(bus::error_name(&error)))
    }

    fn watch(&self, changed: Notify) {
        self.bus.watch(SERVICE, changed);
    }
}

#[cfg(test)]
mod tests {
    use super::bus::testing::{object, path, text};
    use super::bus::Prop;
    use super::*;

    const WLAN: &str = "/net/connman/iwd/0/4";

    fn merged(mut base: Object, extra: Object) -> Object {
        base.interfaces.extend(extra.interfaces);
        base
    }

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
            props.push(("KnownNetwork", path("/net/connman/iwd/known")));
        }
        object(&format!("{WLAN}/{suffix}"), NETWORK, &props)
    }

    fn ordered(entries: &[(&str, i16)]) -> Vec<(String, i16)> {
        entries
            .iter()
            .map(|(suffix, signal)| (format!("{WLAN}/{suffix}"), *signal))
            .collect()
    }

    #[test]
    fn no_device_no_wifi() {
        assert_eq!(device(&[]), None);
        assert_eq!(device(&[adapter(true)]), None);
    }

    #[test]
    fn a_powered_off_adapter_is_wifi_off() {
        let objects = [adapter(false), wlan(false)];
        let device = device(&objects).unwrap();
        assert_eq!(device.path, WLAN);
        assert_eq!(device.adapter.as_deref(), Some("/net/connman/iwd/0"));
        assert!(!device.powered);
        assert_eq!(reading(&objects, &device, &[]), Wifi::Off);

        // Powered device under an adapter that is off: still off.
        let objects = [adapter(false), station("disconnected", false)];
        let device = super::device(&objects).unwrap();
        assert_eq!(reading(&objects, &device, &[]), Wifi::Off);
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
    fn the_device_with_a_station_is_preferred() {
        let idle = object(
            "/net/connman/iwd/0/2",
            DEVICE,
            &[("Powered", Prop::Bool(false)), ("Mode", text("station"))],
        );
        let objects = [idle, station("connected", false)];
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
        let device = device(&objects).unwrap();
        let listed = ordered(&[
            ("43616665_open", -7100),
            ("486f6d65_psk", -5200),
            ("4c6162_8021x", -6000),
            // Listed by the station but gone from the object list.
            ("676f6e65_psk", -4000),
        ]);
        let Wifi::On {
            link,
            scanning,
            networks,
        } = reading(&objects, &device, &listed)
        else {
            panic!("expected Wi-Fi to be on");
        };
        assert_eq!(link, Link::Connected);
        assert!(scanning);
        let seen: Vec<_> = networks
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
        assert_eq!(networks[0].id, format!("{WLAN}/486f6d65_psk"));
    }

    #[test]
    fn station_states() {
        let link = |state: &str| {
            let objects = [station(state, false)];
            device(&objects).unwrap().station.unwrap().link
        };
        assert_eq!(link("connected"), Link::Connected);
        assert_eq!(link("roaming"), Link::Connected);
        assert_eq!(link("connecting"), Link::Connecting);
        assert_eq!(link("disconnecting"), Link::Disconnected);
        assert_eq!(link("disconnected"), Link::Disconnected);
        assert_eq!(link("something new"), Link::Disconnected);
    }

    #[test]
    fn hostile_network_names_are_cleaned() {
        let objects = [
            station("disconnected", false),
            net("a_open", "Free\u{202e}iFiW\u{7}\n", "open", false, false),
            net("b_open", "\u{200b}\u{0}", "open", false, false),
            net("c_psk", &"long ".repeat(100), "psk", false, false),
        ];
        let device = device(&objects).unwrap();
        let listed = ordered(&[("a_open", -5000), ("b_open", -5100), ("c_psk", -5200)]);
        let Wifi::On { networks, .. } = reading(&objects, &device, &listed) else {
            panic!("expected Wi-Fi to be on");
        };
        assert_eq!(networks[0].name, "FreeiFiW");
        assert_eq!(networks[1].name, "Unnamed network");
        assert_eq!(networks[2].name.chars().count(), text::MAX_NAME_CHARS);
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
        let device = device(&objects).unwrap();
        let Wifi::On { networks, .. } = reading(&objects, &device, &listed) else {
            panic!("expected Wi-Fi to be on");
        };
        assert_eq!(networks.len(), wifi::MAX_NETWORKS);
    }

    #[test]
    fn properties_of_the_wrong_type_are_ignored() {
        let odd = object(
            WLAN,
            DEVICE,
            &[("Powered", text("yes")), ("Mode", Prop::Other)],
        );
        // Mode is unreadable, so this is not known to be a station.
        assert_eq!(device(&[odd]), None);
        let objects = [
            station("disconnected", false),
            object(
                &format!("{WLAN}/x_open"),
                NETWORK,
                &[("Name", Prop::Bool(true))],
            ),
        ];
        let device = device(&objects).unwrap();
        let listed = ordered(&[("x_open", -5000)]);
        let Wifi::On { networks, .. } = reading(&objects, &device, &listed) else {
            panic!("expected Wi-Fi to be on");
        };
        assert!(networks.is_empty());
    }

    #[test]
    fn connect_errors() {
        assert_eq!(
            join_error(Some("net.connman.iwd.Failed")),
            JoinError::Failed
        );
        assert_eq!(
            join_error(Some("net.connman.iwd.InvalidFormat")),
            JoinError::Failed
        );
        assert_eq!(
            join_error(Some("net.connman.iwd.Aborted")),
            JoinError::Aborted
        );
        assert_eq!(
            join_error(Some("net.connman.iwd.Timeout")),
            JoinError::Timeout
        );
        assert_eq!(
            join_error(Some("net.connman.iwd.NotConfigured")),
            JoinError::NotConfigured
        );
        assert_eq!(
            join_error(Some("net.connman.iwd.NotSupported")),
            JoinError::NotSupported
        );
        assert_eq!(
            join_error(Some("org.freedesktop.DBus.Error.ServiceUnknown")),
            JoinError::Unavailable
        );
        assert_eq!(join_error(None), JoinError::Unavailable);
    }
}
