//! The Accessibility page: text size, high contrast, reduced motion and
//! the size of the pointer. The settings are `appearance.text-size`,
//! `appearance.theme` (high contrast is one of its three values),
//! `appearance.reduce-motion` and `appearance.cursor-size`.
//!
//! This window follows a change at once; the bar, the dock, the desktop
//! and the notifications are started again to read it, as for the theme
//! (see `theme`), and the compositor reads the file again for the title
//! bars and the pointer.

use std::rc::Rc;

use slint::ComponentHandle;

use crate::app::{App, Controller};
use crate::settings::{CURSOR_SIZES, TEXT_SIZES, THEME_DARK, THEME_HIGH_CONTRAST};
use crate::store::Value;
use crate::ui::AccessibilityData;

/// The word of the setting for the choice at `index` of the text size.
pub fn text_size_name(index: i32) -> Option<&'static str> {
    usize::try_from(index)
        .ok()
        .and_then(|i| TEXT_SIZES.get(i))
        .map(|(name, _)| *name)
}

/// The word of the setting for the choice at `index` of the cursor size.
pub fn cursor_size_name(index: i32) -> Option<&'static str> {
    usize::try_from(index)
        .ok()
        .and_then(|i| CURSOR_SIZES.get(i))
        .map(|(name, _)| *name)
}

/// Where a setting's word is among the choices; the first when it is not
/// one of them.
pub fn index_of(choices: &[(&str, u32)], name: &str) -> i32 {
    choices
        .iter()
        .position(|(known, _)| *known == name)
        .unwrap_or(0) as i32
}

/// The theme after the High contrast switch is turned `on`. Turning it off
/// leaves the dark theme, the one the setting starts as; any other theme
/// is already not high contrast and stays.
pub fn theme_after_switch(current: &str, on: bool) -> &str {
    match (on, current == THEME_HIGH_CONTRAST) {
        (true, _) => THEME_HIGH_CONTRAST,
        (false, true) => THEME_DARK,
        (false, false) => current,
    }
}

pub struct AccessibilityPage;

impl AccessibilityPage {
    pub fn attach(app: &Rc<App>) -> Rc<AccessibilityPage> {
        let data = app.window.global::<AccessibilityData>();
        let weak = Rc::downgrade(app);
        data.on_set_text_size({
            let weak = weak.clone();
            move |index| {
                if let (Some(app), Some(name)) = (weak.upgrade(), text_size_name(index)) {
                    change(&app, "text-size", Value::text(name), true);
                }
            }
        });
        data.on_set_cursor_size({
            let weak = weak.clone();
            move |index| {
                if let (Some(app), Some(name)) = (weak.upgrade(), cursor_size_name(index)) {
                    change(&app, "cursor-size", Value::text(name), false);
                }
            }
        });
        data.on_set_reduce_motion({
            let weak = weak.clone();
            move |on| {
                if let Some(app) = weak.upgrade() {
                    change(&app, "reduce-motion", Value::Flag(on), true);
                }
            }
        });
        data.on_set_high_contrast(move |on| {
            if let Some(app) = weak.upgrade() {
                let theme =
                    theme_after_switch(&app.settings.borrow().text("appearance", "theme"), on)
                        .to_string();
                change(&app, "theme", Value::Text(theme), true);
            }
        });
        Rc::new(AccessibilityPage)
    }

    fn render(&self, app: &Rc<App>) {
        let settings = app.settings.borrow();
        let data = app.window.global::<AccessibilityData>();
        data.set_text_size(index_of(
            &TEXT_SIZES,
            &settings.text("appearance", "text-size"),
        ));
        data.set_cursor_size(index_of(
            &CURSOR_SIZES,
            &settings.text("appearance", "cursor-size"),
        ));
        data.set_high_contrast(settings.high_contrast());
        data.set_reduce_motion(settings.reduce_motion());
    }
}

/// Save `appearance.key` and have this window and the session show it.
/// `looks` says whether the window itself changes, which the pointer's
/// size does not.
fn change(app: &Rc<App>, key: &str, value: Value, looks: bool) {
    if !app.set("appearance", key, value) {
        return;
    }
    if looks {
        crate::theme::follow(&app.window, &app.settings.borrow());
    }
    app.after_saved(crate::theme::apply_to_session);
}

impl Controller for AccessibilityPage {
    fn show(&self, app: &Rc<App>) {
        self.render(app);
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choices_are_the_words_of_the_settings_in_order() {
        let names: Vec<_> = (0..4).filter_map(text_size_name).collect();
        assert_eq!(names, ["small", "default", "large", "extra-large"]);
        assert_eq!(text_size_name(4), None);
        assert_eq!(text_size_name(-1), None);
        let names: Vec<_> = (0..3).filter_map(cursor_size_name).collect();
        assert_eq!(names, ["normal", "large", "extra-large"]);
        assert_eq!(cursor_size_name(3), None);
    }

    #[test]
    fn a_word_is_found_among_the_choices_and_an_unknown_one_is_the_first() {
        assert_eq!(index_of(&TEXT_SIZES, "large"), 2);
        assert_eq!(index_of(&TEXT_SIZES, "extra-large"), 3);
        assert_eq!(index_of(&CURSOR_SIZES, "extra-large"), 2);
        assert_eq!(index_of(&CURSOR_SIZES, "huge"), 0);
    }

    #[test]
    fn the_high_contrast_switch_only_moves_between_high_contrast_and_dark() {
        assert_eq!(theme_after_switch("dark", true), "high-contrast");
        assert_eq!(theme_after_switch("light", true), "high-contrast");
        assert_eq!(theme_after_switch("high-contrast", false), "dark");
        assert_eq!(theme_after_switch("light", false), "light");
        assert_eq!(theme_after_switch("dark", false), "dark");
    }
}
