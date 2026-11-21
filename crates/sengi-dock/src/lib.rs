//! sengi-dock, the Sengi OS dock.
//!
//! The pieces are kept apart so each can be tested alone: pins, the item
//! list and click rules, icon lookup, launching, and the window tracker.
//! Desktop entries and matching windows to apps come from sengi-apps. `app` joins them to the Slint
//! component; `main.rs` only has to put that component on a layer surface.

pub mod app;
pub mod config;
pub mod launch;
pub mod model;
pub mod windows;

#[cfg(test)]
mod testing;

/// The Slint component and its types, compiled from `ui/dock.slint`.
pub mod ui {
    slint::include_modules!();
}
