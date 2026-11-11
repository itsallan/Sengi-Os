//! sengi-dock, the Sengi OS dock.
//!
//! The pieces are kept apart so each can be tested alone: desktop entries
//! and pins, matching windows to apps, the item list and click rules, icon
//! lookup, launching, and the window tracker. `app` joins them to the Slint
//! component; `main.rs` only has to put that component on a layer surface.

pub mod app;
pub mod config;
pub mod desktop;
pub mod exec;
pub mod icons;
pub mod launch;
pub mod matching;
pub mod model;
pub mod windows;

/// The Slint component and its types, compiled from `ui/dock.slint`.
pub mod ui {
    slint::include_modules!();
}
