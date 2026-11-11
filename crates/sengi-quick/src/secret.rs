//! A Wi-Fi passphrase on its way from the text field to iwd.

use std::fmt;

/// Holds a passphrase. It cannot be printed or cloned, and its bytes are
/// overwritten when it is dropped.
pub struct Secret(String);

impl Secret {
    pub fn new(text: String) -> Secret {
        Secret(text)
    }

    /// The passphrase itself, for handing to iwd and for nothing else.
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
    fn a_secret_does_not_print_itself() {
        let secret = Secret::new("correct horse".to_string());
        assert_eq!(format!("{secret:?}"), "Secret(..)");
        assert_eq!(secret.expose(), "correct horse");
        assert_eq!(secret.len(), 13);
    }
}
