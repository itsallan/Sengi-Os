//! The panel, as a Slint component, and what a level does to it.

use slint::SharedString;

use crate::level::Level;

slint::include_modules!();

/// Shows `level` in `window`.
pub fn apply(window: &OsdWindow, level: &Level) {
    window.set_glyph(SharedString::from(level.icon()));
    window.set_percent(level.bar() as i32);
}
