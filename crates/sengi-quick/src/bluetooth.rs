//! Bluetooth as the panel sees it. `system::bluez` fills this in.

/// More connected devices than this are counted, not named.
pub const MAX_DEVICES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bluetooth {
    /// BlueZ is not running or has no adapter.
    Absent,
    /// BlueZ is running and the bus policy keeps this user out.
    Denied,
    Off,
    On {
        /// Names of the connected devices, cleaned for display.
        devices: Vec<String>,
    },
}

/// The line under the tile's title when Bluetooth is on.
pub fn summary(devices: &[String]) -> String {
    match devices {
        [] => "On".to_string(),
        [one] => one.clone(),
        [one, two] => format!("{one}, {two}"),
        [one, rest @ ..] => format!("{one} and {} more", rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries() {
        let names = |n: usize| -> Vec<String> { (1..=n).map(|i| format!("Device {i}")).collect() };
        assert_eq!(summary(&names(0)), "On");
        assert_eq!(summary(&names(1)), "Device 1");
        assert_eq!(summary(&names(2)), "Device 1, Device 2");
        assert_eq!(summary(&names(5)), "Device 1 and 4 more");
    }
}
