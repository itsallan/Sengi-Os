//! Installed applications for the Sengi OS shell: finding and reading
//! desktop entries, turning an Exec line into a command, and starting it.
//!
//! [`desktop`] finds and parses desktop files, [`exec`] builds and starts
//! their command lines, [`env`] is the environment both read, and [`files`]
//! reads files without trusting their size.

pub mod desktop;
pub mod env;
pub mod exec;
pub mod files;

pub use desktop::DesktopEntry;
pub use env::Env;

#[cfg(test)]
mod testutil;
