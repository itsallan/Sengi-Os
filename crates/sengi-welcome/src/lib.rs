//! sengi-welcome, the first-run welcome of Sengi OS: a short wizard shown
//! once, on the first login on an installed system. It is not a service:
//! it starts when asked for and is gone when its window is closed.
//!
//! `model` is the order of the steps, `marker` the once-only rule, `system`
//! what it reads from the machine and the commands it runs, `app` joins
//! them to the window in `ui/`.

pub mod app;
pub mod marker;
pub mod model;
pub mod system;

pub mod ui {
    slint::include_modules!();
}
