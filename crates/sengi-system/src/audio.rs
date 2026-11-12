//! The audio devices one by one: the outputs and inputs PipeWire has,
//! which is the default of each, and their volume and mute, through
//! WirePlumber's `wpctl`; and `pw-dump --monitor` to hear about changes.
//!
//! `wpctl` has no way to wait for a change, and looking every second
//! would wake the machine for nothing. `pw-dump -m` prints an object
//! whenever it changes. Most of what it prints is no news to a list of
//! devices, and some of it is of the reader's own making: every `wpctl`
//! run is a client that comes and goes. [`Changes`] reads just enough of
//! the stream to tell a device, a node or the defaults changing from the
//! rest.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use crate::services::{Notify, Refusal};
use crate::{commands, names, programs, volume};

/// How many devices of one direction are listed.
pub const MAX_NODES: usize = 32;

/// An output or an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// PipeWire's number for it, which is what `wpctl` is told.
    pub id: u32,
    /// Already cleaned for display.
    pub name: String,
    /// Its volume in percent. PipeWire allows more than 100.
    pub percent: u32,
    /// Whether it is muted.
    pub muted: bool,
    /// The one sound goes to, or comes from.
    pub default: bool,
}

/// The outputs and the inputs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Audio {
    /// Where sound can go.
    pub outputs: Vec<Node>,
    /// Where sound can come from.
    pub inputs: Vec<Node>,
}

impl Audio {
    /// The output sound goes to.
    pub fn output(&self) -> Option<&Node> {
        self.outputs.iter().find(|node| node.default)
    }

    /// The input sound comes from.
    pub fn input(&self) -> Option<&Node> {
        self.inputs.iter().find(|node| node.default)
    }

    /// The devices of one direction.
    pub fn side(&self, side: Side) -> &[Node] {
        match side {
            Side::Output => &self.outputs,
            Side::Input => &self.inputs,
        }
    }
}

/// A direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Out of the machine.
    Output,
    /// Into it.
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
        name: names::name(name, "Unnamed device"),
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

/// Audio for a program that manages the devices, where
/// [`AudioService`](crate::services::AudioService) only has the default
/// output's volume.
pub trait AudioDevices: Send + Sync + 'static {
    /// `None` when PipeWire is not there to ask.
    fn read(&self) -> Option<Audio>;
    /// Make the device `id` the one sound goes to, or comes from.
    fn set_default(&self, id: u32) -> Result<(), Refusal>;
    /// Set the volume of `id`, in percent. It is held at 100.
    fn set_volume(&self, id: u32, percent: u32) -> Result<(), Refusal>;
    /// Mute or unmute `id`.
    fn set_muted(&self, id: u32, muted: bool) -> Result<(), Refusal>;
    /// Call `changed` when a later `read` could differ. Watching again
    /// after `close` starts afresh.
    fn watch(&self, changed: Notify);
    /// Stop watching and let go of whatever the watch was using.
    fn close(&self);
}

/// The objects whose changes show in a list of devices: the devices, the
/// nodes on them, and the metadata that holds the defaults.
fn tells(kind: &[u8]) -> bool {
    [
        &b"PipeWire:Interface:Node"[..],
        b"PipeWire:Interface:Device",
        b"PipeWire:Interface:Metadata",
    ]
    .contains(&kind)
}

/// The keys of an object in the monitor's output that are read.
#[derive(Clone, Copy, PartialEq)]
enum Key {
    Id,
    Type,
    Info,
    Other,
}

/// How many objects are remembered. PipeWire on a desktop has a few
/// hundred; past this many, new ones are taken for uninteresting.
const MAX_KNOWN: usize = 4096;

/// Reads the output of `pw-dump --monitor` for whether anything in it is
/// news to a list of devices.
///
/// The output is a JSON array for every batch of changes, each element
/// an object with an `id`, a `type` and the rest; an object that has
/// gone is printed with its `id` and `"info": null`. Only those three
/// keys are read, and only as far as needed: this is a scanner that keeps
/// its place in the nesting, not a parser, and what it cannot follow it
/// takes for no news.
#[derive(Default)]
pub struct Changes {
    /// How deep in arrays and objects the scanner is. The objects that
    /// count are at depth 2.
    depth: usize,
    in_string: bool,
    escaped: bool,
    /// The string or bare value being read, at depth 2 only.
    token: Vec<u8>,
    /// The key last read, while its value is awaited.
    awaiting: Option<Key>,
    id: Option<u32>,
    /// Whether the object's type is one that tells, if it gave a type.
    kind: Option<bool>,
    removed: bool,
    /// Whether each object seen so far, by id, is of a type that tells.
    known: HashMap<u32, bool>,
}

impl Changes {
    /// A scanner at the start of the stream.
    pub fn new() -> Changes {
        Changes::default()
    }

    /// Take in the next bytes of the stream. True if an object that
    /// tells was printed, changed or gone, in them.
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        let mut news = false;
        for &byte in bytes {
            news |= self.step(byte);
        }
        news
    }

    fn step(&mut self, byte: u8) -> bool {
        if self.in_string {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.in_string = false;
                if self.depth == 2 {
                    self.string_ended();
                }
                return false;
            }
            if self.depth == 2 && self.token.len() < 64 {
                self.token.push(byte);
            }
            return false;
        }
        match byte {
            b'"' => {
                self.in_string = true;
                self.token.clear();
            }
            b'{' | b'[' => {
                // A value that is itself an object or a list is not read.
                if self.depth == 2 {
                    self.awaiting = None;
                }
                self.depth += 1;
                if self.depth == 2 {
                    (self.id, self.kind, self.removed) = (None, None, false);
                    self.awaiting = None;
                    self.token.clear();
                }
            }
            b'}' | b']' => {
                self.bare_ended();
                let was = self.depth;
                self.depth = self.depth.saturating_sub(1);
                if was == 2 && byte == b'}' {
                    return self.object_ended();
                }
            }
            b':' if self.depth == 2 => self.token.clear(),
            b',' => self.bare_ended(),
            byte if byte.is_ascii_whitespace() => self.bare_ended(),
            byte => {
                if self.depth == 2 && self.awaiting.is_some() && self.token.len() < 64 {
                    self.token.push(byte);
                }
            }
        }
        false
    }

    /// A string at depth 2 has ended: a key, or the value of one.
    fn string_ended(&mut self) {
        match self.awaiting.take() {
            None => {
                self.awaiting = Some(match self.token.as_slice() {
                    b"id" => Key::Id,
                    b"type" => Key::Type,
                    b"info" => Key::Info,
                    _ => Key::Other,
                });
            }
            Some(Key::Type) => self.kind = Some(tells(&self.token)),
            Some(_) => {}
        }
        self.token.clear();
    }

    /// A number, `null`, `true` or `false` at depth 2 has ended, if one
    /// was being read.
    fn bare_ended(&mut self) {
        if self.depth != 2 || self.token.is_empty() {
            return;
        }
        match self.awaiting.take() {
            Some(Key::Id) => {
                self.id = std::str::from_utf8(&self.token)
                    .ok()
                    .and_then(|digits| digits.parse().ok());
            }
            Some(Key::Info) => self.removed = self.token == b"null",
            _ => {}
        }
        self.token.clear();
    }

    /// An object at depth 2 is complete. Whether it is news.
    fn object_ended(&mut self) -> bool {
        let Some(id) = self.id else {
            return false;
        };
        let tells = match self.kind {
            Some(tells) => {
                if self.known.len() < MAX_KNOWN || self.known.contains_key(&id) {
                    self.known.insert(id, tells);
                }
                tells
            }
            // Gone, or printed without its type: what it was is what was
            // last said of it. One never seen before is nobody's news.
            None => self.known.get(&id).copied().unwrap_or(false),
        };
        if self.removed {
            self.known.remove(&id);
        }
        tells
    }
}

fn done(output: Option<String>) -> Result<(), Refusal> {
    output.map(|_| ()).ok_or(Refusal::Failed)
}

/// The audio devices through `wpctl`, with a `pw-dump --monitor` child
/// while they are being watched. The child is killed by `close`, and by
/// the kernel if this process dies first.
#[derive(Default)]
pub struct Pipewire {
    monitor: Mutex<Option<Child>>,
}

impl AudioDevices for Pipewire {
    fn read(&self) -> Option<Audio> {
        programs::output(commands::WPCTL, &["status"]).map(|status| parse_status(&status))
    }

    fn set_default(&self, id: u32) -> Result<(), Refusal> {
        done(programs::output(
            commands::WPCTL,
            &["set-default", &id.to_string()],
        ))
    }

    fn set_volume(&self, id: u32, percent: u32) -> Result<(), Refusal> {
        let args = volume::volume_args(&id.to_string(), percent);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        done(programs::output(commands::WPCTL, &args))
    }

    fn set_muted(&self, id: u32, muted: bool) -> Result<(), Refusal> {
        let args = volume::mute_args(&id.to_string(), muted);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        done(programs::output(commands::WPCTL, &args))
    }

    fn watch(&self, changed: Notify) {
        self.close();
        let path = std::env::var("PATH").unwrap_or_default();
        let Some(program) = programs::find_program(commands::PW_DUMP, &path) else {
            // No monitor: the list still works, it just does not hear
            // about changes made elsewhere until it is read again.
            return;
        };
        let mut command = Command::new(program);
        command
            .arg("--monitor")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // SAFETY: the closure runs in the child between fork and exec and
        // makes one async-signal-safe system call. It has the kernel end
        // the monitor if this process dies without cleaning up, as it
        // would on an abort.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let Ok(mut child) = command.spawn() else {
            return;
        };
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return;
        };
        *self.monitor.lock().unwrap_or_else(|p| p.into_inner()) = Some(child);
        let spawned = std::thread::Builder::new()
            .name("sengi-audio-watch".to_string())
            .spawn(move || {
                let mut changes = Changes::new();
                let mut buffer = [0u8; 8192];
                // The first thing it prints is everything there is. That
                // costs one read more than needed and nothing after it.
                while let Ok(read) = stdout.read(&mut buffer) {
                    if read == 0 {
                        break;
                    }
                    if changes.feed(&buffer[..read]) {
                        changed();
                    }
                }
            });
        if spawned.is_err() {
            self.close();
        }
    }

    fn close(&self) {
        let child = self
            .monitor
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Pipewire {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
             71. output_FL       > Dummy Output:playback_FL\t[active]

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
        assert_eq!(audio.outputs[0].name.chars().count(), names::MAX_NAME_CHARS);
        // A volume far out of range is held to something that can be shown.
        let loud = parse_status("Audio\n ├─ Sinks:\n │  1. X [vol: 99999]\n │  2. Y [vol: -3]\n");
        assert_eq!(loud.outputs[0].percent, 1000);
        assert_eq!(loud.outputs[1].percent, 0);
    }

    #[test]
    fn closing_without_a_monitor_is_harmless() {
        let service = Pipewire::default();
        service.close();
        service.close();
    }

    /// What `pw-dump --monitor` prints first: everything there is.
    const EVERYTHING: &str = r#"[
  {
    "id": 0,
    "type": "PipeWire:Interface:Core",
    "version": 4,
    "info": { "cookie": 1, "props": { "core.name": "pipewire-0" } }
  },
  {
    "id": 33,
    "type": "PipeWire:Interface:Client",
    "info": { "props": { "application.name": "WirePlumber" } }
  },
  {
    "id": 46,
    "type": "PipeWire:Interface:Device",
    "info": { "props": { "device.description": "Built-in \"Audio\" {x}" } }
  },
  {
    "id": 52,
    "type": "PipeWire:Interface:Node",
    "info": {
      "state": "suspended",
      "params": { "Props": [ { "volume": 0.75, "mute": false, "type": "x", "id": 9 } ] }
    }
  },
  {
    "id": 40,
    "type": "PipeWire:Interface:Metadata",
    "props": { "metadata.name": "default" },
    "metadata": [ { "subject": 0, "key": "default.audio.sink", "value": { "name": "a" } } ]
  }
]
"#;

    fn client(id: u32) -> String {
        format!(
            r#"[
  {{
    "id": {id},
    "type": "PipeWire:Interface:Client",
    "version": 3,
    "permissions": [ "r", "w", "x", "m" ],
    "info": {{
      "change-mask": [ "props" ],
      "props": {{ "application.name": "wpctl", "pipewire.sec.pid": 4242, "id": 52, "type": "PipeWire:Interface:Node" }}
    }}
  }}
]
"#
        )
    }

    fn gone(id: u32) -> String {
        format!("[\n  {{\n    \"id\": {id},\n    \"info\": null\n  }}\n]\n")
    }

    #[test]
    fn the_first_dump_is_news_and_a_client_coming_and_going_is_not() {
        let mut changes = Changes::new();
        assert!(changes.feed(EVERYTHING.as_bytes()));
        // A `wpctl status`: a client appears, says its piece and leaves.
        // Nothing nested in it is mistaken for an object of its own.
        for round in 0..3 {
            assert!(!changes.feed(client(70 + round).as_bytes()));
            assert!(!changes.feed(client(70 + round).as_bytes()));
            assert!(!changes.feed(gone(70 + round).as_bytes()));
        }
    }

    #[test]
    fn nodes_devices_and_defaults_changing_or_going_are_news() {
        let mut changes = Changes::new();
        changes.feed(EVERYTHING.as_bytes());
        let volume =
            r#"[ { "id": 52, "type": "PipeWire:Interface:Node", "info": { "params": {} } } ]"#;
        assert!(changes.feed(volume.as_bytes()));
        let default = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","metadata":[]}]"#;
        assert!(changes.feed(default.as_bytes()));
        // Headphones unplugged: the node goes, then its device.
        assert!(changes.feed(gone(52).as_bytes()));
        assert!(changes.feed(gone(46).as_bytes()));
        // Gone once. And something never seen going is nobody's news.
        assert!(!changes.feed(gone(52).as_bytes()));
        assert!(!changes.feed(gone(999).as_bytes()));
        // A client and a node in one batch: the node counts.
        let both = format!("{}{}", client(71), volume);
        assert!(changes.feed(both.as_bytes()));
        // Ports and links, which a stream starting makes, do not.
        let port = r#"[ { "id": 90, "type": "PipeWire:Interface:Port", "info": {} } ]"#;
        assert!(!changes.feed(port.as_bytes()));
    }

    #[test]
    fn the_stream_can_arrive_in_any_pieces() {
        let stream = format!("{EVERYTHING}{}{}", client(70), gone(70));
        for size in [1, 2, 3, 7, 64] {
            let mut changes = Changes::new();
            let mut news = Vec::new();
            for piece in stream.as_bytes().chunks(size) {
                news.push(changes.feed(piece));
            }
            // All of the news is within the first dump.
            let last = news.iter().rposition(|&n| n).unwrap();
            assert!(last * size < EVERYTHING.len(), "pieces of {size}");
        }
    }

    #[test]
    fn nonsense_on_the_stream_is_no_news_and_no_harm() {
        let mut changes = Changes::new();
        for junk in [
            &b"]]]}}}"[..],
            b"\xff\xfe\"\\",
            b"\"",
            b"[{\"id\": 99999999999, \"type\": \"PipeWire:Interface:Node\"}]",
            b"[{\"type\": \"PipeWire:Interface:Node\"}]",
            b"[[{\"id\": 1, \"type\": \"PipeWire:Interface:Node\"}]]",
        ] {
            assert!(!changes.feed(junk), "{junk:?}");
            changes = Changes::new();
        }
        // More objects than are remembered: the rest are not, and their
        // going is not news.
        let mut changes = Changes::new();
        for id in 0..(MAX_KNOWN as u32 + 10) {
            let node = format!(r#"[{{"id":{id},"type":"PipeWire:Interface:Port"}}]"#);
            changes.feed(node.as_bytes());
        }
        assert_eq!(changes.known.len(), MAX_KNOWN);
    }
}
