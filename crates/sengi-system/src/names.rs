//! Names that come from outside: network names, Bluetooth device names.
//!
//! Anyone in radio range chooses these, so nothing in them is trusted. What
//! is shown has been through sengi-text and has a bounded length.

/// An SSID is at most 32 bytes; device names are cut to the same length.
pub const MAX_NAME_CHARS: usize = 32;

/// Where a device's name is cut in a list that gives it a row of its
/// own. A Bluetooth name can be 248 bytes; this is a bound on what is
/// kept, not a width: the row elides what does not fit.
pub const MAX_DEVICE_NAME_CHARS: usize = 64;

/// A name for display, or `fallback` when nothing printable is left of it.
pub fn name(raw: &str, fallback: &str) -> String {
    within(raw, fallback, MAX_NAME_CHARS)
}

/// As [`name`], for a device in a list with room for it.
pub fn device_name(raw: &str, fallback: &str) -> String {
    within(raw, fallback, MAX_DEVICE_NAME_CHARS)
}

fn within(raw: &str, fallback: &str, limit: usize) -> String {
    let cleaned = sengi_text::line(raw, limit);
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_of_nothing_gets_the_fallback() {
        assert_eq!(name("\u{202e}\u{0}\n", "Unnamed"), "Unnamed");
        assert_eq!(name("\u{2800}\u{2800}", "Unnamed"), "Unnamed");
        assert_eq!(name("", "Unnamed"), "Unnamed");
        assert_eq!(name("x", "Unnamed"), "x");
    }

    #[test]
    fn a_long_name_is_cut_at_the_length_of_an_ssid() {
        let long = "é".repeat(500);
        assert_eq!(name(&long, "Unnamed").chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn a_device_name_is_kept_longer_but_not_whole() {
        let named = "Living room speaker (left of the bookshelf)";
        assert_eq!(device_name(named, "Unnamed"), named);
        let long = "é".repeat(500);
        assert_eq!(
            device_name(&long, "Unnamed").chars().count(),
            MAX_DEVICE_NAME_CHARS
        );
        assert_eq!(device_name("\u{202e}\u{0}", "Unnamed"), "Unnamed");
    }
}
