//! Installed applications for the Sengi OS shell: finding and reading
//! desktop entries, turning an Exec line into a command, and starting it.
//!
//! [`desktop`] finds and parses desktop files, [`exec`] builds and starts
//! their command lines, [`index`] looks installed applications up by ID and
//! [`matching`] finds the one a window belongs to. [`env`] is the environment
//! they read, and [`files`] reads files without trusting their size.
//!
//! With the `icons` feature, `icons` finds an application's or a tray
//! item's icon and decodes it in a resource-limited child process; `svg`
//! adds scalable icons.

pub mod desktop;
pub mod env;
pub mod exec;
pub mod files;
#[cfg(feature = "icons")]
pub mod icons;
pub mod index;
pub mod matching;

pub use desktop::DesktopEntry;
pub use env::Env;
pub use index::AppIndex;

#[cfg(test)]
mod testutil;
