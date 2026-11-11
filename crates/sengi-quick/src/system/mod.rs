//! The real services: iwd, BlueZ and logind on the system bus, PipeWire
//! and the notification service through their commands, and `/sys`.

pub mod bluez;
pub mod bus;
pub mod iwd;
pub mod machine;
pub mod programs;

use std::path::PathBuf;
use std::sync::Arc;

use crate::services::Services;

/// Names a directory to read in place of `/sys/class`. For testing the
/// battery and backlight code on a machine that has neither; the bus that
/// is talked to can be changed the usual way, with
/// `DBUS_SYSTEM_BUS_ADDRESS`.
const SYS_CLASS_VARIABLE: &str = "SENGI_QUICK_SYS_CLASS";

pub fn services() -> Services {
    let class = std::env::var_os(SYS_CLASS_VARIABLE)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/sys/class"));
    let bus = Arc::new(bus::Bus::default());
    Services {
        wifi: Arc::new(iwd::Iwd::new(bus.clone())),
        bluetooth: Arc::new(bluez::Bluez::new(bus.clone())),
        audio: Box::new(programs::Wpctl),
        machine: Box::new(machine::Sysfs::new(bus, class)),
        dnd: Box::new(programs::Notify::new()),
    }
}
