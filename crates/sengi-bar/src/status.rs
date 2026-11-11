//! What the right-hand side of the bar shows: network, volume and battery.
//!
//! Everything here reads from `/sys` or parses command output, and the
//! parsing is kept apart from the reading so it can be tested.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Network {
    Wired,
    Wifi,
    Offline,
}

impl Network {
    pub fn label(&self) -> &'static str {
        match self {
            Network::Wired => "Wired",
            Network::Wifi => "Wi-Fi",
            Network::Offline => "Offline",
        }
    }
}

/// One entry of `/sys/class/net`.
pub struct Interface {
    pub up: bool,
    pub wireless: bool,
    /// Backed by real hardware. Bridges, veth pairs and tunnels are not, and
    /// say nothing about whether the machine is online.
    pub physical: bool,
}

/// A wired link wins over Wi-Fi when both are up, the same order the
/// routing table prefers them in.
pub fn network(interfaces: &[Interface]) -> Network {
    let live = || interfaces.iter().filter(|i| i.up && i.physical);
    if live().any(|i| !i.wireless) {
        Network::Wired
    } else if live().any(|i| i.wireless) {
        Network::Wifi
    } else {
        Network::Offline
    }
}

pub fn read_interfaces(root: &Path) -> Vec<Interface> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            let state = fs::read_to_string(path.join("operstate")).unwrap_or_default();
            Interface {
                up: state.trim() == "up",
                wireless: path.join("wireless").exists() || path.join("phy80211").exists(),
                physical: path.join("device").exists(),
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    pub percent: u32,
    pub muted: bool,
}

impl Volume {
    pub fn label(&self) -> String {
        if self.muted {
            "Muted".to_string()
        } else {
            format!("Vol {}%", self.percent)
        }
    }
}

/// Parses `wpctl get-volume` output, for example `Volume: 0.40 [MUTED]`.
pub fn parse_volume(output: &str) -> Option<Volume> {
    let rest = output.trim().strip_prefix("Volume:")?.trim();
    let mut parts = rest.split_whitespace();
    let level: f32 = parts.next()?.parse().ok()?;
    Some(Volume {
        percent: (level * 100.0).round().max(0.0) as u32,
        muted: parts.any(|part| part == "[MUTED]"),
    })
}

pub fn read_volume() -> Option<Volume> {
    let output = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_volume(&String::from_utf8_lossy(&output.stdout))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battery {
    pub percent: u32,
    pub charging: bool,
}

/// Below these the bar draws the battery in the warning and danger colours.
const BATTERY_WARNING: u32 = 20;
const BATTERY_CRITICAL: u32 = 10;

impl Battery {
    pub fn label(&self) -> String {
        if self.charging {
            format!("{}% charging", self.percent)
        } else {
            format!("{}%", self.percent)
        }
    }

    pub fn low(&self) -> bool {
        !self.charging && self.percent <= BATTERY_CRITICAL
    }

    pub fn warning(&self) -> bool {
        !self.charging && self.percent <= BATTERY_WARNING && !self.low()
    }
}

pub fn parse_battery(capacity: &str, status: &str) -> Option<Battery> {
    Some(Battery {
        percent: capacity.trim().parse::<u32>().ok()?.min(100),
        charging: status.trim() == "Charging",
    })
}

/// The first battery under `/sys/class/power_supply`, if the machine has one.
pub fn read_battery(root: &Path) -> Option<Battery> {
    let mut names: Vec<_> = fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|entry| {
            fs::read_to_string(entry.path().join("type")).is_ok_and(|t| t.trim() == "Battery")
        })
        .map(|entry| entry.path())
        .collect();
    names.sort();
    let path = names.first()?;
    parse_battery(
        &fs::read_to_string(path.join("capacity")).ok()?,
        &fs::read_to_string(path.join("status")).unwrap_or_default(),
    )
}

/// Finds the connected Wi-Fi network in iwd's object list, as printed by
/// `busctl --json=short`. iwd names a network object after its SSID in hex
/// followed by the security type, so the name is read from the object path
/// of the station's `ConnectedNetwork`.
pub fn parse_wifi_name(objects: &str) -> Option<String> {
    let marker = "\"ConnectedNetwork\":{\"type\":\"o\",\"data\":\"";
    let rest = &objects[objects.find(marker)? + marker.len()..];
    let path = &rest[..rest.find('"')?];
    let (hex, _security) = path.rsplit('/').next()?.rsplit_once('_')?;
    if hex.is_empty() || hex.len() % 2 != 0 || !hex.is_ascii() {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect();
    let name = String::from_utf8_lossy(&bytes?).into_owned();
    (!name.is_empty()).then_some(name)
}

/// Asks iwd which network is connected. Never starts iwd: on a machine with
/// no Wi-Fi it is not running and should stay that way.
pub fn read_wifi_name() -> Option<String> {
    let output = Command::new("busctl")
        .args([
            "--system",
            "--json=short",
            "--auto-start=no",
            "--timeout=2",
            "call",
        ])
        .args(["net.connman.iwd", "/", "org.freedesktop.DBus.ObjectManager"])
        .arg("GetManagedObjects")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_wifi_name(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wired(up: bool) -> Interface {
        Interface {
            up,
            wireless: false,
            physical: true,
        }
    }

    fn wifi(up: bool) -> Interface {
        Interface {
            up,
            wireless: true,
            physical: true,
        }
    }

    fn virtual_link() -> Interface {
        Interface {
            up: true,
            wireless: false,
            physical: false,
        }
    }

    #[test]
    fn loopback_alone_is_offline() {
        assert_eq!(network(&[virtual_link()]), Network::Offline);
    }

    #[test]
    fn wired_wins_over_wifi() {
        assert_eq!(network(&[wifi(true), wired(true)]), Network::Wired);
    }

    #[test]
    fn wifi_when_the_cable_is_out() {
        assert_eq!(network(&[wired(false), wifi(true)]), Network::Wifi);
    }

    #[test]
    fn a_bridge_does_not_count_as_wired() {
        assert_eq!(network(&[virtual_link(), wifi(true)]), Network::Wifi);
        assert_eq!(network(&[virtual_link(), wifi(false)]), Network::Offline);
    }

    /// A throwaway directory laid out like a corner of /sys.
    struct Sysfs(std::path::PathBuf);

    impl Sysfs {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("sengi-bar-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Sysfs(dir)
        }

        fn file(&self, path: &str, contents: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    impl Drop for Sysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn interfaces_are_read_from_sysfs() {
        let sys = Sysfs::new("net");
        sys.file("lo/operstate", "unknown\n");
        sys.file("enp1s0/operstate", "down\n");
        sys.file("enp1s0/device/vendor", "0x8086\n");
        sys.file("wlan0/operstate", "up\n");
        sys.file("wlan0/device/vendor", "0x8086\n");
        sys.file("wlan0/phy80211/name", "phy0\n");
        sys.file("docker0/operstate", "up\n");
        assert_eq!(network(&read_interfaces(&sys.0)), Network::Wifi);
    }

    #[test]
    fn a_missing_sysfs_reads_as_offline() {
        let nowhere = Path::new("/nonexistent/sengi-bar");
        assert_eq!(network(&read_interfaces(nowhere)), Network::Offline);
        assert_eq!(read_battery(nowhere), None);
    }

    #[test]
    fn the_battery_is_found_among_other_supplies() {
        let sys = Sysfs::new("power");
        sys.file("AC0/type", "Mains\n");
        sys.file("BAT0/type", "Battery\n");
        sys.file("BAT0/capacity", "64\n");
        sys.file("BAT0/status", "Charging\n");
        assert_eq!(
            read_battery(&sys.0),
            Some(Battery {
                percent: 64,
                charging: true
            })
        );
    }

    #[test]
    fn volume_is_rounded_to_percent() {
        assert_eq!(
            parse_volume("Volume: 0.40\n"),
            Some(Volume {
                percent: 40,
                muted: false
            })
        );
        assert_eq!(
            parse_volume("Volume: 0.335"),
            Some(Volume {
                percent: 34,
                muted: false
            })
        );
    }

    #[test]
    fn muted_volume() {
        let volume = parse_volume("Volume: 0.65 [MUTED]\n").unwrap();
        assert!(volume.muted);
        assert_eq!(volume.label(), "Muted");
    }

    #[test]
    fn volume_above_one_hundred() {
        assert_eq!(parse_volume("Volume: 1.20").unwrap().label(), "Vol 120%");
    }

    #[test]
    fn nonsense_is_not_a_volume() {
        assert_eq!(parse_volume("Could not connect to PipeWire"), None);
    }

    #[test]
    fn battery_labels() {
        assert_eq!(
            parse_battery("87\n", "Discharging\n").unwrap().label(),
            "87%"
        );
        assert_eq!(
            parse_battery("42", "Charging").unwrap().label(),
            "42% charging"
        );
    }

    #[test]
    fn battery_is_low_only_when_draining() {
        assert!(parse_battery("8", "Discharging").unwrap().low());
        assert!(!parse_battery("8", "Charging").unwrap().low());
        assert!(!parse_battery("60", "Discharging").unwrap().low());
    }

    #[test]
    fn battery_warns_before_it_is_low() {
        let battery = parse_battery("18", "Discharging").unwrap();
        assert!(battery.warning() && !battery.low());
        assert!(!parse_battery("8", "Discharging").unwrap().warning());
    }

    #[test]
    fn odd_battery_readings() {
        assert_eq!(parse_battery("", "Full"), None);
        assert_eq!(parse_battery("250", "Full").unwrap().percent, 100);
    }

    #[test]
    fn wifi_name_comes_from_the_connected_network_path() {
        // "Home WiFi" in hex, WPA-PSK.
        let objects = concat!(
            r#"{"type":"a{oa{sa{sv}}}","data":[{"/net/connman/iwd/0/4":{"net.connman.iwd.Station":"#,
            r#"{"State":{"type":"s","data":"connected"},"ConnectedNetwork":{"type":"o","data":"#,
            r#""/net/connman/iwd/0/4/486f6d652057694669_psk"},"Scanning":{"type":"b","data":false}}}}]}"#
        );
        assert_eq!(parse_wifi_name(objects), Some("Home WiFi".to_string()));
    }

    #[test]
    fn no_wifi_name_when_nothing_is_connected() {
        let objects = r#"{"data":[{"/net/connman/iwd/0/4":{"net.connman.iwd.Station":{"State":{"type":"s","data":"disconnected"}}}}]}"#;
        assert_eq!(parse_wifi_name(objects), None);
        assert_eq!(parse_wifi_name(""), None);
    }
}
