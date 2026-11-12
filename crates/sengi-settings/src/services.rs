//! The services behind the pages: the real ones, and in debug builds the
//! made-up ones for looking at states the machine at hand cannot produce.

use std::path::PathBuf;
use std::sync::Arc;

use crate::bluetooth::{self, BluetoothService};
use crate::bus::Bus;
use crate::display::{self, DisplayService};
use crate::network::{self, NetworkService};
use crate::sound::{self, AudioDevices};

pub struct Services {
    pub network: Arc<dyn NetworkService>,
    pub bluetooth: Arc<dyn BluetoothService>,
    pub sound: Arc<dyn AudioDevices>,
    pub display: Arc<dyn DisplayService>,
    /// For the pages that only read a few properties off the bus.
    pub bus: Arc<Bus>,
    pub class: PathBuf,
}

/// Names a directory to read in place of `/sys/class`, in debug builds
/// only, for trying the pages on a machine without the hardware. The bus
/// that is talked to can be changed the usual way, with
/// `DBUS_SYSTEM_BUS_ADDRESS`. A release build reads the real `/sys` and
/// nothing else, so Settings cannot be made to show an invented machine.
#[cfg(debug_assertions)]
const SYS_CLASS_VARIABLE: &str = "SENGI_SETTINGS_SYS_CLASS";

pub fn sys_class() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os(SYS_CLASS_VARIABLE)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return path;
    }
    PathBuf::from("/sys/class")
}

pub fn real() -> Services {
    let class = sys_class();
    let bus = Arc::new(Bus::default());
    Services {
        network: Arc::new(network::iwd::Iwd::new(bus.clone(), class.clone())),
        bluetooth: Arc::new(bluetooth::bluez::Bluez::new(bus.clone(), class.clone())),
        display: Arc::new(display::wlr::Wlr::new(bus.clone(), class.clone())),
        bus,
        class,
        sound: Arc::new(sengi_system::audio::Pipewire::default()),
    }
}

#[cfg(debug_assertions)]
pub const DEMOS: &[&str] = &[
    "network",
    "network-off",
    "network-none",
    "bluetooth",
    "bluetooth-off",
    "sound",
    "display",
];

/// The services of a made-up machine, and the page that shows them off.
#[cfg(debug_assertions)]
pub fn demo(name: &str) -> Option<(crate::cli::Page, Services)> {
    use crate::cli::Page;
    use network::testing::{network as net, Fake};
    use network::{Security, Wired};
    use std::time::Duration;

    let mut services = real();
    let page = match name {
        "network" | "network-off" | "network-none" => {
            let mut networks = vec![
                net(
                    "/n/home",
                    "Elephant Shrew",
                    Security::Passphrase,
                    true,
                    -4800,
                ),
                net("/n/office", "Office 5G", Security::Passphrase, true, -6400),
                net("/n/new", "Neighbours", Security::Passphrase, false, -6600),
                net("/n/cafe", "Cafe Guest", Security::Open, false, -7200),
                net("/n/lab", "eduroam", Security::Enterprise, false, -7600),
                net("/n/far", "Far Away", Security::Passphrase, false, -8800),
                net(
                    "/n/evil",
                    &crate::text::name(
                        "Free\u{202e}iFiW \u{7}Wi-Fi with a very long name indeed",
                        "x",
                    ),
                    Security::Open,
                    false,
                    -6900,
                ),
            ];
            networks[0].connected = name == "network";
            let fake = Fake::with(if name == "network-none" {
                Vec::new()
            } else {
                networks
            });
            {
                let mut state = fake.state.lock().unwrap_or_else(|p| p.into_inner());
                state.powered = name != "network-off";
                state.delay = Duration::from_secs(3);
                state.accepts = vec![("/n/new".to_string(), "correct horse".to_string())];
                state.wired = vec![Wired {
                    interface: "enp3s0".to_string(),
                    up: name == "network",
                    addresses: vec!["10.0.2.15".to_string()],
                }];
            }
            services.network = fake;
            Page::Network
        }
        "bluetooth" | "bluetooth-off" => {
            use bluetooth::testing::{device, Fake};
            use bluetooth::{Kind, Question};
            let fake = Fake::with(vec![
                device("/d/head", "Trail headphones", Kind::Audio, true, true),
                device("/d/keys", "Keyboard K3", Kind::Keyboard, true, false),
            ]);
            {
                let mut state = fake.lock();
                state.powered = name == "bluetooth";
                state.delay = Duration::from_millis(1500);
                state.hidden = vec![
                    device("/d/mouse", "Travel mouse", Kind::Mouse, false, false),
                    device("/d/phone", "Allan's phone", Kind::Other, false, false),
                    device("/d/speaker", "Kitchen speaker", Kind::Audio, false, false),
                    device("/d/old", "Old headset", Kind::Audio, false, false),
                ];
                state.asks = vec![
                    ("/d/phone".into(), Question::ConfirmPasskey(482_913), None),
                    ("/d/old".into(), Question::EnterPin, Some("0000".into())),
                    ("/d/keys".into(), Question::ShowPasskey(271_828), None),
                ];
            }
            services.bluetooth = fake;
            Page::Bluetooth
        }
        "sound" => {
            use sound::testing::{node, Fake};
            services.sound = Fake::with(sound::Audio {
                outputs: vec![
                    node(40, "Built-in speakers", 65, true),
                    node(41, "HDMI / DisplayPort", 100, false),
                    node(42, "Trail headphones", 40, false),
                ],
                inputs: vec![node(50, "Built-in microphone", 80, true)],
            });
            Page::Sound
        }
        "display" => {
            use display::testing::{laptop, mode, Fake};
            let external = display::Output {
                name: "HDMI-A-1".to_string(),
                title: "Dell U2720Q".to_string(),
                modes: vec![
                    mode(3840, 2160, 60),
                    mode(2560, 1440, 60),
                    mode(1920, 1080, 60),
                ],
                current: Some(mode(3840, 2160, 60)),
                scale: 1500,
                ..laptop()
            };
            let fake = Fake::with(vec![laptop(), external]);
            *fake.backlight.lock().unwrap_or_else(|p| p.into_inner()) =
                display::backlight::parse("intel_backlight", "4800", "7500");
            services.display = fake;
            Page::Display
        }
        _ => return None,
    };
    Some((page, services))
}
