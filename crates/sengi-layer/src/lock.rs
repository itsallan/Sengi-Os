//! What a session locker needs beyond a layer surface.

use std::fmt;

/// Why a session lock was not taken, or how it ended without an unlock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockError {
    /// The compositor does not speak ext-session-lock-v1.
    Unsupported,
    /// The compositor turned the lock down before the session was locked.
    /// The usual reason is that another locker already holds it.
    Refused,
    /// The compositor ended a lock that was held, by some means of its own.
    Ended,
    /// The loop was asked to quit while the session was locked. The
    /// session stays locked.
    Abandoned,
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LockError::Unsupported => "the compositor does not support ext-session-lock-v1",
            LockError::Refused => {
                "the compositor refused the lock; the session may be locked already"
            }
            LockError::Ended => "the compositor ended the session lock",
            LockError::Abandoned => "stopped while the session was locked; it stays locked",
        })
    }
}

impl std::error::Error for LockError {}

/// The parts of the keyboard's state a password prompt shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keyboard {
    /// Caps Lock is on.
    pub caps_lock: bool,
    /// Which of the configured layouts is active, counted from 0.
    pub layout: u32,
}

/// Overwrites text that must not be left behind in freed memory. The
/// string is empty afterwards.
pub fn wipe(text: &mut String) {
    // SAFETY: zero bytes are valid UTF-8, and the length is cleared next.
    for byte in unsafe { text.as_bytes_mut() } {
        // Volatile, so the wipe of a buffer about to be freed is kept.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    text.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiped_text_is_zeroed_where_it_lay() {
        let mut text = String::from("hunter2");
        let (start, length) = (text.as_ptr(), text.len());
        wipe(&mut text);
        assert!(text.is_empty());
        // The buffer is still the string's own; only its length changed.
        let left = unsafe { std::slice::from_raw_parts(start, length) };
        assert_eq!(left, [0; 7]);
    }
}
