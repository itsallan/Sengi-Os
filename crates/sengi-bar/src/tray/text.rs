//! Cleaning up text that tray apps hand us.
//!
//! Titles, tooltips and menu labels come from whatever is on the session
//! bus. They are drawn in the bar and in menus, so anything that could
//! change how the surrounding text reads is taken out and the length is
//! capped.

/// Longest title, tooltip or menu label we draw, in characters.
pub const MAX_TEXT: usize = 96;

/// How much of an over-long string is looked at before giving up on it.
const MAX_SCAN: usize = MAX_TEXT * 8;

/// Characters that are dropped: the bidi embeddings, overrides and isolates,
/// line and paragraph separators, zero-width spaces and marks used for
/// spoofing, interlinear annotation and tag characters.
fn unwanted(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{e0000}'..='\u{e007f}'
    )
}

/// One line of plain text, at most `max` characters. Control characters and
/// runs of white space become a single space; a cut is marked with an
/// ellipsis.
pub fn clean(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut space = false;
    let mut cut = false;
    for c in text.chars().take(MAX_SCAN) {
        if unwanted(c) {
            continue;
        }
        if c.is_control() || c.is_whitespace() {
            space = count > 0;
            continue;
        }
        let needed = if space { 2 } else { 1 };
        if count + needed > max {
            cut = true;
            break;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
        count += needed;
    }
    if cut || text.chars().nth(MAX_SCAN).is_some() {
        // Make room for the ellipsis within the limit.
        if count >= max {
            out.pop();
        }
        let kept = out.trim_end().len();
        out.truncate(kept);
        out.push('\u{2026}');
    }
    out
}

/// A menu label as it should be drawn. dbusmenu marks the access key with
/// an underscore before it and writes a real underscore as two.
pub fn label(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_SCAN));
    let mut chars = text.chars().take(MAX_SCAN);
    while let Some(c) = chars.next() {
        if c == '_' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    clean(&out, MAX_TEXT)
}

/// The letter for the tile of an item with no usable icon.
pub fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_left_alone() {
        assert_eq!(clean("Network Manager", MAX_TEXT), "Network Manager");
        assert_eq!(
            clean("Café \u{4e2d}\u{6587}", MAX_TEXT),
            "Café \u{4e2d}\u{6587}"
        );
        assert_eq!(clean("", MAX_TEXT), "");
    }

    #[test]
    fn control_characters_become_single_spaces() {
        assert_eq!(clean("one\ntwo\t\tthree\r\n", MAX_TEXT), "one two three");
        assert_eq!(clean("\u{1b}[31mred\u{7}\u{0}", MAX_TEXT), "[31mred");
        assert_eq!(clean("  padded   out  ", MAX_TEXT), "padded out");
        assert_eq!(clean("\n\n\n", MAX_TEXT), "");
    }

    #[test]
    fn bidi_overrides_and_invisible_marks_are_dropped() {
        // The classic: "exe.txt" made to read as "txt.exe".
        assert_eq!(clean("photo\u{202e}txt.exe", MAX_TEXT), "phototxt.exe");
        assert_eq!(clean("\u{2066}a\u{2069}\u{200b}b\u{feff}", MAX_TEXT), "ab");
        assert_eq!(clean("a\u{2028}b\u{2029}c", MAX_TEXT), "abc");
        assert_eq!(clean("tag\u{e0041}ged", MAX_TEXT), "tagged");
        // Joiners that scripts and emoji need are kept.
        assert_eq!(clean("a\u{200d}b", MAX_TEXT), "a\u{200d}b");
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis() {
        let cut = clean("abcdefghij", 5);
        assert_eq!(cut, "abcd\u{2026}");
        assert_eq!(cut.chars().count(), 5);
        assert_eq!(clean("abcde", 5), "abcde");
        assert_eq!(clean("abc def", 5), "abc\u{2026}");
        // Counted in characters, not bytes.
        assert_eq!(clean("ééééé", 5), "ééééé");

        let endless = "x".repeat(1_000_000);
        let cut = clean(&endless, MAX_TEXT);
        assert_eq!(cut.chars().count(), MAX_TEXT);
        assert!(cut.ends_with('\u{2026}'));
    }

    #[test]
    fn text_hidden_behind_a_wall_of_junk_is_not_searched_for() {
        let text = format!("{}tail", "\u{0}".repeat(MAX_SCAN * 2));
        assert_eq!(clean(&text, MAX_TEXT), "\u{2026}");
    }

    #[test]
    fn mnemonics_are_stripped_from_labels() {
        assert_eq!(label("_File"), "File");
        assert_eq!(label("Save _As\u{2026}"), "Save As\u{2026}");
        assert_eq!(label("snake__case"), "snake_case");
        assert_eq!(label("trailing_"), "trailing");
        assert_eq!(label("_"), "");
        assert_eq!(label("_Quit\n\u{202e}now"), "Quit now");
    }

    #[test]
    fn the_tile_letter_is_the_first_letter_or_digit() {
        assert_eq!(initial("nm-applet"), "N");
        assert_eq!(initial("  (beta) chat"), "B");
        assert_eq!(initial("7zip"), "7");
        assert_eq!(initial("élan"), "É");
        assert_eq!(initial("---"), "?");
        assert_eq!(initial(""), "?");
    }
}
