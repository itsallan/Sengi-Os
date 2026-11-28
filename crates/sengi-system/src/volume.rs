//! The default output's volume, through `wpctl`, the queue that keeps a
//! dragged slider from starting a process for every pixel, and the notice
//! that tells the bar the volume has just changed.

use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::{fs, io};

/// The socket under `$XDG_RUNTIME_DIR` on which the bar waits to hear
/// that the volume changed, so it need not ask often.
pub const NOTICE_SOCKET: &str = "sengi-volume.sock";

fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

/// Tells the bar that the volume may have changed. It never waits, and
/// with nothing listening it does nothing.
pub fn announce() {
    if let Some(dir) = runtime_dir() {
        announce_in(&dir);
    }
}

/// [`announce`] for the runtime directory `dir`.
pub fn announce_in(dir: &Path) {
    let Ok(socket) = UnixDatagram::unbound() else {
        return;
    };
    if socket.set_nonblocking(true).is_ok() {
        // A full queue means the listener has plenty to wake up for.
        let _ = socket.send_to(&[0], dir.join(NOTICE_SOCKET));
    }
}

/// The listening end of [`announce`]. What is sent carries nothing: the
/// listener asks PipeWire for the volume itself.
pub struct Notices(UnixDatagram);

impl Notices {
    pub fn listen() -> io::Result<Notices> {
        match runtime_dir() {
            Some(dir) => Notices::listen_in(&dir),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "XDG_RUNTIME_DIR is not set",
            )),
        }
    }

    /// Takes the socket over from whoever had it before, which for the
    /// one bar of a session is a bar that has gone.
    pub fn listen_in(dir: &Path) -> io::Result<Notices> {
        let path = dir.join(NOTICE_SOCKET);
        match fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        UnixDatagram::bind(path).map(Notices)
    }

    /// Sleeps until the next notice.
    pub fn wait(&self) -> io::Result<()> {
        self.0.recv(&mut [0]).map(drop)
    }
}

/// The slider's range. PipeWire can go louder; the panel does not.
pub const MAX_PERCENT: u32 = 100;

/// The default output's volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Volume {
    /// In percent. PipeWire allows more than 100.
    pub percent: u32,
    /// Whether the output is muted.
    pub muted: bool,
}

/// Parses `wpctl get-volume` output, for example `Volume: 0.40 [MUTED]`.
pub fn parse(output: &str) -> Option<Volume> {
    let rest = output.trim().strip_prefix("Volume:")?.trim();
    let mut parts = rest.split_whitespace();
    let level: f32 = parts.next()?.parse().ok()?;
    if !level.is_finite() {
        return None;
    }
    Some(Volume {
        // Far above anything real, but it keeps the arithmetic in range.
        percent: (level * 100.0).round().clamp(0.0, 1000.0) as u32,
        muted: parts.any(|part| part == "[MUTED]"),
    })
}

/// What PipeWire calls the output it makes up while a machine has none, so
/// that programs have somewhere to play to. Its description is
/// [`STAND_IN_DESCRIPTION`].
pub const STAND_IN_NAME: &str = "auto_null";
/// What `wpctl status` calls that output.
pub const STAND_IN_DESCRIPTION: &str = "Dummy Output";

/// Whether `wpctl inspect` output describes the stand-in and not a device.
pub fn is_stand_in(inspect: &str) -> bool {
    inspect.lines().any(|line| {
        line.trim_start_matches([' ', '*'])
            .strip_prefix("node.name = ")
            .is_some_and(|name| name.trim().trim_matches('"') == STAND_IN_NAME)
    })
}

/// The default output's volume, asked of `wpctl` through `run`, which gives
/// what a command printed if it succeeded. `None` when there is no sound
/// device: no output at all, or only the stand-in.
pub fn default_output(run: impl Fn(&[&str]) -> Option<String>) -> Option<Volume> {
    let volume = parse(&run(&["get-volume", crate::commands::SINK])?)?;
    let stand_in = run(&["inspect", crate::commands::SINK]).is_some_and(|text| is_stand_in(&text));
    (!stand_in).then_some(volume)
}

/// The arguments for `pw-cli` that list the nodes PipeWire has.
pub const LIST_NODES: [&str; 2] = ["ls", "Node"];

/// Whether `pw-cli ls Node` output has an output other than the stand-in.
///
/// This is what to ask while sound may still be starting. `wpctl` asks
/// every device about itself, and PipeWire 1.4 keeps the half an answer a
/// client left behind by leaving: one that ends just as a device appears
/// can leave it with no profiles, and the machine with no sound, until
/// WirePlumber is restarted.
pub fn lists_output(nodes: &str) -> bool {
    // One node's lines at a time, each from the line with its number.
    nodes.split("\tid ").any(|node| {
        let value = |key: &str| {
            node.lines().find_map(|line| {
                let rest = line.trim().strip_prefix(key)?.strip_prefix(" = ")?;
                Some(rest.trim_matches('"'))
            })
        };
        value("media.class")
            .is_some_and(|class| class.starts_with("Audio/Sink") || class == "Audio/Duplex")
            && value("node.name") != Some(STAND_IN_NAME)
    })
}

/// The arguments for `wpctl` that set the volume of `target`, which is a
/// node's number or one of wpctl's names for a default. `-l 1` holds it at
/// 100% whatever is asked for.
pub fn volume_args(target: &str, percent: u32) -> [String; 5] {
    [
        "set-volume".to_string(),
        "-l".to_string(),
        "1".to_string(),
        target.to_string(),
        format!("{}%", percent.min(MAX_PERCENT)),
    ]
}

/// The arguments for `wpctl` that mute or unmute `target`.
pub fn mute_args(target: &str, muted: bool) -> [String; 3] {
    [
        "set-mute".to_string(),
        target.to_string(),
        if muted { "1" } else { "0" }.to_string(),
    ]
}

/// The arguments for `wpctl` that set the default output's volume.
pub fn set_volume_args(percent: u32) -> [String; 5] {
    volume_args(crate::commands::SINK, percent)
}

/// The arguments for `wpctl` that mute or unmute the default output.
pub fn set_mute_args(muted: bool) -> [String; 3] {
    mute_args(crate::commands::SINK, muted)
}

/// A change asked for by a slider, numbered so its echo can be told from
/// a stale one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// Counts up with every change one slider asks for.
    pub sequence: u64,
    /// The value asked for, in percent.
    pub value: u32,
}

/// Of everything that queued up while the last change was being applied,
/// only the newest matters: the slider has moved on from the rest.
pub fn newest(queued: impl IntoIterator<Item = Step>) -> Option<Step> {
    queued.into_iter().max_by_key(|step| step.sequence)
}

/// The UI's side of a slider that is set through a slow worker. Readings
/// come back numbered with the last change the worker had applied when it
/// took them; one taken before the slider's latest move is out of date and
/// must not drag the knob back.
#[derive(Debug, Default)]
pub struct Echo {
    sent: u64,
}

impl Echo {
    /// Number the next change.
    pub fn next(&mut self, value: u32) -> Step {
        self.sent += 1;
        Step {
            sequence: self.sent,
            value,
        }
    }

    /// Whether a reading that saw changes up to `sequence` is current.
    pub fn current(&self, sequence: u64) -> bool {
        sequence >= self.sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_reaches_whoever_listens_last() {
        let dir = std::env::temp_dir().join(format!("sengi-notice-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Nothing listening is not an error.
        announce_in(&dir);

        let first = Notices::listen_in(&dir).unwrap();
        announce_in(&dir);
        first.wait().unwrap();
        // A bar started after one that died takes the socket over.
        drop(first);
        let second = Notices::listen_in(&dir).unwrap();
        announce_in(&dir);
        second.wait().unwrap();
        // A listener that is not reading never holds the sender up.
        for _ in 0..5000 {
            announce_in(&dir);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn volume_is_rounded_to_percent() {
        assert_eq!(
            parse("Volume: 0.40\n"),
            Some(Volume {
                percent: 40,
                muted: false
            })
        );
        assert_eq!(parse("Volume: 0.335").unwrap().percent, 34);
        assert_eq!(parse("Volume: 1.20").unwrap().percent, 120);
    }

    #[test]
    fn muted_volume() {
        let volume = parse("Volume: 0.65 [MUTED]\n").unwrap();
        assert!(volume.muted);
        assert_eq!(volume.percent, 65);
    }

    #[test]
    fn nonsense_is_not_a_volume() {
        assert_eq!(parse("Could not connect to PipeWire"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("Volume: loud"), None);
        assert_eq!(parse("Volume: NaN"), None);
        assert_eq!(parse("Volume: inf"), None);
        assert_eq!(parse("Volume: -3").unwrap().percent, 0);
        assert_eq!(parse("Volume: 1e30").unwrap().percent, 1000);
    }

    #[test]
    fn the_stand_in_output_is_no_sound_device() {
        let inspect = |name: &str| {
            format!("id 46, type PipeWire:Interface:Node\n  * node.name = \"{name}\"\n")
        };
        assert!(is_stand_in(&inspect("auto_null")));
        assert!(!is_stand_in(&inspect(
            "alsa_output.pci-0000_00_1f.3.analog-stereo"
        )));
        assert!(!is_stand_in(""));

        let machine = |name: &'static str| {
            move |args: &[&str]| match args[0] {
                "get-volume" => Some("Volume: 0.40\n".to_string()),
                _ => Some(inspect(name)),
            }
        };
        assert_eq!(default_output(machine("auto_null")), None);
        assert_eq!(default_output(machine("speakers")).unwrap().percent, 40);
        // No output at all: wpctl prints nothing to read.
        assert_eq!(default_output(|_| Some(String::new())), None);
        assert_eq!(default_output(|_| None), None);
    }

    #[test]
    fn an_output_is_found_in_the_list_of_nodes() {
        let node = |id: u32, name: &str, class: &str| {
            format!(
                "\tid {id}, type PipeWire:Interface:Node/3\n \t\tobject.serial = \"{id}\"\n \
                 \t\tnode.name = \"{name}\"\n \t\tmedia.class = \"{class}\"\n"
            )
        };
        let driver = "\tid 30, type PipeWire:Interface:Node/3\n \t\tnode.name = \"Dummy-Driver\"\n";
        let source = node(
            50,
            "alsa_input.pci-0000_00_1f.3.analog-stereo",
            "Audio/Source",
        );
        let midi = node(43, "Midi-Bridge", "Midi/Bridge");
        let stand_in = node(46, STAND_IN_NAME, "Audio/Sink");
        let none = format!("{driver}{midi}{source}{stand_in}");
        assert!(!lists_output(&none));
        assert!(!lists_output(""));
        assert!(!lists_output("Error: \"failed to connect\"\n"));
        for class in ["Audio/Sink", "Audio/Sink/Virtual", "Audio/Duplex"] {
            let speakers = node(49, "alsa_output.pci-0000_00_1f.3.analog-stereo", class);
            assert!(lists_output(&format!("{none}{speakers}")), "{class}");
        }
    }

    #[test]
    fn the_volume_command_is_capped() {
        assert_eq!(set_volume_args(40)[4], "40%");
        assert_eq!(set_volume_args(400)[4], "100%");
        assert_eq!(set_volume_args(40)[3], "@DEFAULT_AUDIO_SINK@");
        assert_eq!(set_mute_args(true)[2], "1");
        assert_eq!(set_mute_args(false)[2], "0");
    }

    #[test]
    fn a_node_is_addressed_by_its_number() {
        assert_eq!(
            volume_args("46", 40),
            ["set-volume", "-l", "1", "46", "40%"]
        );
        assert_eq!(volume_args("46", 400)[4], "100%");
        assert_eq!(mute_args("46", true), ["set-mute", "46", "1"]);
    }

    fn steps(values: &[u32]) -> Vec<Step> {
        let mut echo = Echo::default();
        values.iter().map(|&value| echo.next(value)).collect()
    }

    #[test]
    fn a_drag_collapses_to_its_last_position() {
        let drag = steps(&[40, 41, 43, 47, 52, 58, 61]);
        assert_eq!(
            newest(drag),
            Some(Step {
                sequence: 7,
                value: 61
            })
        );
        assert_eq!(newest(Vec::new()), None);
        // Whatever order they are looked at in.
        let mut reversed = steps(&[10, 20, 30]);
        reversed.reverse();
        assert_eq!(newest(reversed).unwrap().value, 30);
    }

    #[test]
    fn stale_readings_do_not_move_the_slider() {
        let mut echo = Echo::default();
        // Something else changed the volume before the slider was touched.
        assert!(echo.current(0));
        let first = echo.next(50);
        let second = echo.next(60);
        // The worker applied the first and reports what it then read.
        assert!(!echo.current(first.sequence));
        assert!(!echo.current(0));
        assert!(echo.current(second.sequence));
    }
}
