//! The display backlight, read from `/sys/class/backlight`.
//!
//! Reading is open to everyone. Writing is not, so changes go through
//! logind (see `machine`), which sets the brightness of a device on
//! the caller's own seat.

use std::fs;
use std::path::Path;

/// The slider never goes below this. At zero many panels switch off, and
/// the slider that would bring them back could no longer be seen.
pub const MIN_PERCENT: u32 = 5;

/// One backlight device and where it is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlight {
    /// The device's name under `/sys/class/backlight`.
    pub name: String,
    /// The current setting, in the device's own units.
    pub brightness: u32,
    /// The highest setting, in the device's own units. Never zero.
    pub max: u32,
}

/// Makes sense of a device's `brightness` and `max_brightness` files.
/// `None` if either is not a number, the maximum is zero, or `name` does
/// not look like a device name.
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

    /// The current setting as a percentage of the maximum, rounded.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn light(brightness: u32, max: u32) -> Backlight {
        Backlight {
            name: "intel_backlight".to_string(),
            brightness,
            max,
        }
    }

    #[test]
    fn parses_the_two_files() {
        assert_eq!(
            parse("intel_backlight", "4800\n", "7500\n"),
            Some(light(4800, 7500))
        );
        assert_eq!(parse("intel_backlight", "x", "7500"), None);
        assert_eq!(parse("intel_backlight", "10", "0"), None);
        assert_eq!(parse("intel_backlight", "10", ""), None);
        // Brighter than the maximum is read as the maximum.
        assert_eq!(parse("b", "900", "255").unwrap().brightness, 255);
    }

    #[test]
    fn names_that_are_not_device_names_are_refused() {
        assert_eq!(parse("", "1", "10"), None);
        assert_eq!(parse("..", "1", "10"), None);
        assert_eq!(parse("a/b", "1", "10"), None);
        assert_eq!(parse("new\nline", "1", "10"), None);
        assert!(parse("acpi_video0", "1", "10").is_some());
        assert!(parse("amdgpu_bl1", "1", "10").is_some());
        assert!(parse("backlight-lvds.0", "1", "10").is_some());
    }

    #[test]
    fn percent_is_rounded() {
        assert_eq!(light(4800, 7500).percent(), 64);
        assert_eq!(light(0, 255).percent(), 0);
        assert_eq!(light(255, 255).percent(), 100);
        assert_eq!(light(127, 255).percent(), 50);
        assert_eq!(light(u32::MAX, u32::MAX).percent(), 100);
    }

    #[test]
    fn the_slider_maps_back_to_raw_values() {
        let light = light(0, 7500);
        assert_eq!(light.raw(100), 7500);
        assert_eq!(light.raw(50), 3750);
        assert_eq!(light.raw(250), 7500);
    }

    #[test]
    fn brightness_never_goes_below_the_floor() {
        assert_eq!(light(0, 7500).floor(), 375);
        assert_eq!(light(0, 7500).raw(0), 375);
        assert_eq!(light(0, 7500).raw(3), 375);
        // Few steps: the floor rounds up, and is at least one step.
        assert_eq!(light(0, 15).floor(), 1);
        assert_eq!(light(0, 15).raw(0), 1);
        assert_eq!(light(0, 1).raw(0), 1);
        assert_eq!(light(0, 255).raw(0), 13);
    }

    #[test]
    fn the_firmware_backlight_is_preferred() {
        let sys = TempDir::new();
        sys.write("intel_backlight/brightness", "100\n");
        sys.write("intel_backlight/max_brightness", "1000\n");
        sys.write("intel_backlight/type", "raw\n");
        sys.write("acpi_video0/brightness", "7\n");
        sys.write("acpi_video0/max_brightness", "15\n");
        sys.write("acpi_video0/type", "firmware\n");
        sys.write("broken/brightness", "7\n");
        assert_eq!(read(sys.path()).unwrap().name, "acpi_video0");
    }

    #[test]
    fn no_backlight() {
        assert_eq!(read(TempDir::new().path()), None);
        assert_eq!(read(Path::new("/nonexistent/sengi-quick")), None);
    }
}
