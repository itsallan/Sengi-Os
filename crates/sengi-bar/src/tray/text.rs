//! Cleaning up text that tray apps hand us.
//!
//! Titles, tooltips and menu labels come from whatever is on the session
//! bus. They are drawn in the bar and in menus, so they go through
//! sengi-text, which takes out anything that could change how the
//! surrounding text reads, and the length is capped.

/// Longest title, tooltip or menu label we draw, in characters.
pub const MAX_TEXT: usize = 96;

/// Longest name shown beside the tray for the icon under the pointer.
pub const MAX_LABEL: usize = 40;

pub use sengi_text::{initial, line as clean};

/// How much of an over-long label is looked at for its access key.
const MAX_LABEL_SCAN: usize = MAX_TEXT * 8;

/// A menu label as it should be drawn. dbusmenu marks the access key with
/// an underscore before it and writes a real underscore as two.
pub fn label(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_LABEL_SCAN));
    let mut chars = text.chars().take(MAX_LABEL_SCAN);
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn an_endless_label_is_cut_like_any_other_text() {
        let endless = "_x".repeat(1_000_000);
        let cut = label(&endless);
        assert_eq!(cut.chars().count(), MAX_TEXT);
        assert!(cut.ends_with('\u{2026}'));
    }
}
