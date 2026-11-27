//! sengi-welcome, the first-run welcome of Sengi OS: a short wizard shown
//! once, on the first login on an installed system, and on the live image a
//! question, install or try first. It is not a service: it starts when asked
//! for and is gone when its window is closed.
//!
//! `model` is the order of the steps, `marker` the once-only rules, `system`
//! what it reads from the machine and the commands it runs, `app` joins
//! them to the window in `ui/`, and `live` is the live image's question.

pub mod app;
pub mod live;
pub mod marker;
pub mod model;
pub mod system;

pub mod ui {
    slint::include_modules!();
}
