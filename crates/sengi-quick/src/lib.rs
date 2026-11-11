//! The Sengi OS quick settings panel: Wi-Fi, Bluetooth, volume, brightness,
//! do-not-disturb, battery, and the way to the lock screen and the session
//! menu.
//!
//! Everything except opening the window lives here so it can be tested.
//! [`panel`] holds the state of an open panel and decides what it shows.
//! [`services`] defines what the panel needs from the system, as traits,
//! and runs each service on a thread of its own; [`system`] implements them
//! for the real machine and [`demo`] for a made-up one. [`wifi`],
//! [`bluetooth`], [`volume`], [`backlight`] and [`battery`] are the rules
//! for each tile, and [`ui`] connects a panel to the Slint window.

pub mod backlight;
pub mod battery;
pub mod bluetooth;
pub mod cli;
pub mod commands;
pub mod demo;
pub mod instance;
pub mod panel;
pub mod secret;
pub mod services;
pub mod system;
pub mod text;
pub mod ui;
pub mod volume;
pub mod wifi;

#[cfg(test)]
mod testutil;
