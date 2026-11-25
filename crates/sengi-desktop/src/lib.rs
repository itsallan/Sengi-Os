//! sengi-desktop, the Sengi OS desktop.
//!
//! The icons of ~/Desktop on the wallpaper, with a menu on the right button.
//! The pieces are kept apart so each can be tested alone: `files` lists and
//! renames, `trash` moves to the Trash, `open` starts other programs,
//! `watch` tells of changes, `desk` holds the selection and the rules, and
//! `app` joins that to the Slint component. `main.rs` puts the component on
//! a layer surface per output.

pub mod app;
pub mod desk;
pub mod files;
pub mod open;
pub mod trash;
pub mod watch;

#[cfg(test)]
mod testing;

/// The Slint component and its types, compiled from `ui/desktop.slint`.
pub mod ui {
    slint::include_modules!();
}
