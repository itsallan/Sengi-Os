//! sengi-store, the Sengi OS app store: pick an app from a category or type
//! its name, and it is installed for this user from Flathub.

pub mod app;
pub mod catalog;
pub mod featured;
pub mod flatpak;
pub mod icons;
pub mod nav;
pub mod online;
pub mod problem;
pub mod progress;
pub mod rows;

pub mod ui {
    slint::include_modules!();
}
