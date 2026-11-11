//! The programs the panel itself runs.

/// Run through `sh -c` after the panel has closed.
pub const LOCK: &str = "swaylock -f";
/// The Lock / Log out / Restart / Shut down menu.
pub const SESSION: &str = "sengi-power";
