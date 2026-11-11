//! Every other program the panel runs, in one place.

/// PipeWire's command-line tool, from the wireplumber package.
pub const WPCTL: &str = "wpctl";
/// The default output, in wpctl's terms.
pub const SINK: &str = "@DEFAULT_AUDIO_SINK@";

/// The notification service. `sengi-notify --dnd on|off` switches
/// do-not-disturb. It has no way to ask which it is, so the panel only
/// knows what it set itself.
pub const NOTIFY: &str = "sengi-notify";
pub const DND_FLAG: &str = "--dnd";
pub const DND_ON: &str = "on";
pub const DND_OFF: &str = "off";

/// Run through `sh -c` after the panel has closed.
pub const LOCK: &str = "swaylock -f";
/// The Lock / Log out / Restart / Shut down menu.
pub const SESSION: &str = "sengi-power";
