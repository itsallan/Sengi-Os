//! sengi-files, the Sengi OS file manager: the folders of the home
//! directory in a window, with the Trash, and the few things done to files
//! every day. Not a service: it starts when asked for and is gone when its
//! window is closed.

pub mod app;
pub mod fsops;
pub mod guard;
pub mod jobs;
pub mod open;
pub mod session;

pub mod ui {
    slint::include_modules!();
}
