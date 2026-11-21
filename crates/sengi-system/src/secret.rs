//! A Wi-Fi passphrase on its way from the text field to iwd, or a
//! password on its way from the keyboard to PAM.
//!
//! The notes below are written for the panel. The lock screen uses the
//! same type differently: it never has a text field, and collects key
//! presses straight into a [`Secret`] made by [`Secret::with_capacity`].
//!
//! What is and is not protected, so nobody reads more into this than is
//! there:
//!
//! * The passphrase is never logged, never put on a command line or in
//!   the environment, and never written to disk by the panel. iwd stores
//!   it, in its own files.
//! * The panel's own copies, held as [`Secret`], are overwritten when they
//!   are dropped: the one taken from the text field and the one handed to
//!   the D-Bus reply.
//! * Copies the panel does not own are freed without being overwritten:
//!   the text field's string inside Slint, and the outgoing message inside
//!   zbus. They stay in freed memory until it is reused or the process
//!   exits, which for this program is seconds later.
//! * Because of those, [`forbid_core_dumps`] is called before anything
//!   else. The panel is built with `panic = "abort"`, and an abort must
//!   not leave a file with a passphrase in it.

use std::fmt;

/// Keep this process out of core dumps, and closed to `ptrace` and
/// `/proc/PID/mem` from other processes of the same user. Not inherited by
/// the programs the panel starts.
pub fn forbid_core_dumps() {
    // SAFETY: prctl with PR_SET_DUMPABLE takes two integers and touches no
    // memory of ours.
    let result = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) };
    if result != 0 {
        eprintln!("{}: could not turn off core dumps", crate::program_name());
    }
}

/// Holds one copy of a passphrase. It cannot be printed or cloned, and its
/// bytes are overwritten when it is dropped. Other copies, made by code
/// that was handed the text, are not its to wipe: see the module's notes.
pub struct Secret {
    text: String,
    /// The buffer's pages are kept out of swap.
    pinned: bool,
}

impl Secret {
    /// Takes charge of `text`. From here on it is wiped when dropped.
    pub fn new(text: String) -> Secret {
        Secret {
            text,
            pinned: false,
        }
    }

    /// An empty secret to type into, with room for `bytes` bytes and no
    /// more. The room is set aside now and never grows, so adding to it
    /// cannot move the text and leave a copy behind. Its memory is kept
    /// out of swap where the kernel allows that.
    pub fn with_capacity(bytes: usize) -> Secret {
        let text = String::with_capacity(bytes);
        // SAFETY: the range is the string's own allocation.
        let pinned =
            bytes > 0 && unsafe { libc::mlock(text.as_ptr().cast(), text.capacity()) } == 0;
        Secret { text, pinned }
    }

    /// The passphrase itself, for handing to iwd or PAM and for nothing
    /// else.
    pub fn expose(&self) -> &str {
        &self.text
    }

    /// Its length in bytes.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Whether it is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// How many characters it holds, which is all a prompt shows of it.
    pub fn chars(&self) -> usize {
        self.text.chars().count()
    }

    /// Adds `more` at the end if there is room for all of it. False, and
    /// nothing added, if there is not: the text is never moved to make
    /// room.
    pub fn push(&mut self, more: &str) -> bool {
        if self.text.len() + more.len() > self.text.capacity() {
            return false;
        }
        self.text.push_str(more);
        true
    }

    /// Takes the last character off and overwrites where it was. False if
    /// there was none.
    pub fn pop(&mut self) -> bool {
        let Some(last) = self.text.chars().next_back() else {
            return false;
        };
        let kept = self.text.len() - last.len_utf8();
        self.wipe_from(kept);
        true
    }

    /// Empties it, overwriting what it held. The room stays.
    pub fn clear(&mut self) {
        self.wipe_from(0);
    }

    fn wipe_from(&mut self, kept: usize) {
        // SAFETY: zero bytes are valid UTF-8 wherever they fall, `kept` is
        // a character boundary, and the text is cut there straight after.
        for byte in &mut unsafe { self.text.as_bytes_mut() }[kept..] {
            // Volatile, so the wipe of a buffer about to be freed is kept.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        self.text.truncate(kept);
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.wipe_from(0);
        if self.pinned {
            // SAFETY: the same range that was locked, not yet freed.
            unsafe { libc::munlock(self.text.as_ptr().cast(), self.text.capacity()) };
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_dumps_can_be_turned_off() {
        forbid_core_dumps();
        // SAFETY: PR_GET_DUMPABLE takes no further arguments.
        assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE) }, 0);
    }

    #[test]
    fn a_secret_does_not_print_itself() {
        let secret = Secret::new("correct horse".to_string());
        assert_eq!(format!("{secret:?}"), "Secret(..)");
        assert_eq!(secret.expose(), "correct horse");
        assert_eq!(secret.len(), 13);
    }

    #[test]
    fn typing_never_moves_the_text() {
        let mut secret = Secret::with_capacity(8);
        let start = secret.expose().as_ptr();
        assert!(secret.push("pass"));
        assert!(secret.push("w\u{f6}"));
        // One byte of room left, and this needs two.
        assert!(!secret.push("\u{f0}"));
        assert!(secret.push("d"));
        assert!(!secret.push("!"));
        assert_eq!(secret.expose(), "passw\u{f6}d");
        assert_eq!(secret.chars(), 7);
        assert_eq!(secret.expose().as_ptr(), start);
    }

    #[test]
    fn what_is_taken_back_is_overwritten() {
        let mut secret = Secret::with_capacity(16);
        secret.push("ab\u{20ac}");
        let start = secret.expose().as_ptr();
        assert!(secret.pop());
        assert_eq!(secret.expose(), "ab");
        // SAFETY: still inside the allocation, which is 16 bytes.
        let buffer = unsafe { std::slice::from_raw_parts(start, 5) };
        assert_eq!(buffer, [b'a', b'b', 0, 0, 0]);
        secret.clear();
        assert!(secret.is_empty());
        let buffer = unsafe { std::slice::from_raw_parts(start, 5) };
        assert_eq!(buffer, [0; 5]);
        assert!(!secret.pop());
        // And it can be typed into again.
        assert!(secret.push("again"));
    }
}
