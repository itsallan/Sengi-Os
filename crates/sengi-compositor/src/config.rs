//! What the user set in Settings that the compositor acts on: the
//! keyboard's layouts and repeat, and how mice and touchpads behave.
//!
//! It is read from the settings file itself, `sengi-os/settings.toml`,
//! the one Settings writes. Settings also writes the same things into
//! labwc's `rc.xml` and `environment`, for labwc; nothing here reads
//! those. The file is read at start and again on SIGHUP.

use sengi_config::{Dirs, Settings};
use sengi_tokens::{Palette, Tokens};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub keyboard: Keyboard,
    pub mouse: Mouse,
    pub touchpad: Touchpad,
    pub appearance: Appearance,
    pub power: Power,
    pub display: Display,
    pub clipboard: Clipboard,
}

/// What `clipboard.*` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clipboard {
    /// Whether the last texts copied are kept, in memory, to paste again.
    pub history: bool,
}

/// What `display.*` says: which output the bar and dock go to, and the
/// night light. The sizes, places and scales of the outputs are
/// not here: they are the compositor's own file, see `output_store`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    /// The output that layer surfaces which name none go to. Empty is the
    /// first output.
    pub primary: String,
    pub night: NightLight,
}

/// The warm tint at night. Times are minutes after midnight, local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NightLight {
    pub enabled: bool,
    pub from: u32,
    pub to: u32,
    /// 0 to 100.
    pub strength: u32,
}

/// What `power.*` in the settings says, in the units it is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Power {
    /// Seconds without input before the screen is dimmed, switched off and
    /// locked. Zero is never.
    pub dim_after: i64,
    pub blank_after: i64,
    pub lock_after: i64,
    /// Lock the screen before the computer sleeps.
    pub lock_on_sleep: bool,
    /// Closing the lid does nothing: the compositor holds the lid switch
    /// so that logind does not suspend.
    pub lid_does_nothing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appearance {
    /// Whether framed windows have a shadow.
    pub window_shadows: bool,
    /// Whether frames, title bars and menus use the light palette.
    pub light: bool,
    /// Whether they use the high contrast palette, which wins over light.
    pub high_contrast: bool,
    /// How big the type of title bars and menus is, in percent.
    pub text_percent: u32,
    /// Whether animations are off. The compositor has none of its own to
    /// stop yet; this is where one that is added looks.
    pub reduce_motion: bool,
    /// The pointer's size in pixels, for the compositor's own cursor and
    /// for the programs it starts.
    pub cursor: u32,
}

impl Appearance {
    /// The palette in use.
    pub fn palette(&self) -> Palette {
        if self.high_contrast {
            Palette::HighContrast
        } else if self.light {
            Palette::Light
        } else {
            Palette::Dark
        }
    }

    /// The design tokens as these settings have them: the palette and
    /// the text size applied.
    pub fn tokens(&self) -> Tokens {
        Tokens::for_appearance(self.palette(), self.text_percent)
    }

    /// Whether `other` changes what the tokens say.
    pub fn tokens_differ(&self, other: &Appearance) -> bool {
        (self.light, self.high_contrast, self.text_percent)
            != (other.light, other.high_contrast, other.text_percent)
    }
}

/// The pointer's size: what the setting says, except that Normal leaves
/// the size the environment was started with, `XCURSOR_SIZE`, alone.
pub fn cursor_size(settings: &Settings, environment: Option<&str>) -> u32 {
    let chosen = settings.cursor_px();
    let normal = sengi_config::settings::CURSOR_SIZES[0].1;
    match environment.and_then(|size| size.parse::<u32>().ok()) {
        Some(size) if chosen == normal && (1..=512).contains(&size) => size,
        _ => chosen,
    }
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
    pub click_method: ClickMethod,
    pub scroll_method: ScrollMethod,
    /// Whether three-finger swipes do anything.
    pub gestures: bool,
}

/// How a press on a touchpad's body becomes a button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickMethod {
    /// The lower left and right of the pad are the two buttons.
    Areas,
    /// The number of fingers on the pad picks the button.
    Clickfinger,
}

/// How fingers on a touchpad scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollMethod {
    TwoFinger,
    /// One finger along the right or bottom edge.
    Edge,
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
    pub click_method: Option<ClickMethod>,
    pub scroll_method: Option<ScrollMethod>,
}

fn speed(settings: &Settings, section: &str) -> f64 {
    (settings.number(section, "speed") as f64 / 100.0).clamp(-1.0, 1.0)
}

fn clock(settings: &Settings, key: &str, fallback: u32) -> u32 {
    sengi_config::settings::parse_clock(&settings.text("display", key)).unwrap_or(fallback)
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
                click_method: match settings.text("touchpad", "click-method").as_str() {
                    "clickfinger" => ClickMethod::Clickfinger,
                    _ => ClickMethod::Areas,
                },
                scroll_method: match settings.text("touchpad", "scroll-method").as_str() {
                    "edge" => ScrollMethod::Edge,
                    _ => ScrollMethod::TwoFinger,
                },
                gestures: settings.flag("touchpad", "gestures"),
            },
            display: Display {
                primary: settings.text("display", "primary"),
                night: NightLight {
                    enabled: settings.flag("display", "night-light"),
                    from: clock(settings, "night-from", 21 * 60),
                    to: clock(settings, "night-to", 7 * 60),
                    strength: settings.number("display", "night-strength").clamp(0, 100) as u32,
                },
            },
            clipboard: Clipboard {
                history: settings.flag("clipboard", "history"),
            },
            appearance: Appearance {
                window_shadows: settings.flag("appearance", "window-shadows"),
                light: settings.light(),
                high_contrast: settings.high_contrast(),
                text_percent: settings.text_percent(),
                reduce_motion: settings.reduce_motion(),
                cursor: cursor_size(settings, std::env::var("XCURSOR_SIZE").ok().as_deref()),
            },
            power: Power {
                dim_after: settings.number("power", "dim-after"),
                blank_after: settings.number("power", "blank-after"),
                lock_after: settings.number("power", "lock-after"),
                lock_on_sleep: settings.flag("power", "lock-on-sleep"),
                lid_does_nothing: settings.text("power", "lid") == "nothing",
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
                click_method: None,
                scroll_method: None,
            },
            Kind::Touchpad => Pointing {
                speed: self.touchpad.speed,
                natural_scroll: self.touchpad.natural_scroll,
                left_handed: None,
                tap: Some(self.touchpad.tap),
                disable_while_typing: Some(self.touchpad.disable_while_typing),
                click_method: Some(self.touchpad.click_method),
                scroll_method: Some(self.touchpad.scroll_method),
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
    fn the_power_defaults_dim_then_switch_off_and_lock_only_for_sleep() {
        let power = config("").power;
        assert_eq!(power.dim_after, 120);
        assert_eq!(power.blank_after, 300);
        assert_eq!(power.lock_after, 0);
        assert!(power.lock_on_sleep);
        assert!(!power.lid_does_nothing);
    }

    #[test]
    fn the_power_settings_are_taken_over() {
        let power = config(
            "[power]\ndim-after = 30\nblank-after = 90\nlock-after = 600\n\
             lock-on-sleep = false\nlid = \"nothing\"\n",
        )
        .power;
        assert_eq!(
            (power.dim_after, power.blank_after, power.lock_after),
            (30, 90, 600)
        );
        assert!(!power.lock_on_sleep && power.lid_does_nothing);
    }

    #[test]
    fn the_theme_is_dark_unless_light_is_chosen() {
        assert!(!config("").appearance.light);
        // Dark while the light theme is not offered.
        assert_eq!(
            config("[appearance]\ntheme = \"light\"\n").appearance.light,
            sengi_config::LIGHT_THEME_ENABLED
        );
    }

    #[test]
    fn high_contrast_text_size_and_motion_come_from_the_settings() {
        let plain = config("").appearance;
        assert_eq!(plain.palette(), Palette::Dark);
        assert_eq!((plain.text_percent, plain.reduce_motion), (100, false));
        let chosen = config(
            "[appearance]\ntheme = \"high-contrast\"\ntext-size = \"extra-large\"\nreduce-motion = true\ncursor-size = \"large\"\n",
        )
        .appearance;
        // Dark while high contrast is not offered.
        let palette = if sengi_config::HIGH_CONTRAST_ENABLED {
            Palette::HighContrast
        } else {
            Palette::Dark
        };
        assert_eq!(chosen.palette(), palette);
        assert!(!chosen.light);
        assert_eq!(chosen.text_percent, 140);
        assert!(chosen.reduce_motion);
        assert_eq!(chosen.cursor, 36);
        // Title bars follow: bigger type, and a thicker border in high
        // contrast.
        let (big, small) = (chosen.tokens(), plain.tokens());
        assert_eq!(big.px("type.size-md"), Some(18));
        assert_eq!(small.px("type.size-md"), Some(13));
        let border = if sengi_config::HIGH_CONTRAST_ENABLED {
            2
        } else {
            1
        };
        assert_eq!(big.px("border.width"), Some(border));
        assert!(chosen.tokens_differ(&plain));
        assert!(!plain.tokens_differ(&config("").appearance));
        // A new pointer size alone leaves the tokens as they are.
        let cursor = config("[appearance]\ncursor-size = \"extra-large\"\n").appearance;
        assert!(!cursor.tokens_differ(&plain));
        assert_ne!(cursor, plain);
    }

    #[test]
    fn the_pointer_size_follows_the_setting_and_the_environment_only_for_normal() {
        let normal = Settings::default();
        assert_eq!(cursor_size(&normal, None), 24);
        assert_eq!(cursor_size(&normal, Some("32")), 32);
        assert_eq!(cursor_size(&normal, Some("junk")), 24);
        assert_eq!(cursor_size(&normal, Some("0")), 24);
        let large = config("[appearance]\ncursor-size = \"large\"\n");
        assert_eq!(large.appearance.cursor, 36);
        let settings = {
            let (user, _) =
                sengi_config::Document::parse("[appearance]\ncursor-size = \"extra-large\"\n");
            Settings::from_documents(sengi_config::Document::parse("").0, user, None)
        };
        assert_eq!(cursor_size(&settings, Some("32")), 48);
    }

    #[test]
    fn shadows_are_on_unless_switched_off() {
        assert!(config("").appearance.window_shadows);
        let off = config("[appearance]\nwindow-shadows = false\n");
        assert!(!off.appearance.window_shadows);
    }

    #[test]
    fn the_touchpad_defaults_are_button_areas_and_two_finger_scrolling() {
        let touchpad = config("").touchpad;
        assert_eq!(touchpad.click_method, ClickMethod::Areas);
        // A tap with two fingers is a right click too.
        assert!(touchpad.tap);
        assert_eq!(touchpad.scroll_method, ScrollMethod::TwoFinger);
        assert!(touchpad.gestures);
        let set = config(
            "[touchpad]\nclick-method = \"clickfinger\"\nscroll-method = \"edge\"\ngestures = false\n",
        )
        .touchpad;
        assert_eq!(set.click_method, ClickMethod::Clickfinger);
        assert_eq!(set.scroll_method, ScrollMethod::Edge);
        assert!(!set.gestures);
        // A mouse has neither.
        let mouse = config("").pointing(Kind::Mouse);
        assert_eq!((mouse.click_method, mouse.scroll_method), (None, None));
        let pad = config("[touchpad]\nscroll-method = \"edge\"\n").pointing(Kind::Touchpad);
        assert_eq!(pad.scroll_method, Some(ScrollMethod::Edge));
    }

    #[test]
    fn the_clipboard_history_is_off_unless_asked_for() {
        assert!(!config("").clipboard.history);
        assert!(config("[clipboard]\nhistory = true\n").clipboard.history);
    }

    #[test]
    fn the_display_settings_default_to_nothing_special() {
        let display = config("").display;
        assert_eq!(display.primary, "");
        assert!(!display.night.enabled);
        assert_eq!((display.night.from, display.night.to), (21 * 60, 7 * 60));
        assert_eq!(display.night.strength, 50);
        let set = config(
            "[display]\nprimary = \"HDMI-A-1\"\nnight-light = true\nnight-from = \"22:30\"\nnight-to = \"06:15\"\nnight-strength = 80\n",
        )
        .display;
        assert_eq!(set.primary, "HDMI-A-1");
        assert!(set.night.enabled);
        assert_eq!(
            (set.night.from, set.night.to, set.night.strength),
            (1350, 375, 80)
        );
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
