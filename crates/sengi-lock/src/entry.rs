//! A password as it is typed.
//!
//! There is no text field behind the lock screen's prompt. Key presses
//! are collected here, in one buffer that is set aside up front, kept out
//! of swap, never moved, and overwritten when a character is taken back,
//! when the entry is cleared and when it is dropped. The screen is told
//! how many characters there are and nothing else.

use sengi_system::secret::Secret;

/// The longest password taken, in bytes of UTF-8. PAM's own limit is 512
/// with the byte that ends the string; anything longer could not be
/// checked anyway.
pub const MAX_BYTES: usize = 511;

/// What became of a key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Typed {
    Added,
    /// There was no room. Nothing was added, not even part of it.
    Full,
    /// Not text: a key code of the toolkit's, or a control character.
    Ignored,
}

pub struct PasswordEntry {
    secret: Secret,
}

/// True for what a key press can add to a password. Slint reports keys
/// that type nothing as control characters and as characters from the
/// private use area.
pub fn is_text(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| !c.is_control() && !('\u{e000}'..='\u{f8ff}').contains(&c))
}

impl PasswordEntry {
    pub fn new() -> PasswordEntry {
        PasswordEntry {
            secret: Secret::with_capacity(MAX_BYTES),
        }
    }

    /// Adds what one key press typed.
    pub fn push(&mut self, text: &str) -> Typed {
        if !is_text(text) {
            Typed::Ignored
        } else if self.secret.push(text) {
            Typed::Added
        } else {
            Typed::Full
        }
    }

    /// Takes back the last character. False if there was none.
    pub fn erase(&mut self) -> bool {
        self.secret.pop()
    }

    /// Forgets everything typed.
    pub fn clear(&mut self) {
        self.secret.clear();
    }

    /// How many characters have been typed.
    pub fn chars(&self) -> usize {
        self.secret.chars()
    }

    pub fn is_empty(&self) -> bool {
        self.secret.is_empty()
    }

    /// Whether the same was typed into both. For a new password, which
    /// is typed twice because it cannot be read back.
    pub fn same_as(&self, other: &PasswordEntry) -> bool {
        self.secret.expose() == other.secret.expose()
    }

    /// Hands the password over for checking and starts afresh. None if
    /// nothing was typed.
    pub fn take(&mut self) -> Option<Secret> {
        if self.secret.is_empty() {
            return None;
        }
        Some(std::mem::replace(
            &mut self.secret,
            Secret::with_capacity(MAX_BYTES),
        ))
    }
}

impl Default for PasswordEntry {
    fn default() -> PasswordEntry {
        PasswordEntry::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_add_up_and_come_off_again() {
        let mut entry = PasswordEntry::new();
        assert_eq!(entry.push("l"), Typed::Added);
        assert_eq!(entry.push("\u{e9}"), Typed::Added);
        assert_eq!(entry.push("v"), Typed::Added);
        assert_eq!(entry.chars(), 3);
        assert!(entry.erase());
        assert_eq!(entry.chars(), 2);
        let secret = entry.take().expect("something was typed");
        assert_eq!(secret.expose(), "l\u{e9}");
        assert!(entry.is_empty());
        assert!(entry.take().is_none());
        assert!(!entry.erase());
    }

    #[test]
    fn keys_that_type_nothing_add_nothing() {
        let mut entry = PasswordEntry::new();
        // Slint's codes for Shift and for the up arrow, a tab, and nothing.
        for key in ["\u{10}", "\u{f700}", "\t", "", "a\n"] {
            assert_eq!(entry.push(key), Typed::Ignored, "{key:?}");
        }
        assert!(entry.is_empty());
        // A space is part of a password like any other character.
        assert_eq!(entry.push(" "), Typed::Added);
    }

    #[test]
    fn a_full_entry_takes_nothing_more_and_keeps_what_it_has() {
        let mut entry = PasswordEntry::new();
        for _ in 0..MAX_BYTES - 1 {
            assert_eq!(entry.push("x"), Typed::Added);
        }
        // Two bytes into one byte of room: none of it goes in.
        assert_eq!(entry.push("\u{e9}"), Typed::Full);
        assert_eq!(entry.chars(), MAX_BYTES - 1);
        assert_eq!(entry.push("y"), Typed::Added);
        assert_eq!(entry.push("z"), Typed::Full);
        assert_eq!(entry.chars(), MAX_BYTES);
        // Backspace still works at the cap, and makes room again.
        assert!(entry.erase());
        assert_eq!(entry.push("z"), Typed::Added);
        let secret = entry.take().unwrap();
        assert_eq!(secret.len(), MAX_BYTES);
        assert!(secret.expose().ends_with("xz"));
    }

    #[test]
    fn two_entries_are_the_same_when_the_same_was_typed() {
        let (mut first, mut again) = (PasswordEntry::new(), PasswordEntry::new());
        assert!(first.same_as(&again));
        first.push("p\u{e4}ss");
        assert!(!first.same_as(&again));
        again.push("p\u{e4}s");
        assert!(!first.same_as(&again));
        again.push("s");
        assert!(first.same_as(&again) && again.same_as(&first));
        again.push(" ");
        assert!(!first.same_as(&again));
    }

    #[test]
    fn clearing_leaves_an_entry_that_can_be_typed_into() {
        let mut entry = PasswordEntry::new();
        entry.push("abc");
        entry.clear();
        assert!(entry.is_empty());
        assert_eq!(entry.push("d"), Typed::Added);
        assert_eq!(entry.take().unwrap().expose(), "d");
    }
}
