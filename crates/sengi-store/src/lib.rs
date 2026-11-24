//! sengi-store, the Sengi OS app store: type the name of an app, pick it
//! from what Flathub has, and it is installed for this user.

pub mod app;
pub mod catalog;
pub mod flatpak;
pub mod icons;
pub mod rows;

pub mod ui {
    slint::include_modules!();
}
