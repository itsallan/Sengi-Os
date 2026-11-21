//! Bluetooth through BlueZ on the system bus: the adapter's power switch
//! and the names of the devices connected to it.
//!
//! The devices one by one, searching and pairing are in
//! [`pairing`](crate::pairing), for a program that asks for them with the
//! `settings` feature.

use std::sync::{Arc, Mutex};

use crate::bluetooth::{Bluetooth, MAX_DEVICES};
use crate::bus::{self, Bus, Listing, Object, Watch};
use crate::names;
use crate::services::{BluetoothService, Notify, Refusal};

pub(crate) const SERVICE: &str = "org.bluez";
pub(crate) const ADAPTER: &str = "org.bluez.Adapter1";
pub(crate) const DEVICE: &str = "org.bluez.Device1";

/// The first adapter's path and whether it is powered.
pub fn adapter(objects: &[Object]) -> Option<(&str, bool)> {
    objects.iter().find_map(|object| {
        let props = object.interface(ADAPTER)?;
        Some((
            object.path.as_str(),
            bus::flag(props, "Powered").unwrap_or(false),
        ))
    })
}

/// What a program shows for these BlueZ objects: whether there is an
/// adapter, whether it is on, and the names of the connected devices.
pub fn reading(objects: &[Object]) -> Bluetooth {
    let Some((adapter, powered)) = adapter(objects) else {
        return Bluetooth::Absent;
    };
    if !powered {
        return Bluetooth::Off;
    }
    let mut devices: Vec<String> = objects
        .iter()
        .filter_map(|object| {
            let props = object.interface(DEVICE)?;
            let ours = bus::path(props, "Adapter").is_none_or(|path| path == adapter);
            if !ours || bus::flag(props, "Connected") != Some(true) {
                return None;
            }
            // The alias is the name the user gave it, or else its own.
            let name = bus::text(props, "Alias").or_else(|| bus::text(props, "Name"));
            Some(names::name(name.unwrap_or_default(), "Unnamed device"))
        })
        .take(MAX_DEVICES)
        .collect();
    devices.sort();
    Bluetooth::On { devices }
}

/// Bluetooth through BlueZ on the system bus.
pub struct Bluez {
    pub(crate) bus: Arc<Bus>,
    watches: Mutex<Vec<Watch>>,
    /// The pairing under way and where its questions go.
    #[cfg(feature = "settings")]
    pub(crate) desk: crate::pairing::Shared,
}

impl Bluez {
    /// A service over `bus`. Nothing is called until it is first used.
    pub fn new(bus: Arc<Bus>) -> Bluez {
        Bluez {
            bus,
            watches: Mutex::default(),
            #[cfg(feature = "settings")]
            desk: Default::default(),
        }
    }
}

impl BluetoothService for Bluez {
    fn read(&self) -> Bluetooth {
        match self.bus.objects(SERVICE, &[ADAPTER, DEVICE]) {
            Listing::Objects(objects) => reading(&objects),
            Listing::Denied => Bluetooth::Denied,
            Listing::Absent => Bluetooth::Absent,
        }
    }

    fn set_powered(&self, on: bool) {
        // Said on standard error by `try_set_powered`. The tile then
        // shows it still off.
        let _ = self.try_set_powered(on);
    }

    fn try_set_powered(&self, on: bool) -> Result<(), Refusal> {
        let Listing::Objects(objects) = self.bus.objects(SERVICE, &[ADAPTER]) else {
            return Err(Refusal::Failed);
        };
        let (path, _) = adapter(&objects).ok_or(Refusal::Failed)?;
        // Fails when the radio is blocked by a hardware switch or rfkill.
        self.bus
            .set_flag(SERVICE, path, ADAPTER, "Powered", on)
            .map_err(|error| {
                eprintln!("{}: switching Bluetooth: {error}", crate::program_name());
                Refusal::of(&error)
            })
    }

    fn watch(&self, changed: Notify) {
        let watch = self.bus.watch(SERVICE, changed);
        self.watches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(watch);
    }

    fn unwatch(&self) {
        let watches = std::mem::take(
            &mut *self
                .watches
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        for watch in watches {
            watch.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::testing::{object, path, text};
    use crate::bus::Prop;

    fn hci(index: u32, powered: bool) -> Object {
        object(
            &format!("/org/bluez/hci{index}"),
            ADAPTER,
            &[("Powered", Prop::Bool(powered))],
        )
    }

    fn device(adapter: u32, id: &str, alias: &str, connected: bool) -> Object {
        object(
            &format!("/org/bluez/hci{adapter}/dev_{id}"),
            DEVICE,
            &[
                ("Alias", text(alias)),
                ("Connected", Prop::Bool(connected)),
                ("Adapter", path(&format!("/org/bluez/hci{adapter}"))),
            ],
        )
    }

    fn names(bluetooth: Bluetooth) -> Vec<String> {
        match bluetooth {
            Bluetooth::On { devices } => devices,
            other => panic!("expected Bluetooth on, got {other:?}"),
        }
    }

    #[test]
    fn no_adapter() {
        assert_eq!(reading(&[]), Bluetooth::Absent);
        // The service's own root object is not an adapter.
        let root = object("/org/bluez", "org.bluez.AgentManager1", &[]);
        assert_eq!(reading(&[root]), Bluetooth::Absent);
    }

    #[test]
    fn adapter_off() {
        let objects = [hci(0, false), device(0, "AA", "Headphones", false)];
        assert_eq!(reading(&objects), Bluetooth::Off);
    }

    #[test]
    fn only_connected_devices_are_named() {
        let objects = [
            hci(0, true),
            device(0, "CC", "Keyboard", true),
            device(0, "AA", "Headphones", true),
            device(0, "BB", "Old speaker", false),
        ];
        assert_eq!(names(reading(&objects)), ["Headphones", "Keyboard"]);
        assert_eq!(names(reading(&[hci(0, true)])), Vec::<String>::new());
    }

    #[test]
    fn devices_of_another_adapter_are_left_out() {
        let objects = [
            hci(0, true),
            hci(1, true),
            device(0, "AA", "Mine", true),
            device(1, "BB", "On the dongle", true),
        ];
        assert_eq!(names(reading(&objects)), ["Mine"]);
    }

    #[test]
    fn device_names_are_cleaned_and_capped() {
        let mut objects = vec![hci(0, true)];
        objects.push(device(0, "00", "Evil\u{202e}\u{1b}[2J\nname", true));
        objects.push(device(0, "01", "", true));
        assert_eq!(names(reading(&objects)), ["Evil[2J name", "Unnamed device"]);
        for i in 2..40 {
            objects.push(device(0, &format!("{i:02}"), &format!("Device {i}"), true));
        }
        assert_eq!(names(reading(&objects)).len(), MAX_DEVICES);
    }

    #[test]
    fn a_device_without_an_alias_uses_its_name() {
        let plain = object(
            "/org/bluez/hci0/dev_AA",
            DEVICE,
            &[("Name", text("Mouse")), ("Connected", Prop::Bool(true))],
        );
        assert_eq!(names(reading(&[hci(0, true), plain])), ["Mouse"]);
    }
}
