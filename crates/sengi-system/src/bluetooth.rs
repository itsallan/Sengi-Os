//! Bluetooth as the panel sees it. [`bluez`](crate::bluez) fills this in.

use sengi_text::isolate;

/// More connected devices than this are counted, not named.
pub const MAX_DEVICES: usize = 8;

/// Bluetooth as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bluetooth {
    /// BlueZ is not running or has no adapter.
    Absent,
    /// BlueZ is running and the bus policy keeps this user out.
    Denied,
    /// There is an adapter and it is switched off.
    Off,
    /// Switched on.
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
        [one, two] => format!("{}, {}", isolate(one), isolate(two)),
        [one, rest @ ..] => format!("{} and {} more", isolate(one), rest.len()),
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
        assert_eq!(
            summary(&names(2)),
            "\u{2068}Device 1\u{2069}, \u{2068}Device 2\u{2069}"
        );
        assert_eq!(summary(&names(5)), "\u{2068}Device 1\u{2069} and 4 more");
    }
}
