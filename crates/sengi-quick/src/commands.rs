//! Every other program the panel runs, in one place.

/// PipeWire's command-line tool, from the wireplumber package.
pub const WPCTL: &str = "wpctl";
/// The default output, in wpctl's terms.
pub const SINK: &str = "@DEFAULT_AUDIO_SINK@";

/// The notification service. `sengi-notify --dnd on|off` switches
/// do-not-disturb and `--dnd status` prints `on` or `off`. The command
/// is used and not the service's bus interface: a call on the bus would
/// start the service from here if it were not running.
pub const NOTIFY: &str = "sengi-notify";
pub const DND_FLAG: &str = "--dnd";
pub const DND_ON: &str = "on";
pub const DND_OFF: &str = "off";
pub const DND_STATUS: &str = "status";

/// Run through `sh -c` after the panel has closed.
pub const LOCK: &str = "swaylock -f";
/// The Lock / Log out / Restart / Shut down menu.
pub const SESSION: &str = "sengi-power";
