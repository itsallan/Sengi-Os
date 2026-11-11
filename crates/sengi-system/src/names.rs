//! Names that come from outside: network names, Bluetooth device names.
//!
//! Anyone in radio range chooses these, so nothing in them is trusted. What
//! is shown has been through sengi-text and has a bounded length.

/// An SSID is at most 32 bytes; device names are cut to the same length.
pub const MAX_NAME_CHARS: usize = 32;

/// A name for display, or `fallback` when nothing printable is left of it.
pub fn name(raw: &str, fallback: &str) -> String {
    let cleaned = sengi_text::line(raw, MAX_NAME_CHARS);
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
}
