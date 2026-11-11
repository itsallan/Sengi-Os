//! What the kernel says under `/sys/class`: battery, backlight and network
//! links. Brightness is written through logind, the one route open to an
//! ordinary user: it sets a backlight on the seat of the caller's session
//! and needs no polkit rule.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::bus::Bus;
use crate::services::{Block, Hardware, Machine};
use crate::{backlight, battery};

const LOGIND: &str = "org.freedesktop.login1";
/// logind resolves this to the caller's session, or failing that to the
/// user's graphical session.
const SESSION_PATH: &str = "/org/freedesktop/login1/session/auto";
const SESSION: &str = "org.freedesktop.login1.Session";

/// A wired link that is up, and whether there is Wi-Fi hardware at all.
/// Bridges, tunnels and the like have no device behind them and say
/// nothing about either.
fn links(net: &Path) -> (bool, bool) {
    let mut wired = false;
    let mut wifi = false;
    for entry in fs::read_dir(net).into_iter().flatten().flatten().take(64) {
        let path = entry.path();
        if !path.join("device").exists() {
            continue;
        }
        if path.join("wireless").exists() || path.join("phy80211").exists() {
            wifi = true;
        } else if fs::read_to_string(path.join("operstate")).is_ok_and(|state| state.trim() == "up")
        {
            wired = true;
        }
    }
    (wired, wifi)
}

fn has_bluetooth(root: &Path) -> bool {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().starts_with("hci"))
}

/// Whether rfkill holds the radios of `kind` ("wlan", "bluetooth") off. A
/// machine often has two switches for one radio, the laptop's and the
/// card's; any one of them blocks it.
fn block(rfkill: &Path, kind: &str) -> Block {
    let mut found = Block::None;
    for entry in fs::read_dir(rfkill)
        .into_iter()
        .flatten()
        .flatten()
        .take(64)
    {
        let path = entry.path();
        let file = |name: &str| fs::read_to_string(path.join(name)).unwrap_or_default();
        if file("type").trim() != kind {
            continue;
        }
        if file("hard").trim() == "1" {
            return Block::Hard;
        }
        if file("soft").trim() == "1" {
            found = Block::Soft;
        }
    }
    found
}

/// Reads `class`, which is `/sys/class` on a real machine.
pub fn read(class: &Path) -> Hardware {
    let (wired, wifi_device) = links(&class.join("net"));
    Hardware {
        battery: battery::read(&class.join("power_supply")),
        backlight: backlight::read(&class.join("backlight")),
        wired,
        wifi_device,
        bluetooth_device: has_bluetooth(&class.join("bluetooth")),
        wifi_block: block(&class.join("rfkill"), "wlan"),
        bluetooth_block: block(&class.join("rfkill"), "bluetooth"),
    }
}

pub struct Sysfs {
    bus: Arc<Bus>,
    class: PathBuf,
}

impl Sysfs {
    /// `class` is `/sys/class`, or a directory laid out like it.
    pub fn new(bus: Arc<Bus>, class: PathBuf) -> Sysfs {
        Sysfs { bus, class }
    }
}

impl Machine for Sysfs {
    fn read(&self) -> Hardware {
        read(&self.class)
    }

    fn set_backlight(&self, name: &str, raw: u32) {
        let result: zbus::Result<()> = self.bus.call(
            LOGIND,
            SESSION_PATH,
            SESSION,
            "SetBrightness",
            &("backlight", name, raw),
        );
        if let Err(error) = result {
            eprintln!("sengi-quick: setting the brightness: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn an_empty_machine() {
        let sys = TempDir::new();
        assert_eq!(read(sys.path()), Hardware::default());
        assert_eq!(
            read(Path::new("/nonexistent/sengi-quick")),
            Hardware::default()
        );
    }

    #[test]
    fn a_desktop_on_a_cable() {
        let sys = TempDir::new();
        sys.write("net/lo/operstate", "unknown\n");
        sys.write("net/enp1s0/operstate", "up\n");
        sys.write("net/enp1s0/device/vendor", "0x8086\n");
        // A bridge is up but is not a cable.
        sys.write("net/docker0/operstate", "up\n");
        let hardware = read(sys.path());
        assert!(hardware.wired);
        assert!(!hardware.wifi_device && !hardware.bluetooth_device);
        assert!(hardware.battery.is_none() && hardware.backlight.is_none());
    }

    #[test]
    fn radios_held_off_by_rfkill() {
        let sys = TempDir::new();
        // The laptop's own switch and the card's, as most have.
        sys.write("rfkill/rfkill0/type", "wlan\n");
        sys.write("rfkill/rfkill0/soft", "0\n");
        sys.write("rfkill/rfkill0/hard", "0\n");
        sys.write("rfkill/rfkill1/type", "wlan\n");
        sys.write("rfkill/rfkill1/soft", "1\n");
        sys.write("rfkill/rfkill1/hard", "0\n");
        sys.write("rfkill/rfkill2/type", "bluetooth\n");
        sys.write("rfkill/rfkill2/soft", "1\n");
        sys.write("rfkill/rfkill2/hard", "1\n");
        sys.write("rfkill/rfkill3/type", "nfc\n");
        sys.write("rfkill/rfkill3/hard", "1\n");
        let hardware = read(sys.path());
        assert_eq!(hardware.wifi_block, Block::Soft);
        assert_eq!(hardware.bluetooth_block, Block::Hard);

        sys.write("rfkill/rfkill1/soft", "0\n");
        assert_eq!(read(sys.path()).wifi_block, Block::None);
        // A hard block on either switch wins.
        sys.write("rfkill/rfkill1/soft", "1\n");
        sys.write("rfkill/rfkill0/hard", "1\n");
        assert_eq!(read(sys.path()).wifi_block, Block::Hard);
    }

    #[test]
    fn a_laptop() {
        let sys = TempDir::new();
        sys.write("net/enp1s0/operstate", "down\n");
        sys.write("net/enp1s0/device/vendor", "0x8086\n");
        sys.write("net/wlan0/operstate", "up\n");
        sys.write("net/wlan0/device/vendor", "0x8086\n");
        sys.write("net/wlan0/phy80211/name", "phy0\n");
        sys.write("bluetooth/hci0/type", "primary\n");
        sys.write("power_supply/BAT0/type", "Battery\n");
        sys.write("power_supply/BAT0/capacity", "81\n");
        sys.write("power_supply/BAT0/status", "Discharging\n");
        sys.write("backlight/intel_backlight/brightness", "300\n");
        sys.write("backlight/intel_backlight/max_brightness", "1000\n");
        let hardware = read(sys.path());
        // Wi-Fi being up is not a cable.
        assert!(!hardware.wired);
        assert!(hardware.wifi_device && hardware.bluetooth_device);
        assert_eq!(hardware.battery.unwrap().percent, 81);
        assert_eq!(hardware.backlight.unwrap().percent(), 30);
    }
}
