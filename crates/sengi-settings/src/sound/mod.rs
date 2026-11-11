//! The Sound page's rules: the output and input devices PipeWire has,
//! which is the default of each, and their volume and mute. `wpctl` does
//! the talking to PipeWire and `page` puts it in the window.

pub mod page;
pub mod wpctl;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::text;
use crate::worker::{self, Handle, Inbox, Next, Notify};

/// The slider's range. PipeWire can go louder; Settings does not.
pub const MAX_PERCENT: u32 = 100;
pub const MAX_NODES: usize = 32;

/// An output or an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// PipeWire's number for it, which is what `wpctl` is told.
    pub id: u32,
    /// Already cleaned for display.
    pub name: String,
    pub percent: u32,
    pub muted: bool,
    /// The one sound goes to, or comes from.
    pub default: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Audio {
    pub outputs: Vec<Node>,
    pub inputs: Vec<Node>,
}

impl Audio {
    pub fn output(&self) -> Option<&Node> {
        self.outputs.iter().find(|node| node.default)
    }

    pub fn input(&self) -> Option<&Node> {
        self.inputs.iter().find(|node| node.default)
    }

    pub fn side(&self, side: Side) -> &[Node] {
        match side {
            Side::Output => &self.outputs,
            Side::Input => &self.inputs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Output,
    Input,
}

/// One line of a list in `wpctl status`, such as
/// ` │  *   46. Built-in Audio Analog Stereo  [vol: 0.40 MUTED]`.
fn parse_node(line: &str) -> Option<Node> {
    let line = line.trim_start_matches(|c: char| c.is_whitespace() || "│├└─".contains(c));
    let (default, line) = match line.strip_prefix('*') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, line),
    };
    let (id, rest) = line.split_once(". ")?;
    let id: u32 = id.parse().ok()?;
    // The name may have brackets of its own; the volume is the last pair.
    let (name, volume) = rest.rsplit_once("[vol:")?;
    let volume = volume.trim().strip_suffix(']')?;
    let mut parts = volume.split_whitespace();
    let level: f32 = parts.next()?.parse().ok()?;
    if !level.is_finite() {
        return None;
    }
    Some(Node {
        id,
        name: text::name(name, "Unnamed device"),
        // Far above anything real, but it keeps the arithmetic in range.
        percent: (level * 100.0).round().clamp(0.0, 1000.0) as u32,
        muted: parts.any(|part| part == "MUTED"),
        default,
    })
}

/// Reads the sinks and sources out of `wpctl status`. Only the `Audio`
/// part counts: `Video` has sources too, which are cameras.
pub fn parse_status(output: &str) -> Audio {
    #[derive(PartialEq)]
    enum List {
        None,
        Sinks,
        Sources,
    }
    let mut audio = Audio::default();
    let mut in_audio = false;
    let mut list = List::None;
    for line in output.lines().take(4096) {
        // A line that starts in the first column opens a part.
        if line.starts_with(|c: char| c.is_alphabetic()) {
            in_audio = line.trim() == "Audio";
            list = List::None;
            continue;
        }
        if !in_audio {
            continue;
        }
        let heading = line
            .trim_start_matches(|c: char| c.is_whitespace() || "│├└─".contains(c))
            .trim_end();
        if heading.ends_with(':') {
            list = match heading {
                "Sinks:" => List::Sinks,
                "Sources:" => List::Sources,
                _ => List::None,
            };
            continue;
        }
        let nodes = match list {
            List::Sinks => &mut audio.outputs,
            List::Sources => &mut audio.inputs,
            List::None => continue,
        };
        if nodes.len() < MAX_NODES {
            nodes.extend(parse_node(line));
        }
    }
    audio
}

pub trait AudioService: Send + Sync + 'static {
    /// `None` when PipeWire is not there to ask.
    fn read(&self) -> Option<Audio>;
    fn set_default(&self, id: u32);
    fn set_volume(&self, id: u32, percent: u32);
    fn set_muted(&self, id: u32, muted: bool);
    /// Call `changed` when anything in PipeWire changes.
    fn watch(&self, changed: Notify);
    /// Stop watching and let go of whatever the watch was using.
    fn close(&self);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Default(u32),
    /// A slider position. `sequence` numbers the positions of that side.
    Volume {
        side: Side,
        id: u32,
        percent: u32,
        sequence: u64,
    },
    Mute(u32, bool),
}

/// The newest slider position asked for, per side. A drag sends a
/// position for every step; the worker runs one `wpctl` at a time and
/// skips the positions the slider has already moved on from.
#[derive(Default)]
pub struct Latest {
    output: AtomicU64,
    input: AtomicU64,
}

impl Latest {
    fn slot(&self, side: Side) -> &AtomicU64 {
        match side {
            Side::Output => &self.output,
            Side::Input => &self.input,
        }
    }

    /// Number the next position of `side`.
    pub fn next(&self, side: Side) -> u64 {
        self.slot(side).fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn is_newest(&self, side: Side, sequence: u64) -> bool {
        self.slot(side).load(Ordering::SeqCst) == sequence
    }
}

pub fn run(
    service: Arc<dyn AudioService>,
    latest: Arc<Latest>,
    handle: Handle<Command>,
    mut inbox: Inbox<Command>,
    emit: impl Fn(Option<Audio>) + Send + Sync + 'static,
) {
    service.watch(handle.notifier());
    emit(service.read());
    while let Some(next) = inbox.wait() {
        match next {
            Next::Refresh => {}
            Next::Command(Command::Default(id)) => service.set_default(id),
            Next::Command(Command::Mute(id, muted)) => service.set_muted(id, muted),
            Next::Command(Command::Volume {
                side,
                id,
                percent,
                sequence,
            }) => {
                if !latest.is_newest(side, sequence) {
                    // Not worth a process, nor a reading that would only
                    // show a position the slider has left.
                    continue;
                }
                service.set_volume(id, percent.min(MAX_PERCENT));
            }
        }
        emit(service.read());
    }
    service.close();
}

pub fn start(
    service: Arc<dyn AudioService>,
    latest: Arc<Latest>,
    emit: impl Fn(Option<Audio>) + Send + Sync + 'static,
) -> Handle<Command> {
    worker::spawn("sound", move |handle, inbox| {
        run(service, latest, handle, inbox, emit)
    })
}

/// A made-up PipeWire, for the tests and, in debug builds only, for
/// looking at the page on a machine with no sound.
#[cfg(any(test, debug_assertions))]
pub mod testing {
    use super::*;
    use std::sync::Mutex;

    pub fn node(id: u32, name: &str, percent: u32, default: bool) -> Node {
        Node {
            id,
            name: name.to_string(),
            percent,
            muted: false,
            default,
        }
    }

    #[derive(Default)]
    pub struct Fake {
        pub audio: Mutex<Option<Audio>>,
        pub log: Mutex<Vec<String>>,
    }

    impl Fake {
        pub fn with(audio: Audio) -> Arc<Fake> {
            Arc::new(Fake {
                audio: Mutex::new(Some(audio)),
                log: Mutex::default(),
            })
        }

        fn change(&self, entry: String, change: impl Fn(&mut Vec<Node>)) {
            self.log
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(entry);
            if let Some(audio) = self
                .audio
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
            {
                change(&mut audio.outputs);
                change(&mut audio.inputs);
            }
        }
    }

    impl AudioService for Fake {
        fn read(&self) -> Option<Audio> {
            self.audio.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }

        fn set_default(&self, id: u32) {
            self.change(format!("default {id}"), |nodes| {
                if nodes.iter().any(|node| node.id == id) {
                    for node in nodes.iter_mut() {
                        node.default = node.id == id;
                    }
                }
            });
        }

        fn set_volume(&self, id: u32, percent: u32) {
            self.change(format!("volume {id} {percent}"), |nodes| {
                for node in nodes.iter_mut().filter(|node| node.id == id) {
                    node.percent = percent;
                }
            });
        }

        fn set_muted(&self, id: u32, muted: bool) {
            self.change(format!("mute {id} {muted}"), |nodes| {
                for node in nodes.iter_mut().filter(|node| node.id == id) {
                    node.muted = muted;
                }
            });
        }

        fn watch(&self, _changed: Notify) {}

        fn close(&self) {
            self.log
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push("close".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    const STATUS: &str = "\
PipeWire 'pipewire-0' [1.4.2, debian@builder, cookie:3470712318]
 └─ Clients:
        33. WirePlumber                         [1.4.2, debian@builder, pid:1234]
        48. wpctl                               [1.4.2, debian@builder, pid:1300]

Audio
 ├─ Devices:
 │      46. Built-in Audio                      [alsa]
 │
 ├─ Sinks:
 │      35. Dummy Output                        [vol: 0.40]
 │  *   52. Built-in Audio Analog Stereo        [vol: 0.75 MUTED]
 │      60. HDMI [Monitor] Output               [vol: 1.50]
 │
 ├─ Sources:
 │  *   53. Built-in Audio Analog Stereo        [vol: 1.00]
 │      61. Webcam Microphone                   [vol: 0.20]
 │
 ├─ Filters:
 │
 └─ Streams:
        70. Firefox
             71. output_FL       > Dummy Output:playback_FL	[active]

Video
 ├─ Devices:
 │      80. Webcam                              [v4l2]
 │
 ├─ Sinks:
 │
 ├─ Sources:
 │  *   81. Webcam (V4L2)
 │
 └─ Streams:

Settings
 └─ Default Configured Devices:
         0. Audio/Sink    alsa_output.pci-0000_00_1f.3.analog-stereo
";

    #[test]
    fn sinks_and_sources_are_read_from_the_audio_part() {
        let audio = parse_status(STATUS);
        let outputs: Vec<_> = audio
            .outputs
            .iter()
            .map(|n| (n.id, n.name.as_str(), n.percent, n.muted, n.default))
            .collect();
        assert_eq!(
            outputs,
            [
                (35, "Dummy Output", 40, false, false),
                (52, "Built-in Audio Analog Stereo", 75, true, true),
                (60, "HDMI [Monitor] Output", 150, false, false),
            ]
        );
        let inputs: Vec<_> = audio.inputs.iter().map(|n| (n.id, n.default)).collect();
        // The camera under Video is not a microphone.
        assert_eq!(inputs, [(53, true), (61, false)]);
        assert_eq!(audio.output().unwrap().id, 52);
        assert_eq!(audio.input().unwrap().id, 53);
    }

    #[test]
    fn output_that_is_not_a_status_gives_nothing() {
        for junk in [
            "",
            "Could not connect to PipeWire\n",
            "Audio\n",
            "Audio\n ├─ Sinks:\n │      nonsense\n │      12. No volume here\n │  x. y [vol: 1]\n",
            "Audio\n ├─ Sinks:\n │      7. Loud [vol: NaN]\n │      8. Odd [vol: ]\n │ 9. Open [vol: 0.5\n",
            "Video\n ├─ Sinks:\n │      7. Screen [vol: 0.5]\n",
        ] {
            assert_eq!(parse_status(junk), Audio::default(), "{junk:?}");
        }
    }

    #[test]
    fn hostile_names_are_cleaned_and_the_lists_bounded() {
        let mut status = String::from("Audio\n ├─ Sinks:\n");
        status.push_str(" │  *   5. Evil\u{202e}\u{7} speaker with a name that goes on and on and on  [vol: 0.50]\n");
        for i in 0..500 {
            status.push_str(&format!(" │      {}. Sink {i} [vol: 1.00]\n", 100 + i));
        }
        let audio = parse_status(&status);
        assert_eq!(audio.outputs.len(), MAX_NODES);
        assert!(audio.outputs[0].name.starts_with("Evil speaker"));
        assert_eq!(audio.outputs[0].name.chars().count(), text::MAX_NAME_CHARS);
        // A volume far out of range is held to something that can be shown.
        let loud = parse_status("Audio\n ├─ Sinks:\n │  1. X [vol: 99999]\n │  2. Y [vol: -3]\n");
        assert_eq!(loud.outputs[0].percent, 1000);
        assert_eq!(loud.outputs[1].percent, 0);
    }

    fn sample() -> Audio {
        Audio {
            outputs: vec![node(35, "Speakers", 40, true), node(60, "HDMI", 100, false)],
            inputs: vec![node(53, "Microphone", 80, true)],
        }
    }

    fn worked(commands: Vec<Command>, latest: Arc<Latest>) -> (Vec<String>, Vec<Option<Audio>>) {
        let fake = Fake::with(sample());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (handle, inbox) = worker::channel();
        for command in commands {
            handle.send(command);
        }
        handle.stop();
        run(fake.clone(), latest, handle, inbox, move |audio| {
            let _ = tx.lock().unwrap().send(audio);
        });
        let log = fake.log.lock().unwrap().clone();
        (log, rx.try_iter().collect())
    }

    #[test]
    fn commands_reach_the_service_and_each_is_followed_by_a_reading() {
        let latest = Arc::new(Latest::default());
        let sequence = latest.next(Side::Output);
        let (log, readings) = worked(
            vec![
                Command::Default(60),
                Command::Mute(53, true),
                Command::Volume {
                    side: Side::Output,
                    id: 60,
                    percent: 250,
                    sequence,
                },
            ],
            latest,
        );
        // Louder than the slider goes is held to its top.
        assert_eq!(
            log,
            ["default 60", "mute 53 true", "volume 60 100", "close"]
        );
        assert_eq!(readings.len(), 4);
        let last = readings.last().unwrap().as_ref().unwrap();
        assert_eq!(last.output().unwrap().id, 60);
        assert!(last.input().unwrap().muted);
    }

    #[test]
    fn a_drag_only_costs_its_last_position() {
        let latest = Arc::new(Latest::default());
        let mut commands = Vec::new();
        for percent in 41..=60 {
            commands.push(Command::Volume {
                side: Side::Output,
                id: 35,
                percent,
                sequence: latest.next(Side::Output),
            });
        }
        // The other side's slider has its own count.
        commands.push(Command::Volume {
            side: Side::Input,
            id: 53,
            percent: 10,
            sequence: latest.next(Side::Input),
        });
        let (log, _) = worked(commands, latest);
        assert_eq!(log, ["volume 35 60", "volume 53 10", "close"]);
    }

    #[test]
    fn pipewire_not_being_there_is_a_reading_of_nothing() {
        let fake = Arc::new(Fake::default());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (handle, inbox) = worker::channel();
        handle.stop();
        run(fake, Arc::default(), handle, inbox, move |audio| {
            let _ = tx.lock().unwrap().send(audio);
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(None));
    }
}
