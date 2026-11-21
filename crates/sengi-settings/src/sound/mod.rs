//! The Sound page's worker: which commands it takes and how a dragged
//! slider is kept from costing a process a position. The devices, their
//! reading out of `wpctl status` and the watch for changes are
//! sengi-system's `audio`; `page` puts them in the window.

pub mod page;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub use sengi_system::audio::{Audio, AudioDevices, Node, Side};
/// The slider's range. PipeWire can go louder; Settings does not.
pub use sengi_system::volume::MAX_PERCENT;

use crate::network::Refusal;
use crate::worker::{self, Handle, Inbox, Next};

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

/// What the worker tells the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The devices as they are now, or nothing without PipeWire.
    Reading(Option<Audio>),
    /// `what` could not be done. The reading that follows shows what is
    /// still true.
    Refused(&'static str),
}

/// The slider positions asked for and carried out, per side. A drag
/// sends a position for every step; the worker runs one `wpctl` at a
/// time and skips the positions the slider has already moved on from,
/// and the page takes no volume from a reading made before the newest
/// position was carried out, which would pull the knob back.
#[derive(Default)]
pub struct Latest {
    asked: [AtomicU64; 2],
    done: [AtomicU64; 2],
}

/// How far the worker had got when a reading was made: the number of the
/// last position it carried out, for the output and for the input.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stamp([u64; 2]);

fn slot(side: Side) -> usize {
    match side {
        Side::Output => 0,
        Side::Input => 1,
    }
}

impl Latest {
    /// Number the next position of `side`.
    pub fn next(&self, side: Side) -> u64 {
        self.asked[slot(side)].fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn is_newest(&self, side: Side, sequence: u64) -> bool {
        self.asked[slot(side)].load(Ordering::SeqCst) == sequence
    }

    /// The worker has carried out position `sequence` of `side`.
    fn carried_out(&self, side: Side, sequence: u64) {
        self.done[slot(side)].store(sequence, Ordering::SeqCst);
    }

    /// For the worker, as it makes a reading.
    pub fn stamp(&self) -> Stamp {
        Stamp([
            self.done[0].load(Ordering::SeqCst),
            self.done[1].load(Ordering::SeqCst),
        ])
    }

    /// Whether a reading stamped `stamp` shows the volume of `side` as
    /// the slider last asked for it, or as someone else has set it since.
    pub fn is_current(&self, side: Side, stamp: Stamp) -> bool {
        self.is_newest(side, stamp.0[slot(side)])
    }

    /// Positions sent to a worker that has been stopped are never carried
    /// out. The next worker starts level, or no reading would count.
    pub fn settle(&self) {
        for side in [0, 1] {
            self.done[side].store(self.asked[side].load(Ordering::SeqCst), Ordering::SeqCst);
        }
    }
}

/// The worker's loop. It starts the watch for changes and does not end
/// it: the page does, when it stops the worker, so that a worker on its
/// way out cannot take the next one's watch with it.
pub fn run(
    service: Arc<dyn AudioDevices>,
    latest: Arc<Latest>,
    handle: Handle<Command>,
    mut inbox: Inbox<Command>,
    emit: impl Fn(Event) + Send + Sync + 'static,
) {
    let report = |what: &'static str, result: Result<(), Refusal>| {
        if result.is_err() {
            eprintln!("sengi-settings: could not {what}");
            emit(Event::Refused(what));
        }
    };
    service.watch(handle.notifier());
    emit(Event::Reading(service.read()));
    while let Some(next) = inbox.wait() {
        match next {
            Next::Refresh => {}
            Next::Command(Command::Default(id)) => {
                report("choose the sound device", service.set_default(id));
            }
            Next::Command(Command::Mute(id, muted)) => {
                report(
                    if muted {
                        "mute the sound device"
                    } else {
                        "unmute the sound device"
                    },
                    service.set_muted(id, muted),
                );
            }
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
                let result = service.set_volume(id, percent.min(MAX_PERCENT));
                // Taken or not: what is read next is where the volume
                // stands after this position.
                latest.carried_out(side, sequence);
                report("set the volume", result);
            }
        }
        emit(Event::Reading(service.read()));
    }
}

pub fn start(
    service: Arc<dyn AudioDevices>,
    latest: Arc<Latest>,
    emit: impl Fn(Event) + Send + Sync + 'static,
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
    use crate::worker::Notify;
    use std::sync::atomic::AtomicBool;
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
        /// Take no change, as PipeWire does when it has gone.
        pub refuse: AtomicBool,
    }

    impl Fake {
        pub fn with(audio: Audio) -> Arc<Fake> {
            Arc::new(Fake {
                audio: Mutex::new(Some(audio)),
                ..Fake::default()
            })
        }

        fn change(&self, entry: String, change: impl Fn(&mut Vec<Node>)) -> Result<(), Refusal> {
            self.log
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(entry);
            if self.refuse.load(Ordering::SeqCst) {
                return Err(Refusal::Failed);
            }
            if let Some(audio) = self
                .audio
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
            {
                change(&mut audio.outputs);
                change(&mut audio.inputs);
            }
            Ok(())
        }
    }

    impl AudioDevices for Fake {
        fn read(&self) -> Option<Audio> {
            self.audio.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }

        fn set_default(&self, id: u32) -> Result<(), Refusal> {
            self.change(format!("default {id}"), |nodes| {
                if nodes.iter().any(|node| node.id == id) {
                    for node in nodes.iter_mut() {
                        node.default = node.id == id;
                    }
                }
            })
        }

        fn set_volume(&self, id: u32, percent: u32) -> Result<(), Refusal> {
            self.change(format!("volume {id} {percent}"), |nodes| {
                for node in nodes.iter_mut().filter(|node| node.id == id) {
                    node.percent = percent;
                }
            })
        }

        fn set_muted(&self, id: u32, muted: bool) -> Result<(), Refusal> {
            self.change(format!("mute {id} {muted}"), |nodes| {
                for node in nodes.iter_mut().filter(|node| node.id == id) {
                    node.muted = muted;
                }
            })
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
    use crate::worker::Notify;
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    fn sample() -> Audio {
        Audio {
            outputs: vec![node(35, "Speakers", 40, true), node(60, "HDMI", 100, false)],
            inputs: vec![node(53, "Microphone", 80, true)],
        }
    }

    fn worked(commands: Vec<Command>, latest: Arc<Latest>) -> (Vec<String>, Vec<Option<Audio>>) {
        let (log, events) = worked_with(&Fake::with(sample()), commands, latest);
        let readings = events
            .into_iter()
            .filter_map(|event| match event {
                Event::Reading(audio) => Some(audio),
                Event::Refused(_) => None,
            })
            .collect();
        (log, readings)
    }

    fn worked_with(
        fake: &Arc<Fake>,
        commands: Vec<Command>,
        latest: Arc<Latest>,
    ) -> (Vec<String>, Vec<Event>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (handle, inbox) = worker::channel();
        for command in commands {
            handle.send(command);
        }
        handle.stop();
        run(fake.clone(), latest, handle, inbox, move |event| {
            let _ = tx.lock().unwrap().send(event);
        });
        let log = fake.log.lock().unwrap().clone();
        (log, rx.try_iter().collect())
    }

    #[test]
    fn a_change_pipewire_does_not_take_is_told_and_then_read() {
        let fake = Fake::with(sample());
        fake.refuse.store(true, Ordering::SeqCst);
        let latest = Arc::new(Latest::default());
        let sequence = latest.next(Side::Output);
        let (log, events) = worked_with(
            &fake,
            vec![
                Command::Mute(35, true),
                Command::Volume {
                    side: Side::Output,
                    id: 35,
                    percent: 70,
                    sequence,
                },
            ],
            latest.clone(),
        );
        assert_eq!(log, ["mute 35 true", "volume 35 70"]);
        let still = Event::Reading(Some(sample()));
        assert_eq!(
            events,
            [
                still.clone(),
                Event::Refused("mute the sound device"),
                still.clone(),
                Event::Refused("set the volume"),
                still,
            ]
        );
        // The slider goes back to what is true: the position was dealt
        // with, though not taken.
        assert!(latest.is_current(Side::Output, latest.stamp()));
    }

    #[test]
    fn a_reading_made_before_the_newest_position_does_not_move_the_slider() {
        let latest = Latest::default();
        let nothing_yet = latest.stamp();
        assert!(latest.is_current(Side::Output, nothing_yet));
        let first = latest.next(Side::Output);
        // Asked for and not carried out yet.
        assert!(!latest.is_current(Side::Output, latest.stamp()));
        // The other slider is not held up by this one.
        assert!(latest.is_current(Side::Input, latest.stamp()));
        latest.carried_out(Side::Output, first);
        let after_first = latest.stamp();
        assert!(latest.is_current(Side::Output, after_first));
        // The slider moves on while that reading is on its way.
        let second = latest.next(Side::Output);
        assert!(!latest.is_current(Side::Output, after_first));
        assert!(!latest.is_current(Side::Output, nothing_yet));
        latest.carried_out(Side::Output, second);
        assert!(latest.is_current(Side::Output, latest.stamp()));
        // A reading from before is still from before.
        assert!(!latest.is_current(Side::Output, after_first));
        // A worker stopped with positions unsent leaves nothing owed.
        latest.next(Side::Input);
        latest.settle();
        assert!(latest.is_current(Side::Input, latest.stamp()));
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
        assert_eq!(log, ["default 60", "mute 53 true", "volume 60 100"]);
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
        assert_eq!(log, ["volume 35 60", "volume 53 10"]);
    }

    /// A PipeWire whose monitor reports what really happens when it is
    /// read: the reader, `wpctl`, comes and goes as a client.
    #[derive(Default)]
    struct Echoing {
        reads: Mutex<u32>,
        monitor: Mutex<Option<(sengi_system::audio::Changes, Notify)>>,
    }

    impl Echoing {
        fn reads(&self) -> u32 {
            *self.reads.lock().unwrap()
        }

        fn monitor_prints(&self, text: &str) {
            if let Some((changes, changed)) = self.monitor.lock().unwrap().as_mut() {
                if changes.feed(text.as_bytes()) {
                    changed();
                }
            }
        }
    }

    impl AudioDevices for Echoing {
        fn read(&self) -> Option<Audio> {
            let client = {
                let mut reads = self.reads.lock().unwrap();
                *reads += 1;
                100 + *reads
            };
            self.monitor_prints(&format!(
                r#"[ {{ "id": {client}, "type": "PipeWire:Interface:Client", "info": {{ "props": {{ "application.name": "wpctl" }} }} }} ]"#
            ));
            self.monitor_prints(&format!(r#"[ {{ "id": {client}, "info": null }} ]"#));
            Some(sample())
        }

        fn set_default(&self, _id: u32) -> Result<(), Refusal> {
            Ok(())
        }

        fn set_volume(&self, _id: u32, _percent: u32) -> Result<(), Refusal> {
            Ok(())
        }

        fn set_muted(&self, _id: u32, _muted: bool) -> Result<(), Refusal> {
            Ok(())
        }

        fn watch(&self, changed: Notify) {
            *self.monitor.lock().unwrap() = Some((Default::default(), changed));
        }

        fn close(&self) {
            *self.monitor.lock().unwrap() = None;
        }
    }

    #[test]
    fn the_sound_worker_goes_quiet_after_a_read_when_the_monitor_reports_only_the_readers_own_client(
    ) {
        let service = Arc::new(Echoing::default());
        let (handle, inbox) = worker::channel();
        let thread = std::thread::spawn({
            let (service, handle) = (service.clone(), handle.clone());
            move || run(service, Arc::default(), handle, inbox, |_| {})
        });
        let settle = worker::PATIENCE + Duration::from_millis(200);
        std::thread::sleep(settle);
        assert_eq!(service.reads(), 1, "the first read set off another");

        // A real change is still heard, once.
        service.monitor_prints(
            r#"[ { "id": 52, "type": "PipeWire:Interface:Node", "info": { "params": {} } } ]"#,
        );
        std::thread::sleep(settle);
        assert_eq!(service.reads(), 2);
        std::thread::sleep(settle);
        assert_eq!(service.reads(), 2, "the worker is feeding itself");

        handle.stop();
        thread.join().unwrap();
    }

    #[test]
    fn a_stopped_worker_reads_no_more_whatever_the_monitor_says() {
        let service = Arc::new(Echoing::default());
        let (handle, inbox) = worker::channel();
        let thread = std::thread::spawn({
            let (service, handle) = (service.clone(), handle.clone());
            move || run(service, Arc::default(), handle, inbox, |_| {})
        });
        std::thread::sleep(Duration::from_millis(100));
        // The page is hidden: it stops the worker and closes the watch.
        handle.stop();
        thread.join().unwrap();
        let before = service.reads();
        // A monitor that was slow to die still has something to say.
        service
            .monitor_prints(r#"[ { "id": 52, "type": "PipeWire:Interface:Node", "info": {} } ]"#);
        service.close();
        assert!(service.monitor.lock().unwrap().is_none());
        std::thread::sleep(worker::PATIENCE);
        assert_eq!(service.reads(), before);
    }

    #[test]
    fn pipewire_not_being_there_is_a_reading_of_nothing() {
        let fake = Arc::new(Fake::default());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (handle, inbox) = worker::channel();
        handle.stop();
        run(fake, Arc::default(), handle, inbox, move |event| {
            let _ = tx.lock().unwrap().send(event);
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)),
            Ok(Event::Reading(None))
        );
    }
}
