//! Names that come from outside: network names, Bluetooth device names,
//! audio devices, monitors, anything read from `/sys` or a bus.
//!
//! Anyone in radio range chooses these, so nothing in them is trusted. What
//! is shown has no control characters, nothing that reorders or hides the
//! text around it, and a bounded length.
//!
//! The same code as sengi-quick's `text.rs`, with [`line`] added; both are to
//! come from sengi-system.

/// An SSID is at most 32 bytes; device names are cut to the same length.
pub const MAX_NAME_CHARS: usize = 32;

/// Characters that are dropped: controls, the bidirectional embedding,
/// override and isolate marks, zero-width characters, the fillers that
/// draw as nothing but take up room (Hangul fillers, the blank Braille
/// pattern), variation selectors, and the invisible tag block.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00ad}'
                | '\u{061c}'
                | '\u{115f}'
                | '\u{1160}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{2800}'
                | '\u{3164}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{e0000}'..='\u{e007f}'
                | '\u{e0100}'..='\u{e01ef}'
        )
}

/// `name` for use inside a sentence of our own. A name in a
/// right-to-left script would otherwise reorder the words around it; the
/// isolate marks keep its direction to itself. `name` has been through
/// [`clean`], so it holds no marks of its own to close these early.
pub fn isolate(name: &str) -> String {
    format!("\u{2068}{name}\u{2069}")
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

/// A longer piece of outside text for one line: a model name, a version
/// string. Cleaned like a name, with room for `max_chars`.
pub fn line(raw: &str, max_chars: usize) -> String {
    clean(raw, max_chars)
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
    fn fillers_that_draw_as_nothing_are_dropped() {
        // Hangul fillers, halfwidth filler, blank Braille: a name made of
        // these looks empty, and padding with them pushes text out of view.
        assert_eq!(clean("\u{115f}\u{1160}\u{3164}\u{ffa0}\u{2800}", 32), "");
        assert_eq!(clean("Home\u{3164}\u{3164}\u{3164}Guest", 32), "HomeGuest");
        assert_eq!(name("\u{2800}\u{2800}", "Unnamed"), "Unnamed");
        // Variation selectors, plain and supplementary.
        assert_eq!(clean("a\u{fe0f}b\u{fe00}c\u{e0100}", 32), "abc");
    }

    #[test]
    fn a_name_in_a_sentence_keeps_its_direction_to_itself() {
        assert_eq!(isolate("Home"), "\u{2068}Home\u{2069}");
        // A hostile name cannot bring its own marks to break out.
        let hostile = clean("\u{2069}\u{202e}evil\u{2068}", 32);
        assert_eq!(isolate(&hostile), "\u{2068}evil\u{2069}");
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
