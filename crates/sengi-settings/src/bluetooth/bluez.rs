//! The Bluetooth page's service on a real machine: sengi-system's BlueZ
//! service, with its device list and pairing agent, and around it what
//! the page shows besides: whether there is hardware at all, and whether
//! rfkill holds the radio off.

use std::path::PathBuf;
use std::sync::Arc;

use sengi_system::machine;
use sengi_system::pairing::DeviceService;
use sengi_system::services::BluetoothService as _;

use super::{BluetoothService, PairError, Prompter, Reading};
use crate::bus::Bus;
use crate::network::Refusal;
use crate::worker::Notify;

pub struct Bluez {
    bluez: sengi_system::bluez::Bluez,
    /// `/sys/class`, or in a debug build a directory laid out like it.
    class: PathBuf,
}

impl Bluez {
    pub fn new(bus: Arc<Bus>, class: PathBuf) -> Bluez {
        Bluez {
            bluez: sengi_system::bluez::Bluez::new(bus),
            class,
        }
    }
}

impl BluetoothService for Bluez {
    fn read(&self) -> Reading {
        Reading {
            bluetooth: self.bluez.devices(),
            hardware: machine::has_bluetooth(&self.class.join("bluetooth")),
            block: machine::block(&self.class.join("rfkill"), "bluetooth"),
        }
    }

    fn set_powered(&self, on: bool) -> Result<(), Refusal> {
        self.bluez.try_set_powered(on)
    }

    fn search(&self, on: bool) {
        self.bluez.search(on);
    }

    fn pair(&self, id: &str) -> Result<(), PairError> {
        self.bluez.pair(id)
    }

    fn cancel_pairing(&self, id: &str) {
        self.bluez.cancel_pairing(id);
    }

    fn connect(&self, id: &str) -> Result<(), Refusal> {
        self.bluez.connect(id)
    }

    fn disconnect(&self, id: &str) -> Result<(), Refusal> {
        self.bluez.disconnect(id)
    }

    fn remove(&self, id: &str) -> Result<(), Refusal> {
        self.bluez.remove(id)
    }

    fn watch(&self, changed: Notify) {
        self.bluez.watch(changed);
    }

    fn unwatch(&self) {
        self.bluez.unwatch();
    }

    fn on_prompt(&self, prompter: Prompter) {
        self.bluez.on_prompt(prompter);
    }
}
