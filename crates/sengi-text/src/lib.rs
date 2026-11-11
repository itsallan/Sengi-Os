//! Making text from outside safe to show.
//!
//! Network names, Bluetooth device names, notification text, tray titles,
//! menu labels and the names in desktop files are all chosen by someone
//! else. What is shown has no control characters, nothing that reorders or
//! hides the text around it, and a bounded length. One list of unwanted
//! characters serves every program, so a trick that is closed in one place
//! is closed in all of them.

/// The least that [`line`] reads of its input before giving up on it, in
/// characters.
const MIN_SCAN: usize = 768;

/// Characters that draw nothing or change the direction of the text around
/// them, which a sender could use to make one text read as another or to
/// pad a label with what looks like nothing: soft hyphen, the bidirectional
/// marks, embeddings, overrides and isolates, line and paragraph
/// separators, zero-width spaces and joiners, the fillers that draw as
/// blanks but take up room (Hangul fillers, the blank Braille pattern),
/// variation selectors, interlinear annotation marks and the invisible tag
/// block.
///
/// Joiners and variation selectors go too: some emoji fall apart into
/// their pieces, which is the price of not having to tell a harmless
/// joiner from a hidden one.
pub fn invisible(c: char) -> bool {
    matches!(c,
        // Soft hyphen, Arabic letter mark, Mongolian vowel separator.
        '\u{ad}' | '\u{61c}' | '\u{180e}'
        // Zero-width spaces and joiners, direction marks.
        | '\u{200b}'..='\u{200f}'
        // Line and paragraph separators, direction embeddings and overrides.
        | '\u{2028}'..='\u{202e}'
        // Word joiner, invisible operators, direction isolates and the
        // deprecated format characters after them.
        | '\u{2060}'..='\u{206f}'
        // Hangul fillers and the blank Braille pattern, which are letters
        // and a symbol that draw as blanks.
        | '\u{115f}' | '\u{1160}' | '\u{3164}' | '\u{ffa0}' | '\u{2800}'
        // Variation selectors.
        | '\u{fe00}'..='\u{fe0f}' | '\u{e0100}'..='\u{e01ef}'
        // Byte order mark, interlinear annotation marks, tag characters.
        | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{e0000}'..='\u{e007f}')
}

/// Characters that must never be shown: controls and the [`invisible`] ones.
pub fn hidden(c: char) -> bool {
    c.is_control() || invisible(c)
}

/// `name` for use inside a sentence of the program's own. A name in a
/// right-to-left script would otherwise reorder the words around it; the
/// isolate marks keep its direction to itself. `name` must have been
/// through [`line`] or [`tidy`], so that it holds no marks of its own to
/// close these early.
pub fn isolate(name: &str) -> String {
    format!("\u{2068}{name}\u{2069}")
}

/// Makes `raw` fit to show on one line: hidden characters removed, runs of
/// white space (line breaks included) folded into one space, none at
/// either end, at most `max_chars` characters with an ellipsis in place of
/// whatever was cut.
///
/// Only the first eight times `max_chars` characters of `raw` (768 at
/// least) are looked at. Text behind a longer wall of junk is not searched
/// for; the ellipsis stands for it.
pub fn line(raw: &str, max_chars: usize) -> String {
    let scan = max_chars.saturating_mul(8).max(MIN_SCAN);
    let mut out = String::new();
    let mut count = 0;
    let mut gap = false;
    let mut cut = false;
    let mut chars = raw.chars();
    for c in chars.by_ref().take(scan) {
        // A line break is a control, and a line separator is on the hidden
        // list, but both separate words all the same.
        if c.is_whitespace() || matches!(c, '\u{2028}' | '\u{2029}') {
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
    if cut || chars.next().is_some() {
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

/// Makes text fit to draw where white space matters: no hidden characters,
/// no runs of blank lines, at most `max_chars` characters, and a single
/// line unless `multiline`. A cut ends in an ellipsis.
pub fn tidy(text: &str, max_chars: usize, multiline: bool) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut newlines = 0;
    let mut cut = false;
    for c in text.trim().chars() {
        let c = match c {
            '\n' if multiline => '\n',
            '\n' | '\t' | '\r' => ' ',
            c if hidden(c) => continue,
            c => c,
        };
        // A space that only led because what came before it was removed.
        if out.is_empty() && c.is_whitespace() {
            continue;
        }
        if c == '\n' {
            newlines += 1;
            if newlines > 2 {
                continue;
            }
        } else {
            newlines = 0;
        }
        if count == max_chars {
            cut = true;
            break;
        }
        out.push(c);
        count += 1;
    }
    let kept = out.trim_end().len();
    out.truncate(kept);
    if cut {
        // The ellipsis takes the place of the last character.
        out.pop();
        let kept = out.trim_end().len();
        out.truncate(kept);
        out.push('\u{2026}');
    }
    out
}

/// The letter for the fallback tile of something with no usable icon: its
/// first letter or digit, in upper case, or `?`.
pub fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric() && !hidden(*c))
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_TEXT: usize = 96;

    // From quick settings, where the text is a network or device name.

    #[test]
    fn ordinary_names_pass_through() {
        assert_eq!(line("Home WiFi", 32), "Home WiFi");
        assert_eq!(line("Café Écran 5G", 32), "Café Écran 5G");
        assert_eq!(line("家のネット", 32), "家のネット");
    }

    #[test]
    fn controls_are_dropped() {
        assert_eq!(line("a\u{7}b\u{1b}[31mc", 32), "ab[31mc");
        assert_eq!(line("nul\0inside", 32), "nulinside");
        assert_eq!(line("\u{9b}x\u{85}", 32), "x");
    }

    #[test]
    fn line_breaks_and_tabs_become_one_space() {
        assert_eq!(line("one\ntwo\r\n\tthree", 32), "one two three");
        assert_eq!(line("  padded   name  ", 32), "padded name");
        assert_eq!(line("a\u{2028}b\u{3000}c", 32), "a b c");
    }

    #[test]
    fn text_direction_tricks_are_dropped() {
        // Right-to-left override, as used to disguise a name.
        assert_eq!(line("Free\u{202e}iFiW", 32), "FreeiFiW");
        assert_eq!(line("\u{2066}a\u{2069}\u{200f}b\u{061c}", 32), "ab");
        assert_eq!(line("zero\u{200b}width\u{feff}", 32), "zerowidth");
        assert_eq!(line("tag\u{e0041}ged", 32), "tagged");
    }

    #[test]
    fn fillers_that_draw_as_nothing_are_dropped() {
        // Hangul fillers, halfwidth filler, blank Braille: a name made of
        // these looks empty, and padding with them pushes text out of view.
        assert_eq!(line("\u{115f}\u{1160}\u{3164}\u{ffa0}\u{2800}", 32), "");
        assert_eq!(line("Home\u{3164}\u{3164}\u{3164}Guest", 32), "HomeGuest");
        // Variation selectors, plain and supplementary.
        assert_eq!(line("a\u{fe0f}b\u{fe00}c\u{e0100}", 32), "abc");
    }

    #[test]
    fn a_name_in_a_sentence_keeps_its_direction_to_itself() {
        assert_eq!(isolate("Home"), "\u{2068}Home\u{2069}");
        // A hostile name cannot bring its own marks to break out.
        let hostile = line("\u{2069}\u{202e}evil\u{2068}", 32);
        assert_eq!(isolate(&hostile), "\u{2068}evil\u{2069}");
    }

    #[test]
    fn long_names_are_cut_with_an_ellipsis() {
        assert_eq!(line("abcdefghij", 10), "abcdefghij");
        assert_eq!(line("abcdefghijk", 10), "abcdefghi\u{2026}");
        assert_eq!(line("abcd efgh ijkl", 10), "abcd efgh\u{2026}");
        // The cut never leaves a space before the ellipsis.
        assert_eq!(line("abcdefgh ijkl", 10), "abcdefgh\u{2026}");
        let long = "é".repeat(500);
        assert_eq!(line(&long, 32).chars().count(), 32);
    }

    #[test]
    fn nothing_fits_in_no_room() {
        assert_eq!(line("anything", 0), "\u{2026}");
    }

    // From the bar's tray, where it is a title, a tooltip or a menu label.

    #[test]
    fn plain_text_is_left_alone() {
        assert_eq!(line("Network Manager", MAX_TEXT), "Network Manager");
        assert_eq!(
            line("Café \u{4e2d}\u{6587}", MAX_TEXT),
            "Café \u{4e2d}\u{6587}"
        );
        assert_eq!(line("", MAX_TEXT), "");
    }

    #[test]
    fn white_space_becomes_single_spaces_and_controls_go() {
        assert_eq!(line("one\ntwo\t\tthree\r\n", MAX_TEXT), "one two three");
        assert_eq!(line("\u{1b}[31mred\u{7}\u{0}", MAX_TEXT), "[31mred");
        assert_eq!(line("  padded   out  ", MAX_TEXT), "padded out");
        assert_eq!(line("\n\n\n", MAX_TEXT), "");
    }

    #[test]
    fn bidi_overrides_and_invisible_marks_are_dropped() {
        // The classic: "exe.txt" made to read as "txt.exe".
        assert_eq!(line("photo\u{202e}txt.exe", MAX_TEXT), "phototxt.exe");
        assert_eq!(line("\u{2066}a\u{2069}\u{200b}b\u{feff}", MAX_TEXT), "ab");
        // Line and paragraph separators separate words, as a line break does.
        assert_eq!(line("a\u{2028}b\u{2029}c", MAX_TEXT), "a b c");
        assert_eq!(line("tag\u{e0041}ged", MAX_TEXT), "tagged");
        // Joiners go too: there is no telling a harmless one from a hidden
        // one, and some emoji falling apart is the price.
        assert_eq!(line("a\u{200d}b", MAX_TEXT), "ab");
    }

    #[test]
    fn fillers_that_draw_as_blank_space_are_dropped() {
        // A title made of these would be a wide, empty, clickable gap.
        let blank = "\u{3164}\u{115f}\u{1160}\u{ffa0}\u{2800}";
        assert_eq!(line(blank, MAX_TEXT), "");
        assert_eq!(line(&format!("a{blank}b"), MAX_TEXT), "ab");
        assert_eq!(initial(blank), "?");
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis() {
        let cut = line("abcdefghij", 5);
        assert_eq!(cut, "abcd\u{2026}");
        assert_eq!(cut.chars().count(), 5);
        assert_eq!(line("abcde", 5), "abcde");
        assert_eq!(line("abc def", 5), "abc\u{2026}");
        // Counted in characters, not bytes.
        assert_eq!(line("ééééé", 5), "ééééé");

        let endless = "x".repeat(1_000_000);
        let cut = line(&endless, MAX_TEXT);
        assert_eq!(cut.chars().count(), MAX_TEXT);
        assert!(cut.ends_with('\u{2026}'));
    }

    #[test]
    fn text_hidden_behind_a_wall_of_junk_is_not_searched_for() {
        let text = format!("{}tail", "\u{0}".repeat(MIN_SCAN * 2));
        assert_eq!(line(&text, MAX_TEXT), "\u{2026}");
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

    #[test]
    fn the_fallback_letter_is_the_first_letter_or_digit() {
        assert_eq!(initial("foot"), "F");
        assert_eq!(initial("  (beta) editor"), "B");
        assert_eq!(initial("7zip"), "7");
        assert_eq!(initial("élan"), "É");
        assert_eq!(initial("---"), "?");
        assert_eq!(initial(""), "?");
    }

    // From the notification service, where it is a summary or a body.

    #[test]
    fn tidy_removes_control_and_direction_characters() {
        assert_eq!(tidy("a\u{0}b\u{7}c\u{1b}[31md", 100, true), "abc[31md");
        assert_eq!(tidy("safe\u{202e}txt.exe", 100, false), "safetxt.exe");
        assert_eq!(tidy("\u{2066}x\u{2069}\u{200f}", 100, false), "x");
    }

    #[test]
    fn tidy_removes_characters_that_draw_nothing() {
        for trick in [
            '\u{ad}',
            '\u{180e}',
            '\u{200b}',
            '\u{200c}',
            '\u{200d}',
            '\u{2060}',
            '\u{2064}',
            '\u{115f}',
            '\u{1160}',
            '\u{3164}',
            '\u{ffa0}',
            '\u{fe0f}',
            '\u{e0100}',
            '\u{e0001}',
            '\u{e0041}',
            '\u{e007f}',
            '\u{feff}',
        ] {
            let text = format!("pay{trick}pal");
            assert_eq!(tidy(&text, 100, false), "paypal", "U+{:04X}", trick as u32);
        }
        // A label made only of them is empty, not a blank button.
        assert_eq!(tidy("\u{200b}\u{3164}\u{e0020}", 100, false), "");
        assert_eq!(tidy("\u{200b} \u{200b} OK \u{200b}", 100, false), "OK");
        // They do not use up the length either.
        let padded = format!("{}end", "\u{200b}".repeat(1_000));
        assert_eq!(tidy(&padded, 5, false), "end");
        // Ordinary spaces, accents and scripts are left alone.
        assert_eq!(
            tidy("a\u{a0}b e\u{301} 한글 😀", 100, false),
            "a\u{a0}b e\u{301} 한글 😀"
        );
    }

    #[test]
    fn tidy_keeps_one_line_or_several() {
        assert_eq!(tidy("one\ntwo\tthree\r\n", 100, false), "one two three");
        assert_eq!(tidy("one\ntwo", 100, true), "one\ntwo");
        assert_eq!(tidy("one\n\n\n\n\n\ntwo", 100, true), "one\n\ntwo");
        assert_eq!(tidy("  padded  \n\n", 100, true), "padded");
    }

    #[test]
    fn tidy_cuts_long_text_with_an_ellipsis() {
        assert_eq!(tidy("abcdef", 6, false), "abcdef");
        assert_eq!(tidy("abcdefg", 6, false), "abcde\u{2026}");
        assert_eq!(tidy("ab cdefg", 4, false), "ab\u{2026}");
        assert_eq!(tidy("ééééé", 3, false), "éé\u{2026}");
        assert_eq!(tidy("anything", 0, false), "\u{2026}");
        assert_eq!(tidy("", 5, false), "");
        let huge = "x".repeat(1_000_000);
        assert_eq!(tidy(&huge, 10, false).chars().count(), 10);
    }

    #[test]
    fn every_copy_of_the_list_is_covered() {
        // One character from each list the programs used to keep.
        for c in [
            '\u{2800}',
            '\u{200d}',
            '\u{fe0f}',
            '\u{e0100}',
            '\u{202e}',
            '\u{2066}',
        ] {
            assert!(invisible(c), "U+{:04X}", c as u32);
            assert!(hidden(c));
        }
        assert!(hidden('\u{1b}') && !invisible('\u{1b}'));
        assert!(!hidden('a') && !hidden(' ') && !hidden('\u{a0}') && !hidden('\u{301}'));
    }
}
