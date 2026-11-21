//! sengi-install, the part of the Sengi OS installer that writes to a
//! disk.
//!
//! `plan` is what an installation is told and `names` what may be in it;
//! `disks` is what there is to install on and what must be left alone;
//! `install` is the list of steps, `edit` the files they rewrite, and
//! `progress` what is said while they run. `real` runs the programs. The
//! window, sengi-installer, uses the first three and `progress`, and
//! starts this program for the rest.

pub mod disks;
pub mod edit;
pub mod install;
pub mod names;
pub mod plan;
pub mod progress;
pub mod real;

#[cfg(test)]
mod testutil;

/// Where the record of an installation is kept in the live session.
pub const LOG: &str = "/var/log/sengi-install.log";
/// Where the new system is mounted while it is put together.
pub const TARGET: &str = "/run/sengi-install/target";
/// Held for as long as an installation runs, so that there is one.
pub const LOCK: &str = "/run/sengi-install/lock";
