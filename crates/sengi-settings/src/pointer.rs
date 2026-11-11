//! The Mouse & Touchpad page: pointer speed, scrolling direction, button
//! order, tapping. The settings are `mouse.*` and `touchpad.*`; labwc
//! takes them from `<libinput>` in the generated `rc.xml` and applies
//! them, to devices already plugged in too, when told to reconfigure.

use std::rc::Rc;

use slint::ComponentHandle;

use crate::app::{App, Controller};
use crate::store::Value;
use crate::ui::{PointerData, PointerSide};

/// The settings that are switches, by section.
pub const FLAGS: [(&str, &str); 5] = [
    ("mouse", "natural-scroll"),
    ("mouse", "left-handed"),
    ("touchpad", "natural-scroll"),
    ("touchpad", "tap-to-click"),
    ("touchpad", "disable-while-typing"),
];

/// Whether `section.key` is one of the switches this page may set. The
/// names arrive from the window as text; nothing else is passed on.
pub fn is_flag(section: &str, key: &str) -> bool {
    FLAGS.contains(&(section, key))
}

pub fn is_section(section: &str) -> bool {
    matches!(section, "mouse" | "touchpad")
}

pub struct PointerPage;

impl PointerPage {
    pub fn attach(app: &Rc<App>) -> Rc<PointerPage> {
        let data = app.window.global::<PointerData>();
        let weak = Rc::downgrade(app);
        data.on_set_flag(move |section, key, on| {
            let Some(app) = weak.upgrade() else { return };
            if is_flag(&section, &key) && app.set(&section, &key, Value::Flag(on)) {
                app.apply_to_session();
            }
            PointerPage::render(&app);
        });
        let weak = Rc::downgrade(app);
        data.on_set_speed(move |section, speed| {
            let Some(app) = weak.upgrade() else { return };
            let speed = Value::Number(i64::from(speed.clamp(-100, 100)));
            if is_section(&section) && app.set(&section, "speed", speed) {
                app.apply_to_session();
            }
            PointerPage::render(&app);
        });
        Rc::new(PointerPage)
    }

    fn render(app: &Rc<App>) {
        let settings = app.settings.borrow();
        let side = |section: &str| PointerSide {
            natural_scroll: settings.flag(section, "natural-scroll"),
            left_handed: settings.flag(section, "left-handed"),
            tap_to_click: settings.flag(section, "tap-to-click"),
            disable_while_typing: settings.flag(section, "disable-while-typing"),
        };
        let data = app.window.global::<PointerData>();
        data.set_mouse(side("mouse"));
        data.set_touchpad(side("touchpad"));
        data.set_mouse_speed(settings.number("mouse", "speed") as i32);
        data.set_touchpad_speed(settings.number("touchpad", "speed") as i32);
    }
}

impl Controller for PointerPage {
    fn show(&self, app: &Rc<App>) {
        PointerPage::render(app);
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::check;

    #[test]
    fn every_switch_is_a_setting_and_nothing_else_is_a_switch() {
        for (section, key) in FLAGS {
            assert!(is_flag(section, key));
            assert!(check(section, key, &Value::Flag(true)).is_ok());
        }
        assert!(!is_flag("mouse", "tap-to-click"));
        assert!(!is_flag("power", "lock-on-sleep"));
        assert!(!is_flag("mouse", "speed"));
        assert!(is_section("mouse") && is_section("touchpad"));
        assert!(!is_section("keyboard"));
        for section in ["mouse", "touchpad"] {
            assert!(check(section, "speed", &Value::Number(-100)).is_ok());
            assert!(check(section, "speed", &Value::Number(100)).is_ok());
        }
    }
}
