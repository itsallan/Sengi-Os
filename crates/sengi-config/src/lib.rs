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

#[cfg(test)]
mod testutil;
