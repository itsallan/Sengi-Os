//! sengi-calendar, the popup that opens when the clock in the bar is
//! clicked. Not a service: it starts when asked for and is gone when it is
//! dismissed, by Escape, a click outside it or the keyboard going elsewhere.

pub mod cal;
pub mod view;

pub mod ui {
    slint::include_modules!();
}
