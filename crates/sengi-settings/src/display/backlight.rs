//! The display backlight, read from `/sys/class/backlight`.
//!
//! This is sengi-quick's `backlight.rs`, to come from sengi-system.
//! Reading is open to everyone. Writing is not, so changes go through
//! logind's `Session.SetBrightness`, which sets the brightness of a device
//! on the caller's own seat and needs no polkit rule.

use std::fs;
use std::path::Path;

use crate::bus::Bus;

/// The slider never goes below this. At zero many panels switch off, and
/// the slider that would bring them back could no longer be seen.
pub const MIN_PERCENT: u32 = 5;

const LOGIND: &str = "org.freedesktop.login1";
/// logind resolves this to the caller's session.
const SESSION_PATH: &str = "/org/freedesktop/login1/session/auto";
const SESSION: &str = "org.freedesktop.login1.Session";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlight {
    /// The device's name under `/sys/class/backlight`.
    pub name: String,
    pub brightness: u32,
    pub max: u32,
}

pub fn parse(name: &str, brightness: &str, max: &str) -> Option<Backlight> {
    let max: u32 = max.trim().parse().ok()?;
    let brightness: u32 = brightness.trim().parse().ok()?;
    // The name goes to logind as it is, so it has to look like a device.
    let plain = !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'));
    (max > 0 && plain).then(|| Backlight {
        name: name.to_string(),
        brightness: brightness.min(max),
        max,
    })
}

impl Backlight {
    /// The lowest raw value the slider will set: `MIN_PERCENT`, and never
    /// zero even on a device with only a handful of steps.
    pub fn floor(&self) -> u32 {
        (u64::from(self.max) * u64::from(MIN_PERCENT))
            .div_ceil(100)
            .max(1) as u32
    }

    pub fn percent(&self) -> u32 {
        ((u64::from(self.brightness) * 100 + u64::from(self.max) / 2) / u64::from(self.max)) as u32
    }

    /// The raw value for a slider position.
    pub fn raw(&self, percent: u32) -> u32 {
        let percent = u64::from(percent.min(100));
        let raw = ((percent * u64::from(self.max) + 50) / 100) as u32;
        raw.clamp(self.floor(), self.max)
    }
}

/// Firmware interfaces know the panel best, then platform drivers; the raw
/// interface of a graphics card comes last. The kernel documents this order.
fn rank(kind: &str) -> u32 {
    match kind.trim() {
        "firmware" => 0,
        "platform" => 1,
        "raw" => 2,
        _ => 3,
    }
}

/// The backlight to control, if the machine has one.
pub fn read(root: &Path) -> Option<Backlight> {
    let mut found: Vec<(u32, Backlight)> = fs::read_dir(root)
        .ok()?
        .flatten()
        .take(16)
        .filter_map(|entry| {
            let path = entry.path();
            let file = |name: &str| fs::read_to_string(path.join(name)).ok();
            let backlight = parse(
                entry.file_name().to_str()?,
                &file("brightness")?,
                &file("max_brightness")?,
            )?;
            Some((rank(&file("type").unwrap_or_default()), backlight))
        })
        .collect();
    found.sort_by(|a, b| (a.0, &a.1.name).cmp(&(b.0, &b.1.name)));
    found.into_iter().next().map(|(_, backlight)| backlight)
}

/// Ask logind to set the brightness of `name`.
pub fn set(bus: &Bus, name: &str, raw: u32) {
    let result: zbus::Result<()> = bus.call(
        LOGIND,
        SESSION_PATH,
        SESSION,
        "SetBrightness",
        &("backlight", name, raw),
    );
    if let Err(error) = result {
        eprintln!("sengi-settings: setting the brightness: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn parses_the_two_files_and_refuses_odd_names() {
        let light = parse("intel_backlight", "4800\n", "7500\n").unwrap();
        assert_eq!((light.brightness, light.max), (4800, 7500));
        assert_eq!(light.percent(), 64);
        assert_eq!(parse("intel_backlight", "x", "7500"), None);
        assert_eq!(parse("intel_backlight", "10", "0"), None);
        assert_eq!(parse("b", "900", "255").unwrap().brightness, 255);
        for bad in ["", ".", "..", "a/b", "a b", "../x", &"n".repeat(65)] {
            assert_eq!(parse(bad, "1", "10"), None, "{bad}");
        }
    }

    #[test]
    fn the_slider_never_reaches_dark() {
        let light = parse("b", "0", "7500").unwrap();
        assert_eq!(light.raw(100), 7500);
        assert_eq!(light.raw(50), 3750);
        assert_eq!(light.raw(0), 375);
        assert_eq!(light.raw(900), 7500);
        // A device with a handful of steps still never gets zero.
        assert_eq!(parse("b", "0", "7").unwrap().raw(0), 1);
    }

    #[test]
    fn the_firmware_interface_is_preferred() {
        let tmp = TempDir::new();
        for (name, kind) in [
            ("nvidia_0", "raw"),
            ("acpi_video0", "firmware"),
            ("x", "platform"),
        ] {
            tmp.write(&format!("{name}/brightness"), "5");
            tmp.write(&format!("{name}/max_brightness"), "10");
            tmp.write(&format!("{name}/type"), kind);
        }
        tmp.write("broken/brightness", "5");
        assert_eq!(read(tmp.path()).unwrap().name, "acpi_video0");
        assert_eq!(read(&tmp.path().join("missing")), None);
    }
}
