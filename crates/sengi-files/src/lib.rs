//! sengi-files, the Sengi OS file manager: the folders of the home
//! directory in a window, with the Trash, and the few things done to files
//! every day. Not a service: it starts when asked for and is gone when its
//! window is closed.

pub mod app;
pub mod devices;
pub mod fsops;
pub mod guard;
pub mod hotplug;
pub mod jobs;
pub mod menu;
pub mod open;
pub mod session;
pub mod udisks;
pub mod watch;

pub mod ui {
    slint::include_modules!();
}
