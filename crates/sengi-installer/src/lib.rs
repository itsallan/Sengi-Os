//! sengi-installer, the window of the Sengi OS installer.
//!
//! It asks, shows what will happen, and follows the installation. It
//! never touches a disk itself: for that it starts sengi-install as
//! root and reads what that program reports. `model` is what the pages
//! show, worked out apart from the toolkit; the window is in `ui/`, and
//! `src/main.rs` joins the two.

pub mod model;

pub mod ui {
    slint::include_modules!();
}
