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
    pub name: String,
    pub up: bool,
    pub wireless: bool,
}

/// A wired link wins over Wi-Fi when both are up, the same order the
/// routing table prefers them in.
pub fn network(interfaces: &[Interface]) -> Network {
    let live = || interfaces.iter().filter(|i| i.up && i.name != "lo");
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
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let path = entry.path();
            let state = fs::read_to_string(path.join("operstate")).unwrap_or_default();
            Some(Interface {
                name,
                up: state.trim() == "up",
                wireless: path.join("wireless").exists() || path.join("phy80211").exists(),
            })
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

/// At or below this the bar draws the battery in the danger colour.
const BATTERY_LOW: u32 = 15;

impl Battery {
    pub fn label(&self) -> String {
        if self.charging {
            format!("{}% charging", self.percent)
        } else {
            format!("{}%", self.percent)
        }
    }

    pub fn low(&self) -> bool {
        !self.charging && self.percent <= BATTERY_LOW
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

#[cfg(test)]
mod tests {
    use super::*;

    fn interface(name: &str, up: bool, wireless: bool) -> Interface {
        Interface {
            name: name.to_string(),
            up,
            wireless,
        }
    }

    #[test]
    fn loopback_alone_is_offline() {
        assert_eq!(network(&[interface("lo", true, false)]), Network::Offline);
    }

    #[test]
    fn wired_wins_over_wifi() {
        let both = [
            interface("wlan0", true, true),
            interface("enp1s0", true, false),
        ];
        assert_eq!(network(&both), Network::Wired);
    }

    #[test]
    fn wifi_when_the_cable_is_out() {
        let links = [
            interface("enp1s0", false, false),
            interface("wlan0", true, true),
        ];
        assert_eq!(network(&links), Network::Wifi);
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
        assert!(parse_battery("12", "Discharging").unwrap().low());
        assert!(!parse_battery("12", "Charging").unwrap().low());
        assert!(!parse_battery("60", "Discharging").unwrap().low());
    }
}
