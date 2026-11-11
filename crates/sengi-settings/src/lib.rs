//! sengi-settings, the Sengi OS settings application.
//!
//! The window is in `ui/`; `app` joins it to the pages. Each page keeps
//! its rules in a module of its own that knows nothing of Slint or of any
//! bus, with the service it talks to behind a trait, so the rules are
//! tested with made-up services. `store` and `settings` are the settings
//! file, `derive` the files other programs read from it.

pub mod app;
pub mod bluetooth;
pub mod bus;
pub mod cli;
pub mod datetime;
pub mod derive;
pub mod display;
pub mod held;
pub mod instance;
pub mod keyboard;
pub mod network;
pub mod pointer;
pub mod power;
pub mod screen;
pub mod secret;
pub mod services;
pub mod settings;
pub mod sound;
pub mod store;
pub mod text;
pub mod worker;

#[cfg(test)]
pub mod testutil;

pub mod ui {
    slint::include_modules!();
}
