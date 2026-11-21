//! The other programs the services run, in one place.

/// PipeWire's command-line tool, from the wireplumber package.
pub const WPCTL: &str = "wpctl";
/// PipeWire's own tool that prints its objects, and with `--monitor`
/// goes on printing them as they change.
pub const PW_DUMP: &str = "pw-dump";
/// The default output, in wpctl's terms.
pub const SINK: &str = "@DEFAULT_AUDIO_SINK@";

/// The notification service. `sengi-notify --dnd on|off` switches
/// do-not-disturb and `--dnd status` prints `on` or `off`. The command
/// is used and not the service's bus interface: a call on the bus would
/// start the service from here if it were not running.
pub const NOTIFY: &str = "sengi-notify";
/// The option that reads or switches do-not-disturb.
pub const DND_FLAG: &str = "--dnd";
/// Its argument, and its answer, for on.
pub const DND_ON: &str = "on";
/// Its argument, and its answer, for off.
pub const DND_OFF: &str = "off";
/// The option that dismisses every notification on screen.
pub const DISMISS_ALL: &str = "--dismiss-all";
/// Its argument that asks which it is.
pub const DND_STATUS: &str = "status";
