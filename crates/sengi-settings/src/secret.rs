//! A Wi-Fi passphrase or Bluetooth PIN on its way from the text field to
//! the service that asked for it.
//!
//! What is and is not protected, so nobody reads more into this than is
//! there:
//!
//! * A secret is never logged, never put on a command line or in the
//!   environment, and never written to disk by this program. iwd and BlueZ
//!   store what they need, in their own files.
//! * The program's own copies, held as [`Secret`], are overwritten when
//!   they are dropped: the one taken from the text field and the one handed
//!   to the D-Bus reply.
//! * Copies the program does not own are freed without being overwritten:
//!   the text field's string inside Slint, and the outgoing message inside
//!   zbus. They stay in freed memory until it is reused or the process
//!   exits. Settings can stay open for hours, so that is longer than it is
//!   for the quick settings panel.
//! * Because of those, [`forbid_core_dumps`] is called before anything
//!   else. The program is built with `panic = "abort"`, and an abort must
//!   not leave a file with a passphrase in it.
//!
//! The same code as sengi-quick's `secret.rs`; both are to come from
//! sengi-system.

use std::fmt;

/// Keep this process out of core dumps, and closed to `ptrace` and
/// `/proc/PID/mem` from other processes of the same user. Not inherited by
/// the programs the window starts.
pub fn forbid_core_dumps() {
    // SAFETY: prctl with PR_SET_DUMPABLE takes two integers and touches no
    // memory of ours.
    let result = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) };
    if result != 0 {
        eprintln!("sengi-settings: could not turn off core dumps");
    }
}

/// Holds one copy of a passphrase or PIN. It cannot be printed or cloned, and its
/// bytes are overwritten when it is dropped. Other copies, made by code
/// that was handed the text, are not its to wipe: see the module's notes.
pub struct Secret(String);

impl Secret {
    pub fn new(text: String) -> Secret {
        Secret(text)
    }

    /// The secret itself, for handing to the service and for nothing else.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

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
