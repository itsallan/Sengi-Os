//! Reading `sengi-os/settings.toml`, the settings Sengi OS keeps itself.
//!
//! sengi-settings shows and changes them; the bar and anything else that
//! only has to honour one reads them through this crate, so there is one
//! parser and one idea of what a valid value is. It depends on nothing but
//! the standard library.
//!
//! `store` is the file as text, `settings` what its keys mean.

pub mod settings;
pub mod store;

pub use settings::Settings;
pub use store::{Dirs, Document, Value};

/// How a program looks, read once as it starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Appearance {
    pub light: bool,
    pub high_contrast: bool,
    /// The type scale: 0.9, 1.0, 1.2 or 1.4.
    pub text_scale: f32,
    pub reduce_motion: bool,
}

/// The appearance the settings and the environment ask for. Without
/// the environment's say, `settings` decides.
pub fn appearance_of(
    settings: &Settings,
    theme: Option<&str>,
    text_size: Option<&str>,
    reduce_motion: Option<&str>,
) -> Appearance {
    let theme = theme.filter(|name| {
        matches!(
            *name,
            settings::THEME_LIGHT | settings::THEME_DARK | settings::THEME_HIGH_CONTRAST
        )
    });
    let percent = text_size
        .and_then(|name| {
            settings::TEXT_SIZES
                .iter()
                .find(|(known, _)| *known == name)
        })
        .map_or(settings.text_percent(), |(_, percent)| *percent);
    Appearance {
        light: theme.map_or(settings.light(), |name| name == settings::THEME_LIGHT),
        high_contrast: theme.map_or(settings.high_contrast(), |name| {
            name == settings::THEME_HIGH_CONTRAST
        }),
        text_scale: percent as f32 / 100.0,
        reduce_motion: match reduce_motion {
            Some("1" | "true") => true,
            Some("0" | "false") => false,
            _ => settings.reduce_motion(),
        },
    }
}

fn appearance() -> Appearance {
    static APPEARANCE: std::sync::OnceLock<Appearance> = std::sync::OnceLock::new();
    *APPEARANCE.get_or_init(|| {
        let (settings, _) = Settings::load(&Dirs::from_env());
        let var = |name: &str| std::env::var(name).ok();
        appearance_of(
            &settings,
            var("SENGI_THEME").as_deref(),
            var("SENGI_TEXT_SIZE").as_deref(),
            var("SENGI_REDUCE_MOTION").as_deref(),
        )
    })
}

/// Whether the light theme is chosen, read from the settings files as this
/// process sees them. Every program with a window gives its answer to the
/// `ThemeSwitch` global once at start, by way of `apply_theme!`.
///
/// `SENGI_THEME=light`, `dark` or `high-contrast` in the environment wins
/// over the files, for trying a program in any theme without touching the
/// setting.
pub fn theme_is_light() -> bool {
    appearance().light
}

/// Whether the high contrast theme is chosen. `light` is false then.
pub fn theme_is_high_contrast() -> bool {
    appearance().high_contrast
}

/// What the type is multiplied by: 0.9, 1.0, 1.2 or 1.4, from
/// `appearance.text-size`. `SENGI_TEXT_SIZE` in the environment wins.
pub fn text_scale() -> f32 {
    appearance().text_scale
}

/// Whether animations are off, from `appearance.reduce-motion`.
/// `SENGI_REDUCE_MOTION=1` or `0` in the environment wins.
pub fn reduce_motion() -> bool {
    appearance().reduce_motion
}

/// Gives a program's `ThemeSwitch` global everything it needs to look as
/// the settings say: the palette, the text scale and the motion. Takes the
/// global, as in `apply_theme!(window.global::<ThemeSwitch>())`.
#[macro_export]
macro_rules! apply_theme {
    ($switch:expr) => {{
        let switch = $switch;
        switch.set_light($crate::theme_is_light());
        switch.set_high_contrast($crate::theme_is_high_contrast());
        switch.set_text_scale($crate::text_scale());
        switch.set_reduce_motion($crate::reduce_motion());
    }};
}

#[cfg(test)]
mod testutil;

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(user: &str) -> Settings {
        Settings::from_documents(Document::parse("").0, Document::parse(user).0, None)
    }

    #[test]
    fn the_settings_decide_without_the_environment() {
        let chosen = settings(
            "[appearance]\ntheme = \"high-contrast\"\ntext-size = \"extra-large\"\nreduce-motion = true\n",
        );
        let found = appearance_of(&chosen, None, None, None);
        assert!(found.high_contrast && !found.light && found.reduce_motion);
        assert_eq!(found.text_scale, 1.4);
        let plain = appearance_of(&settings(""), None, None, None);
        assert_eq!(
            plain,
            Appearance {
                light: false,
                high_contrast: false,
                text_scale: 1.0,
                reduce_motion: false
            }
        );
    }

    #[test]
    fn the_environment_wins_and_a_word_nobody_knows_is_ignored() {
        let chosen = settings("[appearance]\ntheme = \"light\"\ntext-size = \"large\"\n");
        let found = appearance_of(&chosen, Some("high-contrast"), Some("small"), Some("1"));
        assert!(found.high_contrast && !found.light && found.reduce_motion);
        assert_eq!(found.text_scale, 0.9);
        let found = appearance_of(&chosen, Some("dark"), None, Some("0"));
        assert!(!found.light && !found.high_contrast);
        assert_eq!(found.text_scale, 1.2);
        let found = appearance_of(&chosen, Some("purple"), Some("huge"), Some("maybe"));
        assert!(found.light);
        assert_eq!(found.text_scale, 1.2);
    }
}
