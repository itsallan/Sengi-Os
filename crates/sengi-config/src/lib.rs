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

/// Whether the light theme is chosen, read from the settings files as this
/// process sees them. Every program with a window calls this once at start
/// and hands the answer to the `light` property of its `Tokens` global.
///
/// `SENGI_THEME=light` or `dark` in the environment wins over the files, for
/// trying a program in either theme without touching the setting.
pub fn theme_is_light() -> bool {
    static LIGHT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LIGHT.get_or_init(|| match std::env::var("SENGI_THEME").as_deref() {
        Ok(settings::THEME_LIGHT) => true,
        Ok(settings::THEME_DARK) => false,
        _ => Settings::load(&Dirs::from_env()).0.light(),
    })
}

#[cfg(test)]
mod testutil;
