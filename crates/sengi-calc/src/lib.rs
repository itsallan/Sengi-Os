//! sengi-calc, the Sengi OS calculator. Not a service: it starts when asked
//! for and is gone when its window is closed.

pub mod app;
pub mod calc;
pub mod eval;

pub mod ui {
    slint::include_modules!();
}
