//! Updates for Sengi OS: the system's packages through apt, and the
//! user's apps through Flatpak.
//!
//! Two programs use this crate. `sengi-update`, the helper, is run as
//! root through pkexec and does the one thing that needs root. Settings
//! links the library, checks for updates as the user, runs the helper and
//! Flatpak, and reads what they report. Nothing here is a service: every
//! call starts a process, waits for it and ends.

pub mod apt;
pub mod cache;
pub mod client;
pub mod elevate;
pub mod event;
pub mod flatpak;
pub mod guard;
pub mod helper;
pub mod json;
pub mod run;
pub mod tools;

#[cfg(test)]
pub(crate) mod testutil;

pub use apt::Upgrade;
pub use event::{Event, Phase, Progress};
pub use flatpak::AppUpdate;
pub use guard::{Paths, Refusal};
pub use tools::Tools;

/// Seconds since 1970, or 0 on a clock set before it.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The last non-empty line of some output, cleaned for showing.
pub fn last_line(text: &str) -> String {
    let line = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    sengi_text::line(line, 160)
}
