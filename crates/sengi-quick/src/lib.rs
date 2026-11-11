//! The Sengi OS quick settings panel: Wi-Fi, Bluetooth, volume, brightness,
//! do-not-disturb, battery, and the way to the lock screen and the session
//! menu.
//!
//! Everything except opening the window lives here so it can be tested.
//! [`panel`] holds the state of an open panel and decides what it shows,
//! and [`ui`] connects a panel to the Slint window. What the panel needs
//! from the system comes from sengi-system: its `services` module defines
//! that as traits and runs each service on a thread of its own, it
//! implements them for the real machine, and `demo` here, in debug builds,
//! implements them for a made-up one.

pub mod cli;
pub mod commands;
// Not in release builds: a panel showing invented state must not be
// something the shipped program can be talked into.
#[cfg(debug_assertions)]
pub mod demo;
pub mod instance;
pub mod panel;
pub mod ui;

#[cfg(test)]
mod testutil;
