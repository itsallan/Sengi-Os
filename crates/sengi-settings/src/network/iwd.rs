//! The Network page's service on a real machine: Wi-Fi from
//! sengi-system's iwd service, with the agent that hands over a typed
//! passphrase, and around it what the page shows besides: the cabled
//! interfaces, the addresses, and whether rfkill holds the radio off.

use std::path::PathBuf;
use std::sync::Arc;

use sengi_system::services::WifiService;
use sengi_system::{links, machine};

use super::{JoinError, Link, NetworkService, Reading, Refusal, Request, Wifi};
use crate::bus::Bus;
use crate::worker::Notify;

pub struct Iwd {
    wifi: sengi_system::iwd::Iwd,
    /// `/sys/class`, or in a debug build a directory laid out like it.
    class: PathBuf,
}

impl Iwd {
    pub fn new(bus: Arc<Bus>, class: PathBuf) -> Iwd {
        Iwd {
            wifi: sengi_system::iwd::Iwd::new(bus),
            class,
        }
    }
}

impl NetworkService for Iwd {
    fn read(&self) -> Reading {
        let net = self.class.join("net");
        let all = links::all_addresses();
        let wifi = self.wifi.read();
        let addresses = match (&wifi, self.wifi.interface()) {
            (
                Wifi::On {
                    link: Link::Connected,
                    ..
                },
                Some(interface),
            ) => links::addresses_of(&all, &interface),
            _ => Vec::new(),
        };
        Reading {
            wifi,
            addresses,
            hardware: machine::interfaces(&net)
                .iter()
                .any(|interface| interface.kind == machine::LinkKind::Wifi),
            block: machine::block(&self.class.join("rfkill"), "wlan"),
            wired: links::wired(&net, &all),
        }
    }

    fn set_powered(&self, on: bool) -> Result<(), Refusal> {
        self.wifi.try_set_powered(on)
    }

    fn scan(&self) {
        self.wifi.scan();
    }

    fn join(&self, request: Request) -> Result<(), JoinError> {
        self.wifi.join(request)
    }

    fn disconnect(&self) -> Result<(), Refusal> {
        self.wifi.disconnect()
    }

    fn forget(&self, id: &str) -> Result<(), Refusal> {
        self.wifi.forget(id)
    }

    fn watch(&self, changed: Notify) {
        self.wifi.watch(changed);
    }
}
