//! The arithmetic of the brightness keys: where one press of each key
//! takes a screen or a keyboard light.

use std::fs;
use std::path::Path;

use sengi_system::backlight::{self, Backlight};

/// How far one press of a screen brightness key moves, in percent.
pub const STEP_PERCENT: u32 = 5;

/// What a key asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    KeyboardUp,
    KeyboardDown,
    /// Off if it is on, otherwise back on, as the key on a ZenBook does.
    KeyboardToggle,
}

impl Key {
    pub fn parse(word: &str) -> Option<Key> {
        Some(match word {
            "up" => Key::Up,
            "down" => Key::Down,
            "kbd-up" => Key::KeyboardUp,
            "kbd-down" => Key::KeyboardDown,
            "kbd-toggle" => Key::KeyboardToggle,
            _ => return None,
        })
    }
}

/// The raw value one press of Up or Down leads to. It moves by
/// `STEP_PERCENT`, by at least one raw step on a device that has few,
/// and never below the floor the quick settings slider keeps either.
pub fn next_screen(light: &Backlight, up: bool) -> u32 {
    let percent = light.percent();
    let target = if up {
        percent + STEP_PERCENT
    } else {
        percent.saturating_sub(STEP_PERCENT)
    };
    let mut raw = light.raw(target);
    // Percent rounding can leave a coarse device where it was.
    if raw == light.brightness {
        raw = if up {
            (light.brightness + 1).min(light.max)
        } else {
            light.brightness.saturating_sub(1).max(light.floor())
        };
    }
    // A press down never raises it and a press up never lowers it, which
    // matters when the screen already sits below the floor.
    if up {
        raw.max(light.brightness.min(light.max))
    } else {
        raw.min(light.brightness)
            .max(light.floor().min(light.brightness))
    }
}

/// The raw value a keyboard light key leads to. Unlike the screen the
/// light may go all the way off, and it has a handful of levels.
pub fn next_keyboard(light: &Backlight, key: Key) -> u32 {
    match key {
        Key::KeyboardUp => (light.brightness + 1).min(light.max),
        Key::KeyboardDown => light.brightness.saturating_sub(1),
        Key::KeyboardToggle if light.brightness > 0 => 0,
        // Back to the middle of the range, or the one level there is.
        Key::KeyboardToggle => light.max.div_ceil(2).max(1),
        Key::Up | Key::Down => light.brightness,
    }
}

/// The keyboard light of the machine, from `/sys/class/leds`, if it has
/// one. A ZenBook calls it `asus::kbd_backlight`; other makers use the
/// same ending.
pub fn read_keyboard(leds: &Path) -> Option<Backlight> {
    let mut found: Vec<Backlight> = fs::read_dir(leds)
        .ok()?
        .flatten()
        .take(64)
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            if !name.contains("kbd_backlight") {
                return None;
            }
            let path = entry.path();
            let file = |name: &str| fs::read_to_string(path.join(name)).ok();
            backlight::parse(&name, &file("brightness")?, &file("max_brightness")?)
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found.into_iter().next()
}

/// What the on-screen indicator is told, in its own words.
pub fn osd_arguments(key: Key, percent: u32) -> [String; 3] {
    let kind = match key {
        Key::Up | Key::Down => "brightness",
        _ => "keyboard",
    };
    ["show".to_string(), kind.to_string(), percent.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(brightness: u32, max: u32) -> Backlight {
        Backlight {
            name: "intel_backlight".into(),
            brightness,
            max,
        }
    }

    #[test]
    fn each_press_moves_five_percent() {
        assert_eq!(next_screen(&light(500, 1000), true), 550);
        assert_eq!(next_screen(&light(500, 1000), false), 450);
        assert_eq!(next_screen(&light(990, 1000), true), 1000);
        assert_eq!(next_screen(&light(1000, 1000), true), 1000);
    }

    #[test]
    fn the_screen_never_goes_below_five_percent() {
        assert_eq!(next_screen(&light(60, 1000), false), 50);
        assert_eq!(next_screen(&light(50, 1000), false), 50);
        // Already below the floor: down leaves it, up brings it to the floor.
        assert_eq!(next_screen(&light(10, 1000), false), 10);
        assert!(next_screen(&light(10, 1000), true) >= 50);
        // A panel with no zero to step to still moves by whole steps.
        assert_eq!(next_screen(&light(0, 1000), false), 0);
        assert!(next_screen(&light(0, 1000), true) >= 50);
    }

    #[test]
    fn a_coarse_panel_still_moves_on_every_press() {
        // Fifteen steps: five percent is under one of them.
        assert_eq!(next_screen(&light(8, 15), true), 9);
        assert_eq!(next_screen(&light(8, 15), false), 7);
        // The floor of such a panel is one step.
        assert_eq!(next_screen(&light(1, 15), false), 1);
    }

    #[test]
    fn the_keyboard_light_steps_through_its_levels() {
        let light = light(1, 3);
        assert_eq!(next_keyboard(&light, Key::KeyboardUp), 2);
        assert_eq!(next_keyboard(&light, Key::KeyboardDown), 0);
        let off = Backlight {
            brightness: 0,
            ..light.clone()
        };
        assert_eq!(next_keyboard(&off, Key::KeyboardDown), 0);
        let full = Backlight {
            brightness: 3,
            ..light.clone()
        };
        assert_eq!(next_keyboard(&full, Key::KeyboardUp), 3);
    }

    #[test]
    fn the_toggle_switches_off_and_comes_back_half_way() {
        let on = light(2, 3);
        assert_eq!(next_keyboard(&on, Key::KeyboardToggle), 0);
        let off = light(0, 3);
        assert_eq!(next_keyboard(&off, Key::KeyboardToggle), 2);
        let one_level = light(0, 1);
        assert_eq!(next_keyboard(&one_level, Key::KeyboardToggle), 1);
    }

    #[test]
    fn keys_have_names() {
        assert_eq!(Key::parse("up"), Some(Key::Up));
        assert_eq!(Key::parse("kbd-toggle"), Some(Key::KeyboardToggle));
        assert_eq!(Key::parse("sideways"), None);
    }

    #[test]
    fn the_keyboard_light_is_found_by_its_name() {
        let dir = std::env::temp_dir().join(format!("sengi-brightness-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (name, value) in [("asus::kbd_backlight", "1"), ("input3::capslock", "0")] {
            let path = dir.join(name);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("brightness"), format!("{value}\n")).unwrap();
            fs::write(path.join("max_brightness"), "3\n").unwrap();
        }
        let found = read_keyboard(&dir).expect("the ZenBook light");
        assert_eq!(found.name, "asus::kbd_backlight");
        assert_eq!((found.brightness, found.max), (1, 3));
        assert_eq!(read_keyboard(&dir.join("missing")), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_indicator_hears_a_kind_and_a_percent() {
        assert_eq!(osd_arguments(Key::Up, 55), ["show", "brightness", "55"]);
        assert_eq!(
            osd_arguments(Key::KeyboardToggle, 0),
            ["show", "keyboard", "0"]
        );
    }
}
