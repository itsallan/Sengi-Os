//! Made-up services, for looking at every state of the panel on a machine
//! with no Wi-Fi, Bluetooth, backlight or battery. `sengi-quick --demo NAME`
//! runs the real panel and workers over these. They keep state, so the
//! switches and sliders can be tried; nothing outside the process changes.

use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use crate::backlight::Backlight;
use crate::battery::{Battery, Charge};
use crate::bluetooth::Bluetooth;
use crate::services::{
    AudioService, BluetoothService, DndService, Hardware, Machine, Notify, Services, WifiService,
};
use crate::volume::Volume;
use crate::wifi::{self, JoinError, Link, Network, Request, Security, Wifi};

pub const SCENARIOS: &[&str] = &[
    "connected",
    "networks",
    "open-network",
    "passphrase",
    "wrong-passphrase",
    "joining",
    "changed-passphrase",
    "failed",
    "bluetooth",
    "low-battery",
    "charging",
    "off",
    "desktop",
    "hostile",
];

/// The one passphrase the made-up networks accept.
pub const PASSPHRASE: &str = "sengi-demo";

/// Something the user would do, done for them once the panel is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Click a row of the network list.
    Choose(usize),
    /// Type a passphrase and press Return.
    Submit(&'static str),
}

pub struct Demo {
    pub services: Services,
    pub script: Vec<Action>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Default)]
struct Watchers(Mutex<Vec<Notify>>);

impl Watchers {
    fn add(&self, notify: Notify) {
        lock(&self.0).push(notify);
    }
    fn tell(&self) {
        for notify in lock(&self.0).iter() {
            notify();
        }
    }
}

struct FakeWifi {
    powered: Mutex<bool>,
    link: Mutex<Link>,
    networks: Mutex<Vec<Network>>,
    present: bool,
    /// How long a join takes to answer.
    delay: Duration,
    watchers: Watchers,
}

impl WifiService for FakeWifi {
    fn read(&self) -> Wifi {
        if !self.present {
            Wifi::Absent
        } else if !*lock(&self.powered) {
            Wifi::Off
        } else {
            Wifi::On {
                link: *lock(&self.link),
                scanning: false,
                networks: wifi::arrange(lock(&self.networks).clone()),
            }
        }
    }

    fn set_powered(&self, on: bool) {
        *lock(&self.powered) = on;
        self.watchers.tell();
    }

    fn scan(&self) {}

    fn join(&self, request: Request) -> Result<(), JoinError> {
        thread::sleep(self.delay);
        let mut networks = lock(&self.networks);
        let target = networks
            .iter()
            .position(|network| network.id == request.id)
            .ok_or(JoinError::Unavailable)?;
        let typed = request.passphrase.as_ref().map(|secret| secret.expose());
        if networks[target].name == "Attic repeater" {
            return Err(JoinError::Timeout);
        }
        let accepted = match networks[target].security {
            Security::Open => true,
            _ if typed.is_some() => typed == Some(PASSPHRASE),
            // "Old router" is known, and has since changed its passphrase.
            _ => networks[target].known && networks[target].name != "Old router",
        };
        if !accepted {
            return Err(JoinError::Failed);
        }
        for network in networks.iter_mut() {
            network.connected = false;
        }
        networks[target].connected = true;
        networks[target].known = true;
        *lock(&self.link) = Link::Connected;
        drop(networks);
        self.watchers.tell();
        Ok(())
    }

    fn watch(&self, changed: Notify) {
        self.watchers.add(changed);
    }
}

struct FakeBluetooth {
    state: Mutex<Bluetooth>,
    devices: Vec<String>,
    watchers: Watchers,
}

impl BluetoothService for FakeBluetooth {
    fn read(&self) -> Bluetooth {
        lock(&self.state).clone()
    }

    fn set_powered(&self, on: bool) {
        let mut state = lock(&self.state);
        if *state != Bluetooth::Absent {
            *state = if on {
                Bluetooth::On {
                    devices: self.devices.clone(),
                }
            } else {
                Bluetooth::Off
            };
        }
        drop(state);
        self.watchers.tell();
    }

    fn watch(&self, changed: Notify) {
        self.watchers.add(changed);
    }
}

struct FakeAudio(Mutex<Option<Volume>>);

impl AudioService for FakeAudio {
    fn read(&self) -> Option<Volume> {
        *lock(&self.0)
    }
    fn set_volume(&self, percent: u32) {
        if let Some(volume) = lock(&self.0).as_mut() {
            volume.percent = percent;
        }
    }
    fn set_muted(&self, muted: bool) {
        if let Some(volume) = lock(&self.0).as_mut() {
            volume.muted = muted;
        }
    }
}

struct FakeMachine(Mutex<Hardware>);

impl Machine for FakeMachine {
    fn read(&self) -> Hardware {
        lock(&self.0).clone()
    }
    fn set_backlight(&self, _name: &str, raw: u32) {
        if let Some(backlight) = lock(&self.0).backlight.as_mut() {
            backlight.brightness = raw;
        }
    }
}

struct FakeDnd {
    installed: bool,
    on: Mutex<Option<bool>>,
}

impl DndService for FakeDnd {
    fn available(&self) -> bool {
        self.installed
    }
    fn read(&self) -> Option<bool> {
        *lock(&self.on)
    }
    fn set(&self, on: bool) {
        *lock(&self.on) = Some(on);
    }
}

fn network(name: &str, security: Security, known: bool, signal: i16) -> Network {
    Network {
        id: format!("/demo/{name}"),
        name: crate::text::name(name, "Unnamed network"),
        security,
        known,
        connected: false,
        signal,
    }
}

fn neighbourhood() -> Vec<Network> {
    use Security::*;
    vec![
        network("Elephant Shrew", Passphrase, true, -4800),
        network("Old router", Passphrase, true, -7600),
        network("Bakery Guest", Open, false, -6300),
        network("Flat 3B", Passphrase, false, -5600),
        network("Campus", Enterprise, false, -6900),
        network("Printer-4F2A", Open, false, -8300),
        network("Number 12 upstairs", Passphrase, false, -8700),
        network("Hall", Passphrase, false, -7900),
    ]
}

fn battery(percent: u32, charge: Charge, minutes: Option<u32>) -> Option<Battery> {
    Some(Battery {
        percent,
        charge,
        minutes,
    })
}

/// The made-up machine for `name`, or `None` if there is no such scenario.
pub fn scenario(name: &str) -> Option<Demo> {
    let mut hardware = Hardware {
        battery: battery(72, Charge::Discharging, Some(245)),
        backlight: Some(Backlight {
            name: "demo".to_string(),
            brightness: 650,
            max: 1000,
        }),
        wired: false,
        wifi_device: true,
        bluetooth_device: true,
    };
    let mut networks = neighbourhood();
    let mut wifi_on = true;
    let mut wifi_present = true;
    let mut link = Link::Connected;
    let mut delay = Duration::from_millis(400);
    let devices = vec!["Trail headphones".to_string(), "Keyboard K3".to_string()];
    let mut bluetooth = Bluetooth::On {
        devices: Vec::new(),
    };
    let mut volume = Some(Volume {
        percent: 40,
        muted: false,
    });
    let mut dnd = None;
    let mut script = Vec::new();

    let disconnect = |networks: &mut Vec<Network>, link: &mut Link| {
        networks[0].known = false;
        networks[1].known = false;
        *link = Link::Disconnected;
    };

    match name {
        "connected" => networks[0].connected = true,
        "networks" => {
            link = Link::Disconnected;
        }
        "open-network" => {
            networks[0].connected = true;
            // Bakery Guest, open and never joined.
            script.push(Action::Choose(2));
        }
        "passphrase" => {
            networks[0].connected = true;
            // Flat 3B, the strongest network that is not known.
            script.push(Action::Choose(1));
        }
        "wrong-passphrase" => {
            networks[0].connected = true;
            script.push(Action::Choose(1));
            script.push(Action::Submit("not-the-passphrase"));
        }
        "joining" => {
            disconnect(&mut networks, &mut link);
            delay = Duration::from_secs(3600);
            script.push(Action::Choose(0));
            script.push(Action::Submit(PASSPHRASE));
        }
        "changed-passphrase" => {
            link = Link::Disconnected;
            networks[0].signal = -9000;
            // Old router, known, refuses what iwd has stored for it.
            script.push(Action::Choose(0));
        }
        "failed" => {
            link = Link::Disconnected;
            networks.insert(0, network("Attic repeater", Security::Open, true, -4000));
            script.push(Action::Choose(0));
        }
        "bluetooth" => {
            networks[0].connected = true;
            bluetooth = Bluetooth::On {
                devices: devices.clone(),
            };
        }
        "low-battery" => {
            networks[0].connected = true;
            hardware.battery = battery(7, Charge::Discharging, Some(14));
            hardware.backlight.as_mut()?.brightness = 200;
        }
        "charging" => {
            networks[0].connected = true;
            hardware.battery = battery(46, Charge::Charging, Some(52));
            dnd = Some(true);
        }
        "off" => {
            wifi_on = false;
            bluetooth = Bluetooth::Off;
            volume = Some(Volume {
                percent: 65,
                muted: true,
            });
            hardware.battery = battery(100, Charge::Full, None);
        }
        "desktop" => {
            wifi_present = false;
            bluetooth = Bluetooth::Absent;
            hardware = Hardware {
                wired: true,
                ..Hardware::default()
            };
        }
        "hostile" => {
            link = Link::Disconnected;
            networks = vec![
                network(
                    "Free\u{202e}iFiW \u{7}\u{1b}[31m",
                    Security::Open,
                    false,
                    -5000,
                ),
                network(&"W".repeat(32), Security::Passphrase, true, -5500),
                network("\u{200b}\u{0}", Security::Open, false, -6000),
                network(
                    "مرحبا بالعالم, a mixed direction name",
                    Security::Passphrase,
                    false,
                    -7000,
                ),
            ];
            bluetooth = Bluetooth::On {
                devices: vec![
                    crate::text::name(&"An extremely long device name ".repeat(4), ""),
                    "Second".to_string(),
                    "Third".to_string(),
                ],
            };
        }
        _ => return None,
    }

    Some(Demo {
        services: Services {
            wifi: Arc::new(FakeWifi {
                powered: Mutex::new(wifi_on),
                link: Mutex::new(link),
                networks: Mutex::new(networks),
                present: wifi_present,
                delay,
                watchers: Watchers::default(),
            }),
            bluetooth: Arc::new(FakeBluetooth {
                state: Mutex::new(bluetooth),
                devices,
                watchers: Watchers::default(),
            }),
            audio: Box::new(FakeAudio(Mutex::new(volume))),
            machine: Box::new(FakeMachine(Mutex::new(hardware))),
            dnd: Box::new(FakeDnd {
                installed: true,
                on: Mutex::new(dnd),
            }),
        },
        script,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::Secret;

    #[test]
    fn every_listed_scenario_exists() {
        for name in SCENARIOS {
            assert!(scenario(name).is_some(), "{name}");
        }
        assert!(scenario("no such thing").is_none());
    }

    #[test]
    fn the_made_up_network_takes_only_its_passphrase() {
        let mut demo = scenario("passphrase").unwrap();
        demo.script.clear();
        let wifi = demo.services.wifi;
        let join = |passphrase: &str| {
            wifi.join(Request {
                id: "/demo/Flat 3B".to_string(),
                passphrase: Some(Secret::new(passphrase.to_string())),
            })
        };
        assert_eq!(join("a wrong guess"), Err(JoinError::Failed));
        assert_eq!(join(PASSPHRASE), Ok(()));
        let Wifi::On { networks, .. } = wifi.read() else {
            panic!("expected Wi-Fi to be on");
        };
        assert_eq!(networks[0].name, "Flat 3B");
        assert!(networks[0].connected && networks[0].known);
        assert_eq!(networks.iter().filter(|n| n.connected).count(), 1);
    }
}
