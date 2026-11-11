//! The services behind the pages: the real ones, and in debug builds the
//! made-up ones for looking at states the machine at hand cannot produce.

use std::path::PathBuf;
use std::sync::Arc;

use crate::bus::Bus;
use crate::network::{self, NetworkService};

pub struct Services {
    pub network: Arc<dyn NetworkService>,
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
        network: Arc::new(network::iwd::Iwd::new(bus, class)),
    }
}

#[cfg(debug_assertions)]
pub const DEMOS: &[&str] = &["network", "network-off", "network-none"];

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
        _ => return None,
    };
    Some((page, services))
}
