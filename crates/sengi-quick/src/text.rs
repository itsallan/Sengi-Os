//! Names that come from outside: network names, Bluetooth device names.
//!
//! Anyone in radio range chooses these, so nothing in them is trusted. What
//! is shown has no control characters, nothing that reorders or hides the
//! text around it, and a bounded length.

/// An SSID is at most 32 bytes; device names are cut to the same length.
pub const MAX_NAME_CHARS: usize = 32;

/// Characters that are dropped: controls, the bidirectional embedding,
/// override and isolate marks, zero-width characters, and the invisible
/// tag block.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00ad}'
                | '\u{061c}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{e0000}'..='\u{e007f}'
        )
}

/// Make `raw` fit to show on one line: hidden characters removed, runs of
/// white space folded into one space, at most `max_chars` characters with
/// an ellipsis in place of whatever was cut.
pub fn clean(raw: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut gap = false;
    let mut cut = false;
    for c in raw.chars() {
        // A line break is a control, but it separates words all the same.
        if c.is_whitespace() {
            gap = count > 0;
            continue;
        }
        if hidden(c) {
            continue;
        }
        let needed = if gap { 2 } else { 1 };
        if count + needed > max_chars {
            cut = true;
            break;
        }
        if gap {
            out.push(' ');
            gap = false;
        }
        out.push(c);
        count += needed;
    }
    if cut {
        // Make room for the ellipsis within the limit.
        while count >= max_chars && count > 0 {
            out.pop();
            count -= 1;
        }
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
        out.push('\u{2026}');
    }
    out
}

/// A name for display, or `fallback` when nothing printable is left of it.
pub fn name(raw: &str, fallback: &str) -> String {
    let cleaned = clean(raw, MAX_NAME_CHARS);
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
    fn ordinary_names_pass_through() {
        assert_eq!(clean("Home WiFi", 32), "Home WiFi");
        assert_eq!(clean("Café Écran 5G", 32), "Café Écran 5G");
        assert_eq!(clean("家のネット", 32), "家のネット");
    }

    #[test]
    fn controls_are_dropped() {
        assert_eq!(clean("a\u{7}b\u{1b}[31mc", 32), "ab[31mc");
        assert_eq!(clean("nul\0inside", 32), "nulinside");
        assert_eq!(clean("\u{9b}x\u{85}", 32), "x");
    }

    #[test]
    fn line_breaks_and_tabs_become_one_space() {
        assert_eq!(clean("one\ntwo\r\n\tthree", 32), "one two three");
        assert_eq!(clean("  padded   name  ", 32), "padded name");
        assert_eq!(clean("a\u{2028}b\u{3000}c", 32), "a b c");
    }

    #[test]
    fn text_direction_tricks_are_dropped() {
        // Right-to-left override, as used to disguise a name.
        assert_eq!(clean("Free\u{202e}iFiW", 32), "FreeiFiW");
        assert_eq!(clean("\u{2066}a\u{2069}\u{200f}b\u{061c}", 32), "ab");
        assert_eq!(clean("zero\u{200b}width\u{feff}", 32), "zerowidth");
        assert_eq!(clean("tag\u{e0041}ged", 32), "tagged");
    }

    #[test]
    fn long_names_are_cut_with_an_ellipsis() {
        assert_eq!(clean("abcdefghij", 10), "abcdefghij");
        assert_eq!(clean("abcdefghijk", 10), "abcdefghi\u{2026}");
        assert_eq!(clean("abcd efgh ijkl", 10), "abcd efgh\u{2026}");
        // The cut never leaves a space before the ellipsis.
        assert_eq!(clean("abcdefgh ijkl", 10), "abcdefgh\u{2026}");
        let long = "é".repeat(500);
        assert_eq!(clean(&long, 32).chars().count(), 32);
    }

    #[test]
    fn a_name_of_nothing_gets_the_fallback() {
        assert_eq!(name("\u{202e}\u{0}\n", "Unnamed"), "Unnamed");
        assert_eq!(name("", "Unnamed"), "Unnamed");
        assert_eq!(name("x", "Unnamed"), "x");
        assert_eq!(clean("anything", 0), "\u{2026}");
    }
}
