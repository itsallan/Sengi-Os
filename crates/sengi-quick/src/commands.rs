//! The programs the panel itself runs.

/// Run through `sh -c` after the panel has closed.
pub const LOCK: &str = "sengi-lock -f";
/// The Lock / Log out / Restart / Shut down menu.
pub const SESSION: &str = "sengi-power";
/// The whole screen, as the Print key takes it. Saves to ~/Pictures and
/// posts the notification itself.
pub const SCREENSHOT: &str = "sengi-shot";
/// A rectangle dragged on the dimmed screen.
pub const SCREENSHOT_AREA: &str = "sengi-shot --select";
/// The whole screen after five seconds of countdown.
pub const SCREENSHOT_DELAYED: &str = "sengi-shot --delay 5";
