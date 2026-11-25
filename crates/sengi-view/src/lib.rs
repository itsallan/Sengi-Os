//! sengi-view, the Sengi OS image viewer: one PNG or JPEG in a window, with
//! the pictures beside it a key away. Not a service: it starts when asked for
//! and is gone when its window is closed.

pub mod app;
pub mod decode;
pub mod folder;
pub mod scale;

pub mod ui {
    slint::include_modules!();
}
