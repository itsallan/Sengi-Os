//! What the Sengi OS shell needs from the system around it.
//!
//! [`instance`] is the socket by which a panel that is already open is
//! found and told to close, and [`volume`] reads what `wpctl` prints; they
//! need no feature and nothing but std.
//!
//! With the `bus` feature, [`connect`] opens the D-Bus connections by hand
//! so that no helper thread is left waking the machine.
//!
//! With the `services` feature come the services themselves, as quick
//! settings uses them and a settings application can: Wi-Fi through iwd
//! ([`iwd`], with the passphrase agent), Bluetooth through BlueZ
//! ([`bluez`]), the backlight through logind, link state, rfkill and the
//! battery from `/sys` ([`machine`]), and audio through `wpctl` and
//! do-not-disturb through `sengi-notify` ([`programs`]).
//!
//! [`mod@services`] defines what a program needs from each of them as a trait,
//! so it can be given fakes, and runs each on a thread of its own.
//! [`wifi`], [`bluetooth`], [`volume`], [`backlight`] and [`battery`] hold
//! the plain types those traits speak in and the rules that go with them.
//!
//! With the `settings` feature comes what a settings application needs
//! on top of that and the panel does not: leaving and forgetting
//! networks, the cabled interfaces and addresses ([`links`]), and the
//! Bluetooth devices one by one with the agent for pairing ([`pairing`]).
//!
//! Nothing here starts a service that is not running: every call on the
//! system bus carries the no-auto-start flag.

#[cfg(feature = "bus")]
pub mod connect;
pub mod instance;

#[cfg(feature = "services")]
pub mod backlight;
#[cfg(feature = "services")]
pub mod battery;
#[cfg(feature = "services")]
pub mod bluetooth;
#[cfg(feature = "services")]
pub mod bluez;
#[cfg(feature = "services")]
pub mod bus;
pub mod commands;
#[cfg(feature = "services")]
pub mod iwd;
#[cfg(feature = "settings")]
pub mod links;
#[cfg(feature = "services")]
pub mod machine;
#[cfg(feature = "services")]
pub mod names;
#[cfg(feature = "settings")]
pub mod pairing;
#[cfg(feature = "services")]
pub mod programs;
#[cfg(feature = "services")]
pub mod secret;
#[cfg(feature = "services")]
pub mod services;
pub mod volume;
#[cfg(feature = "services")]
pub mod wifi;

#[cfg(test)]
mod testutil;

#[cfg(feature = "services")]
mod real {
    use std::path::PathBuf;
    use std::sync::{Arc, OnceLock};

    use crate::services::Services;
    use crate::{bluez, bus, iwd, machine, programs};

    static PROGRAM: OnceLock<&'static str> = OnceLock::new();

    /// Names the program in what this crate prints to standard error, as
    /// in `sengi-quick: no system bus`. Call it once, first thing; later
    /// calls change nothing.
    pub fn set_program_name(name: &'static str) {
        let _ = PROGRAM.set(name);
    }

    /// The name given to [`set_program_name`], or `sengi-system`.
    pub fn program_name() -> &'static str {
        PROGRAM.get().copied().unwrap_or("sengi-system")
    }

    /// Names a directory to read in place of `/sys/class`, in debug builds
    /// only. For testing the battery and backlight code on a machine that has
    /// neither; the bus that is talked to can be changed the usual way, with
    /// `DBUS_SYSTEM_BUS_ADDRESS`. A release build reads the real `/sys` and
    /// nothing else, so a program cannot be made to show an invented machine.
    #[cfg(debug_assertions)]
    const SYS_CLASS_VARIABLE: &str = "SENGI_SYS_CLASS";

    fn sys_class() -> PathBuf {
        #[cfg(debug_assertions)]
        if let Some(path) = std::env::var_os(SYS_CLASS_VARIABLE)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            return path;
        }
        PathBuf::from("/sys/class")
    }

    /// The real services: iwd, BlueZ and logind on the system bus, PipeWire
    /// and the notification service through their commands, and `/sys`.
    /// Nothing is connected to or read until a service is first used.
    pub fn real_services() -> Services {
        let class = sys_class();
        let bus = Arc::new(bus::Bus::default());
        Services {
            wifi: Arc::new(iwd::Iwd::new(bus.clone())),
            bluetooth: Arc::new(bluez::Bluez::new(bus.clone())),
            audio: Box::new(programs::Wpctl),
            machine: Box::new(machine::Sysfs::new(bus, class)),
            dnd: Box::new(programs::Notify::new()),
        }
    }
}

#[cfg(feature = "services")]
pub use real::{program_name, real_services, set_program_name};
