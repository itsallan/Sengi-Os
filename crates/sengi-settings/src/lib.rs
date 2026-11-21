//! sengi-settings, the Sengi OS settings application.
//!
//! The window is in `ui/`; `app` joins it to the pages. Each page keeps
//! its rules in a module of its own that knows nothing of Slint or of any
//! bus, with the service it talks to behind a trait, so the rules are
//! tested with made-up services. `store` and `settings` are the settings
//! file, `derive` the files other programs read from it.

pub mod about;
pub mod app;
pub mod bluetooth;
pub mod cli;
pub mod datetime;
pub mod derive;
pub mod display;
pub mod held;
pub mod instance;
pub mod keyboard;
pub mod network;
pub mod notifications;
pub mod pointer;
pub mod power;
pub mod screen;
pub mod services;
pub mod settings;
pub mod sound;
pub mod store;
pub mod worker;

/// The system bus, as sengi-system opens and reads it.
pub use sengi_system::bus;
/// Passphrases and PINs in memory, as sengi-system holds them.
pub use sengi_system::secret;

/// Cleaning text that came from outside, under the names the pages use.
pub mod text {
    pub use sengi_system::names::{name, MAX_NAME_CHARS};
    pub use sengi_text::{isolate, line, line as clean};
}

#[cfg(test)]
pub mod testutil;

pub mod ui {
    slint::include_modules!();
}
