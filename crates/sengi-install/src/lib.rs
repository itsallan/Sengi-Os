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
/// What an installation says as it goes, a line an event, for whoever
/// wants to follow it: the window, when it started the installation and
/// when it was opened again while one runs.
pub const PROGRESS: &str = "/run/sengi-install/progress";
/// Where the password and the plan wait, for the moment between the
/// window handing them over and the installation taking them.
pub const REQUEST: &str = "/run/sengi-install/request";
/// The transient service an installation started by the window runs as.
pub const UNIT: &str = "sengi-install.service";
