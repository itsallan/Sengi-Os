//! Printing for Sengi OS, on top of CUPS.
//!
//! Two programs use this crate. `sengi-print`, the helper, is run as root
//! through pkexec and does the three things that need root: add a
//! printer, remove one, and look for printers on the network. Settings
//! links the library, reads what CUPS says as the user, and runs the
//! helper. Nothing here is a service: every call starts a process, waits
//! for it and ends.

pub mod cups;
pub mod exec;
pub mod helper;
pub mod parse;
pub mod valid;

#[cfg(test)]
pub(crate) mod testutil;

pub use cups::{Problem, Snapshot, Tools};
pub use parse::{Found, Job, Printer, State};
