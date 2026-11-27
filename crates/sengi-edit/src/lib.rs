//! sengi-edit, the Sengi OS text editor: one plain UTF-8 file in a window.
//! Not a service: it starts when asked for and is gone when its window is
//! closed.

pub mod app;
pub mod browse;
pub mod doc;
pub mod find;

pub mod ui {
    slint::include_modules!();
}
