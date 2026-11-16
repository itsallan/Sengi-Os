//! Whose session this is.
//!
//! The user to authenticate is the one this process runs as, looked up by
//! number. Nothing typed, and nothing in the environment, has a say.

use std::ffi::CStr;

/// The longest name shown, in characters.
const MAX_SHOWN: usize = 48;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// The login name, which is what PAM is asked about.
    pub login: String,
    /// The name to show: the full name from the account if it has one,
    /// otherwise the login name. Safe to draw.
    pub shown: String,
}

/// The account this process runs as. None if it has no entry, in which
/// case nothing could unlock a screen locked in its name.
pub fn current() -> Option<User> {
    // SAFETY: getuid cannot fail and takes nothing.
    let uid = unsafe { libc::getuid() };
    let mut buffer = vec![0u8; 1024];
    loop {
        // SAFETY: zeroed is a valid passwd to be filled in.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is to memory that outlives the call, and
        // the length passed is the buffer's.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if status == libc::ERANGE && buffer.len() < 1 << 20 {
            buffer.resize(buffer.len() * 4, 0);
            continue;
        }
        if status != 0 || found.is_null() {
            return None;
        }
        // SAFETY: on success the strings are NUL-terminated and live in
        // `buffer`, which is still here.
        let (login, gecos) = unsafe {
            let text = |pointer: *const libc::c_char| {
                if pointer.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(pointer).to_string_lossy().into_owned()
                }
            };
            (text(entry.pw_name), text(entry.pw_gecos))
        };
        if login.is_empty() {
            return None;
        }
        let shown = shown_name(&login, &gecos);
        return Some(User { login, shown });
    }
}

/// The account's full name, which is the comment field up to its first
/// comma, or the login name when that is empty.
pub fn shown_name(login: &str, gecos: &str) -> String {
    let full = gecos.split(',').next().unwrap_or_default();
    let full = sengi_text::line(full, MAX_SHOWN);
    if full.is_empty() {
        sengi_text::line(login, MAX_SHOWN)
    } else {
        full
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_full_name_is_shown_when_there_is_one() {
        assert_eq!(shown_name("allan", "Allan Example,,,"), "Allan Example");
        assert_eq!(shown_name("live", "Sengi OS"), "Sengi OS");
        assert_eq!(shown_name("allan", ""), "allan");
        assert_eq!(shown_name("allan", ",Room 2,555"), "allan");
        assert_eq!(shown_name("allan", "  \t "), "allan");
    }

    #[test]
    fn a_name_cannot_take_over_the_screen() {
        let shown = shown_name("x", "Line one\nLine two\u{202e}");
        assert_eq!(shown, "Line one Line two");
        let long = "n".repeat(500);
        assert!(shown_name("x", &long).chars().count() <= MAX_SHOWN);
    }

    #[test]
    fn this_process_has_an_account() {
        // Not so in every build sandbox; where it is, the names are sane.
        if let Some(user) = current() {
            assert!(!user.login.is_empty());
            assert!(!user.shown.is_empty());
        }
    }
}
