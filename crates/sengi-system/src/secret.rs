//! A Wi-Fi passphrase on its way from the text field to iwd.
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
pub struct Secret(String);

impl Secret {
    /// Takes charge of `text`. From here on it is wiped when dropped.
    pub fn new(text: String) -> Secret {
        Secret(text)
    }

    /// The passphrase itself, for handing to iwd and for nothing else.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Its length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether it is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        for byte in bytes.iter_mut() {
            // Volatile, so the wipe of a buffer about to be freed is kept.
            unsafe { std::ptr::write_volatile(byte, 0) };
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
}
