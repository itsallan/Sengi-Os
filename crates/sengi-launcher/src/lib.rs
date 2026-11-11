//! The Sengi OS launcher: search installed applications and start one, or
//! pick a line from a menu given on standard input.
//!
//! Everything except opening the window lives here so it can be tested:
//! [`desktop`] finds applications, [`exec`] builds their command lines,
//! [`matcher`] ranks them against what was typed, [`session`] holds the state
//! of an open panel and [`ui`] connects that state to the Slint component.

pub mod apps;
pub mod cli;
pub mod desktop;
pub mod env;
pub mod exec;
pub mod matcher;
pub mod menu;
pub mod run;
pub mod session;
pub mod ui;
pub mod usage;

#[cfg(test)]
mod testutil;
