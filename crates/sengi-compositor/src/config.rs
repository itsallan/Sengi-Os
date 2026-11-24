//! What the user set in Settings that the compositor acts on: the
//! keyboard's layouts and repeat, and how mice and touchpads behave.
//!
//! It is read from the settings file itself, `sengi-os/settings.toml`,
//! the one Settings writes. Settings also writes the same things into
//! labwc's `rc.xml` and `environment`, for labwc; nothing here reads
//! those. The file is read at start and again on SIGHUP.

use sengi_config::{Dirs, Settings};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub keyboard: Keyboard,
    pub mouse: Mouse,
    pub touchpad: Touchpad,
    pub appearance: Appearance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appearance {
    /// Whether framed windows have a shadow.
    pub window_shadows: bool,
}

/// The layouts as XKB takes them: names, variants and options, each a
/// list with commas. From the settings the layout is always named, `us`
/// too: an empty name would have XKB fall back on the `XKB_DEFAULT_*`
/// variables in the compositor's own environment, which were right at
/// login and are not after a change. An empty layout stands for exactly
/// that, the environment's keyboard, and is what the login screen uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyboard {
    pub layout: String,
    pub variant: String,
    pub options: String,
    /// Milliseconds a key is held before it repeats.
    pub repeat_delay: i32,
    /// Repeats a second.
    pub repeat_rate: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mouse {
    /// From -1, slowest, to 1.
    pub speed: f64,
    pub natural_scroll: bool,
    pub left_handed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touchpad {
    pub speed: f64,
    pub natural_scroll: bool,
    pub tap: bool,
    pub disable_while_typing: bool,
}

/// The two kinds of pointing device Settings tells apart, as labwc does:
/// what can be tapped is a touchpad, the rest are mice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mouse,
    Touchpad,
}

impl Kind {
    pub fn of(tap_fingers: u32) -> Kind {
        if tap_fingers > 0 {
            Kind::Touchpad
        } else {
            Kind::Mouse
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Mouse => "mouse",
            Kind::Touchpad => "touchpad",
        }
    }
}

/// What one pointing device is to be set to. `None` leaves a setting the
/// device's kind does not have alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pointing {
    pub speed: f64,
    pub natural_scroll: bool,
    pub left_handed: Option<bool>,
    pub tap: Option<bool>,
    pub disable_while_typing: Option<bool>,
}

fn speed(settings: &Settings, section: &str) -> f64 {
    (settings.number(section, "speed") as f64 / 100.0).clamp(-1.0, 1.0)
}

impl Config {
    /// The settings in effect for whoever runs the session.
    pub fn load() -> Config {
        let (settings, warnings) = Settings::load(&Dirs::from_env());
        for warning in warnings {
            crate::say!("{warning}");
        }
        Config::from_settings(&settings)
    }

    pub fn from_settings(settings: &Settings) -> Config {
        let layouts = settings.layouts();
        let (layout, variant) = if layouts.is_empty() {
            ("us".to_string(), String::new())
        } else {
            let join = |pick: fn(&sengi_config::settings::Layout) -> &str| {
                layouts.iter().map(pick).collect::<Vec<_>>().join(",")
            };
            (join(|l| &l.layout), join(|l| &l.variant))
        };
        Config {
            keyboard: Keyboard {
                layout,
                variant,
                options: settings.text("keyboard", "switch"),
                repeat_delay: settings.number("keyboard", "repeat-delay").clamp(1, 10_000) as i32,
                repeat_rate: settings.number("keyboard", "repeat-rate").clamp(0, 1_000) as i32,
            },
            mouse: Mouse {
                speed: speed(settings, "mouse"),
                natural_scroll: settings.flag("mouse", "natural-scroll"),
                left_handed: settings.flag("mouse", "left-handed"),
            },
            touchpad: Touchpad {
                speed: speed(settings, "touchpad"),
                natural_scroll: settings.flag("touchpad", "natural-scroll"),
                tap: settings.flag("touchpad", "tap-to-click"),
                disable_while_typing: settings.flag("touchpad", "disable-while-typing"),
            },
            appearance: Appearance {
                window_shadows: settings.flag("appearance", "window-shadows"),
            },
        }
    }

    /// The keyboard as the environment has it, with Settings' defaults
    /// for the rest: for the login screen, which runs as a user of its
    /// own with no settings, and gets its layout from the installer
    /// through `XKB_DEFAULT_*`.
    pub fn from_environment() -> Config {
        let mut config = Config::from_settings(&Settings::default());
        config.keyboard.layout.clear();
        config.keyboard.variant.clear();
        config.keyboard.options.clear();
        config
    }

    /// What a device of `kind` is set to.
    pub fn pointing(&self, kind: Kind) -> Pointing {
        match kind {
            Kind::Mouse => Pointing {
                speed: self.mouse.speed,
                natural_scroll: self.mouse.natural_scroll,
                left_handed: Some(self.mouse.left_handed),
                tap: None,
                disable_while_typing: None,
            },
            Kind::Touchpad => Pointing {
                speed: self.touchpad.speed,
                natural_scroll: self.touchpad.natural_scroll,
                left_handed: None,
                tap: Some(self.touchpad.tap),
                disable_while_typing: Some(self.touchpad.disable_while_typing),
            },
        }
    }
}

impl Keyboard {
    /// One line for the SIGUSR1 report.
    pub fn describe(&self) -> String {
        let or_default = |text: &str| {
            if text.is_empty() {
                "(default)".to_string()
            } else {
                text.to_string()
            }
        };
        format!(
            "keyboard: layout {}, variant {}, options {}, repeat {} a second after {} ms",
            or_default(&self.layout),
            or_default(&self.variant),
            or_default(&self.options),
            self.repeat_rate,
            self.repeat_delay
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sengi_config::Document;

    fn config(user: &str) -> Config {
        let (user, warnings) = Document::parse(user);
        assert!(warnings.is_empty(), "{warnings:?}");
        Config::from_settings(&Settings::from_documents(Document::default(), user, None))
    }

    #[test]
    fn with_nothing_set_the_layout_is_named_and_the_defaults_are_settings_own() {
        let config = config("");
        assert_eq!(config.keyboard.layout, "us");
        assert_eq!(config.keyboard.variant, "");
        assert_eq!(config.keyboard.options, "");
        assert_eq!(config.keyboard.repeat_delay, 600);
        assert_eq!(config.keyboard.repeat_rate, 25);
        assert_eq!(config.mouse.speed, 0.0);
        assert!(!config.mouse.natural_scroll);
        assert!(config.touchpad.tap);
        assert!(config.touchpad.disable_while_typing);
    }

    #[test]
    fn the_environments_keyboard_is_an_empty_layout() {
        let config = Config::from_environment();
        assert_eq!(config.keyboard.layout, "");
        assert_eq!(config.keyboard.repeat_delay, 600);
    }

    #[test]
    fn layouts_and_variants_are_lists_of_the_same_length() {
        let config = config(
            "[keyboard]\nlayouts = [\"us\", \"fr(azerty)\", \"de\"]\nswitch = \"grp:alt_shift_toggle\"\n",
        );
        assert_eq!(config.keyboard.layout, "us,fr,de");
        assert_eq!(config.keyboard.variant, ",azerty,");
        assert_eq!(config.keyboard.options, "grp:alt_shift_toggle");
    }

    #[test]
    fn a_single_layout_other_than_us_is_named() {
        let config = config("[keyboard]\nlayouts = [\"de\"]\n");
        assert_eq!(config.keyboard.layout, "de");
        assert_eq!(config.keyboard.variant, "");
    }

    #[test]
    fn repeat_and_pointer_settings_are_taken_over() {
        let config = config(
            "[keyboard]\nrepeat-delay = 300\nrepeat-rate = 40\n\
             [mouse]\nspeed = -50\nnatural-scroll = true\nleft-handed = true\n\
             [touchpad]\nspeed = 100\ntap-to-click = false\ndisable-while-typing = false\n",
        );
        assert_eq!(config.keyboard.repeat_delay, 300);
        assert_eq!(config.keyboard.repeat_rate, 40);
        assert_eq!(config.mouse.speed, -0.5);
        assert!(config.mouse.natural_scroll && config.mouse.left_handed);
        assert_eq!(config.touchpad.speed, 1.0);
        assert!(!config.touchpad.tap && !config.touchpad.disable_while_typing);
    }

    #[test]
    fn shadows_are_on_unless_switched_off() {
        assert!(config("").appearance.window_shadows);
        let off = config("[appearance]\nwindow-shadows = false\n");
        assert!(!off.appearance.window_shadows);
    }

    #[test]
    fn each_kind_of_device_gets_its_own_settings_and_no_others() {
        let config = config("[mouse]\nnatural-scroll = true\n[touchpad]\ntap-to-click = false\n");
        let mouse = config.pointing(Kind::Mouse);
        assert!(mouse.natural_scroll);
        assert_eq!(mouse.left_handed, Some(false));
        assert_eq!(mouse.tap, None);
        assert_eq!(mouse.disable_while_typing, None);
        let touchpad = config.pointing(Kind::Touchpad);
        assert!(!touchpad.natural_scroll);
        assert_eq!(touchpad.tap, Some(false));
        assert_eq!(touchpad.left_handed, None);
        assert_eq!(Kind::of(0), Kind::Mouse);
        assert_eq!(Kind::of(3), Kind::Touchpad);
    }
}
