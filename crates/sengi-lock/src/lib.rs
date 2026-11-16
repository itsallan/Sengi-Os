//! The Sengi OS lock screen, as a library.
//!
//! The program is `src/main.rs`. What is here is kept apart so it can be
//! tested without a compositor and so the login screen can use the parts
//! it shares with the lock screen:
//!
//! * [`entry`]: a password as it is typed, held where it can be wiped.
//! * [`backoff`]: how long a wrong password costs.
//! * [`user`]: whose session this is.
//! * [`screen`]: the lock screen's states and what it shows in each.
//! * [`supervise`]: keeping a locker alive while the session is locked.
//! * [`pam`], with the `pam` feature: asking PAM about a password.
//!
//! The password field and the clock themselves are Slint components in
//! `ui/components.slint`, and the clock's text comes from `sengi-clock`.

pub mod backoff;
pub mod cli;
pub mod entry;
#[cfg(feature = "pam")]
pub mod pam;
pub mod screen;
pub mod supervise;
pub mod user;
