//! Installed applications for the Sengi OS shell: finding and reading
//! desktop entries, turning an Exec line into a command, and starting it.
//!
//! [`desktop`] finds and parses desktop files, [`exec`] builds and starts
//! their command lines, [`index`] looks installed applications up by ID and
//! [`matching`] finds the one a window belongs to. [`env`] is the environment
//! they read, and [`files`] reads files without trusting their size.

pub mod desktop;
pub mod env;
pub mod exec;
pub mod files;
pub mod index;
pub mod matching;

pub use desktop::DesktopEntry;
pub use env::Env;
pub use index::AppIndex;

#[cfg(test)]
mod testutil;
