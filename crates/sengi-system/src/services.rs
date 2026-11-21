//! The services behind the tiles, as traits, and the worker threads that
//! sit between them and the panel.
//!
//! Every method of a service may block: on the system bus, on a child
//! process, on a file. None is ever called on the UI thread. Each service
//! has a worker that takes [`Command`]s from the panel, calls the service,
//! and reports what it then reads as [`Event`]s. The real services come
//! from [`real_services`](crate::real_services); a program can supply
//! made-up ones of its own, as quick settings does for looking at its panel.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::backlight::Backlight;
use crate::battery::Battery;
use crate::bluetooth::Bluetooth;
use crate::volume::{self, Step, Volume};
use crate::wifi::{JoinError, Request, Wifi};

/// Called by a service, from any thread, when a later read could differ.
pub type Notify = Box<dyn Fn() + Send + Sync>;

/// A request a service turned down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The bus policy, or polkit behind the service, does not allow it.
    Denied,
    /// It could not be done, or there was nobody to ask.
    Failed,
}

impl Refusal {
    /// What a failed call on the bus comes to.
    pub fn of(error: &zbus::Error) -> Refusal {
        if crate::bus::denied(error) {
            Refusal::Denied
        } else {
            Refusal::Failed
        }
    }
}

/// Wi-Fi: reading its state, switching the radio, scanning and joining a
/// network.
pub trait WifiService: Send + Sync + 'static {
    /// Wi-Fi as it is now. Never starts the daemon: one that is not running
    /// reads as absent.
    fn read(&self) -> Wifi;
    /// Switch the radio on or off. The change shows in a later `read`.
    fn set_powered(&self, on: bool);
    /// As `set_powered`, for a caller that can show why it did not work.
    fn try_set_powered(&self, on: bool) -> Result<(), Refusal> {
        self.set_powered(on);
        Ok(())
    }
    /// Ask for a fresh look at what is in range. Returns at once.
    fn scan(&self);
    /// Returns when the network is joined or the attempt has failed, which
    /// can take half a minute. `read` and the rest stay usable meanwhile.
    fn join(&self, request: Request) -> Result<(), JoinError>;
    /// Leave the connected network, or call off a join under way. A
    /// service that only joins refuses.
    fn disconnect(&self) -> Result<(), Refusal> {
        Err(Refusal::Failed)
    }
    /// Have the service drop what it stored about the network `id`. A
    /// service that only joins refuses.
    fn forget(&self, _id: &str) -> Result<(), Refusal> {
        Err(Refusal::Failed)
    }
    /// The kernel's name for the device the last `read` was about, such
    /// as `wlan0`, if the service knows it.
    fn interface(&self) -> Option<String> {
        None
    }
    /// Arrange for `changed` to be called whenever a later `read` could
    /// differ.
    fn watch(&self, changed: Notify);
    /// Stop every watch started with `watch`, and what it was using. A
    /// program that exits when its panel closes has no need to.
    fn unwatch(&self) {}
}

/// Bluetooth: whether there is an adapter, whether it is on, and what is
/// connected.
pub trait BluetoothService: Send + Sync + 'static {
    /// Bluetooth as it is now. Never starts the daemon: one that is not
    /// running reads as absent.
    fn read(&self) -> Bluetooth;
    /// Switch the adapter on or off. The change shows in a later `read`.
    fn set_powered(&self, on: bool);
    /// As `set_powered`, for a caller that can show why it did not work.
    fn try_set_powered(&self, on: bool) -> Result<(), Refusal> {
        self.set_powered(on);
        Ok(())
    }
    /// Arrange for `changed` to be called whenever a later `read` could
    /// differ.
    fn watch(&self, changed: Notify);
    /// Stop every watch started with `watch`, and what it was using. A
    /// program that exits when its panel closes has no need to.
    fn unwatch(&self) {}
}

/// The default audio output.
pub trait AudioService: Send + 'static {
    /// `None` when there is no output to control.
    fn read(&self) -> Option<Volume>;
    /// Set the volume, in percent. It is held at 100.
    fn set_volume(&self, percent: u32);
    /// Mute or unmute the output.
    fn set_muted(&self, muted: bool);
    /// As `set_volume`, for a caller that can show that it did not work.
    fn try_set_volume(&self, percent: u32) -> Result<(), Refusal> {
        self.set_volume(percent);
        Ok(())
    }
    /// As `set_muted`, for a caller that can show that it did not work.
    fn try_set_muted(&self, muted: bool) -> Result<(), Refusal> {
        self.set_muted(muted);
        Ok(())
    }
}

/// What the kernel says about the machine itself.
pub trait Machine: Send + 'static {
    /// The battery, backlight, links and radio blocks as they are now.
    fn read(&self) -> Hardware;
    /// Set the backlight called `name` to `raw`, in the device's own units:
    /// see [`Backlight::raw`].
    fn set_backlight(&self, name: &str, raw: u32);
    /// As `set_backlight`, for a caller that can show why it did not work.
    fn try_set_backlight(&self, name: &str, raw: u32) -> Result<(), Refusal> {
        self.set_backlight(name, raw);
        Ok(())
    }
}

/// Do-not-disturb, which the notification service owns.
pub trait DndService: Send + 'static {
    /// Whether the notification service is installed at all.
    fn available(&self) -> bool;
    /// `None` when the service cannot say.
    fn read(&self) -> Option<bool>;
    /// Switch do-not-disturb on or off.
    fn set(&self, on: bool);
    /// As `set`, for a caller that can show that it did not work.
    fn try_set(&self, on: bool) -> Result<(), Refusal> {
        self.set(on);
        Ok(())
    }
}

/// Whether the kernel's rfkill is holding a radio off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Block {
    /// Not held off.
    #[default]
    None,
    /// By software: a key on the keyboard, or `rfkill block`.
    Soft,
    /// By a physical switch. Nothing on the screen can undo it.
    Hard,
}

/// What [`Machine::read`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hardware {
    /// The first system battery, if the machine has one.
    pub battery: Option<Battery>,
    /// The backlight to control, if the machine has one.
    pub backlight: Option<Backlight>,
    /// A cable is in and the link is up.
    pub wired: bool,
    /// Some other kind of hardware link is up: a phone's modem, say.
    pub other_link: bool,
    /// There is Wi-Fi hardware, connected or not.
    pub wifi_device: bool,
    /// There is Bluetooth hardware.
    pub bluetooth_device: bool,
    /// Whether rfkill is holding Wi-Fi off.
    pub wifi_block: Block,
    /// Whether rfkill is holding Bluetooth off.
    pub bluetooth_block: Block,
}

/// One of each service: the real ones from
/// [`real_services`](crate::real_services), or a program's own fakes.
pub struct Services {
    /// Wi-Fi.
    pub wifi: Arc<dyn WifiService>,
    /// Bluetooth.
    pub bluetooth: Arc<dyn BluetoothService>,
    /// The default audio output.
    pub audio: Box<dyn AudioService>,
    /// The machine itself: `/sys`, and logind for the backlight.
    pub machine: Box<dyn Machine>,
    /// Do-not-disturb.
    pub dnd: Box<dyn DndService>,
}

/// News for the panel.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// Wi-Fi as it now reads.
    Wifi(Wifi),
    /// A [`Command::Join`] has ended.
    Joined {
        /// The network's ID, as it was in the request.
        id: String,
        /// How the attempt ended.
        result: Result<(), JoinError>,
    },
    /// Bluetooth as it now reads.
    Bluetooth(Bluetooth),
    /// The volume as read after applying slider change `sequence`.
    Volume {
        /// The number of the change this reading follows.
        sequence: u64,
        /// `None` when there is no output to control.
        volume: Option<Volume>,
    },
    /// Likewise for the brightness slider.
    Hardware {
        /// The number of the change this reading follows.
        sequence: u64,
        /// The machine as it now reads.
        hardware: Hardware,
    },
    /// Do-not-disturb as it now reads. `None` when the service cannot say.
    Dnd(Option<bool>),
}

/// What the panel asks for.
#[derive(Debug)]
#[non_exhaustive]
pub enum Command {
    /// Switch the Wi-Fi radio on or off.
    WifiPower(bool),
    /// Join a network. Answered with [`Event::Joined`].
    Join(Request),
    /// Switch the Bluetooth adapter on or off.
    BluetoothPower(bool),
    /// Set the volume to the step's value, in percent.
    Volume(Step),
    /// Numbered in the same series as the volume changes.
    Mute {
        /// This change's number.
        sequence: u64,
        /// Whether to mute.
        muted: bool,
    },
    /// Set the brightness to the step's value, in percent.
    Brightness(Step),
    /// Switch do-not-disturb on or off.
    Dnd(bool),
}

/// Delivers an event to the panel. False once the panel has gone, which
/// tells a worker to stop.
pub trait Emit: Fn(Event) -> bool + Send + Sync + 'static {}
impl<F: Fn(Event) -> bool + Send + Sync + 'static> Emit for F {}

/// The bus sends changes in bursts: a scan result is dozens of signals.
/// A worker waits for this much quiet before reading again, and for no
/// longer than the limit in all.
const SETTLE: Duration = Duration::from_millis(30);
const SETTLE_LIMIT: Duration = Duration::from_millis(250);

/// How often volume, battery and backlight are looked at while the panel
/// is open. Nothing announces changes to them.
pub const POLL: Duration = Duration::from_secs(2);

/// What the Wi-Fi worker takes in.
#[derive(Debug)]
enum WifiMessage {
    /// The service says a read could now differ.
    Changed,
    /// Switch the radio on or off.
    Power(bool),
    /// Join a network.
    Join(Request),
}

/// What the Bluetooth worker takes in.
#[derive(Debug)]
enum BluetoothMessage {
    /// The service says a read could now differ.
    Changed,
    /// Switch the adapter on or off.
    Power(bool),
}

/// What the audio worker takes in.
#[derive(Debug)]
enum AudioMessage {
    /// Set the volume.
    Volume(Step),
    /// Mute or unmute.
    Mute {
        /// This change's number.
        sequence: u64,
        /// Whether to mute.
        muted: bool,
    },
    /// The panel is closing: apply what is queued and end.
    Stop,
}

/// What the machine worker takes in.
#[derive(Debug)]
enum MachineMessage {
    /// Set the brightness.
    Brightness(Step),
    /// The panel is closing: apply what is queued and end.
    Stop,
}

/// The running workers.
pub struct Workers {
    wifi: Sender<WifiMessage>,
    bluetooth: Sender<BluetoothMessage>,
    audio: Sender<AudioMessage>,
    machine: Sender<MachineMessage>,
    dnd: Sender<bool>,
    /// The audio and machine workers each say here when they have ended.
    ended: Receiver<()>,
}

impl Workers {
    /// Start a thread for each service. Everything they learn arrives
    /// through `emit`, on those threads.
    pub fn start(services: Services, emit: impl Emit + Clone) -> Workers {
        let (wifi, inbox) = mpsc::channel();
        thread::spawn({
            let (service, own, emit) = (services.wifi, wifi.clone(), emit.clone());
            move || wifi_worker(service, inbox, own, emit)
        });
        let (bluetooth, inbox) = mpsc::channel();
        thread::spawn({
            let (service, own, emit) = (services.bluetooth, bluetooth.clone(), emit.clone());
            move || bluetooth_worker(service, inbox, own, emit)
        });
        let (end, ended) = mpsc::channel();
        let (audio, inbox) = mpsc::channel();
        thread::spawn({
            let (service, emit, end) = (services.audio, emit.clone(), end.clone());
            move || {
                audio_worker(service.as_ref(), inbox, emit, POLL);
                let _ = end.send(());
            }
        });
        let (machine, inbox) = mpsc::channel();
        thread::spawn({
            let (service, emit) = (services.machine, emit.clone());
            move || {
                machine_worker(service.as_ref(), inbox, emit, POLL);
                let _ = end.send(());
            }
        });
        let (dnd, inbox) = mpsc::channel();
        thread::spawn({
            let service = services.dnd;
            move || dnd_worker(service.as_ref(), inbox, emit)
        });
        Workers {
            wifi,
            bluetooth,
            audio,
            machine,
            dnd,
            ended,
        }
    }

    /// The panel has closed. Give the last move of a slider the time to
    /// be carried out, up to `bound`: the process is about to exit, and a
    /// worker that was cut off mid-way would leave the volume or the
    /// brightness one step short of where the knob was let go.
    pub fn finish(&self, bound: Duration) {
        let _ = self.audio.send(AudioMessage::Stop);
        let _ = self.machine.send(MachineMessage::Stop);
        let deadline = Instant::now() + bound;
        for _ in 0..2 {
            let left = deadline.saturating_duration_since(Instant::now());
            if self.ended.recv_timeout(left).is_err() {
                return;
            }
        }
    }

    /// Never blocks.
    pub fn send(&self, command: Command) {
        // A worker that has stopped has nobody left to tell.
        let _ = match command {
            Command::WifiPower(on) => self.wifi.send(WifiMessage::Power(on)).is_ok(),
            Command::Join(request) => self.wifi.send(WifiMessage::Join(request)).is_ok(),
            Command::BluetoothPower(on) => self.bluetooth.send(BluetoothMessage::Power(on)).is_ok(),
            Command::Volume(step) => self.audio.send(AudioMessage::Volume(step)).is_ok(),
            Command::Mute { sequence, muted } => self
                .audio
                .send(AudioMessage::Mute { sequence, muted })
                .is_ok(),
            Command::Brightness(step) => {
                self.machine.send(MachineMessage::Brightness(step)).is_ok()
            }
            Command::Dnd(on) => self.dnd.send(on).is_ok(),
        };
    }
}

/// Wait for a message, then for the burst it belongs to to end.
fn burst<T>(inbox: &Receiver<T>) -> Option<Vec<T>> {
    let mut messages = vec![inbox.recv().ok()?];
    let limit = Instant::now() + SETTLE_LIMIT;
    while let Some(left) = limit.checked_duration_since(Instant::now()) {
        match inbox.recv_timeout(SETTLE.min(left)) {
            Ok(message) => messages.push(message),
            Err(_) => break,
        }
    }
    Some(messages)
}

/// The Wi-Fi worker's loop, run by [`Workers::start`].
fn wifi_worker(
    service: Arc<dyn WifiService>,
    inbox: Receiver<WifiMessage>,
    own: Sender<WifiMessage>,
    emit: impl Emit + Clone,
) {
    service.watch(Box::new({
        let own = own.clone();
        move || {
            let _ = own.send(WifiMessage::Changed);
        }
    }));
    let mut last = None;
    let mut scanned = false;
    loop {
        let wifi = service.read();
        match wifi {
            // One scan each time Wi-Fi is found switched on, so the list
            // is fresh without scanning for as long as the panel is open.
            Wifi::On {
                scanning: false, ..
            } if !scanned => {
                scanned = true;
                service.scan();
            }
            Wifi::On { .. } => {}
            _ => scanned = false,
        }
        if last.as_ref() != Some(&wifi) {
            if !emit(Event::Wifi(wifi.clone())) {
                return;
            }
            last = Some(wifi);
        }

        let Some(messages) = burst(&inbox) else {
            return;
        };
        for message in messages {
            match message {
                WifiMessage::Changed => {}
                WifiMessage::Power(on) => {
                    service.set_powered(on);
                    // Report back even if nothing changed, so a switch
                    // that could not be thrown springs back.
                    last = None;
                }
                WifiMessage::Join(request) => {
                    let (service, own, emit) = (service.clone(), own.clone(), emit.clone());
                    thread::spawn(move || {
                        let id = request.id.clone();
                        let result = service.join(request);
                        emit(Event::Joined { id, result });
                        let _ = own.send(WifiMessage::Changed);
                    });
                }
            }
        }
    }
}

/// The Bluetooth worker's loop, run by [`Workers::start`].
fn bluetooth_worker(
    service: Arc<dyn BluetoothService>,
    inbox: Receiver<BluetoothMessage>,
    own: Sender<BluetoothMessage>,
    emit: impl Emit,
) {
    service.watch(Box::new(move || {
        let _ = own.send(BluetoothMessage::Changed);
    }));
    let mut last = None;
    loop {
        let bluetooth = service.read();
        if last.as_ref() != Some(&bluetooth) {
            if !emit(Event::Bluetooth(bluetooth.clone())) {
                return;
            }
            last = Some(bluetooth);
        }
        let Some(messages) = burst(&inbox) else {
            return;
        };
        // Only the last position of the switch matters.
        let power = messages.iter().rev().find_map(|message| match message {
            BluetoothMessage::Power(on) => Some(*on),
            BluetoothMessage::Changed => None,
        });
        if let Some(on) = power {
            service.set_powered(on);
            last = None;
        }
    }
}

/// What a batch of slider and mute messages comes to: the newest volume
/// and the last mute state. Everything in between was overtaken.
fn settle(batch: impl IntoIterator<Item = AudioMessage>) -> Settled {
    let mut steps = Vec::new();
    let mut settled = Settled::default();
    for message in batch {
        match message {
            AudioMessage::Volume(step) => steps.push(step),
            AudioMessage::Mute { sequence, muted } => {
                settled.mute = Some(muted);
                settled.sequence = settled.sequence.max(sequence);
            }
            AudioMessage::Stop => settled.stop = true,
        }
    }
    settled.volume = volume::newest(steps);
    if let Some(step) = settled.volume {
        settled.sequence = settled.sequence.max(step.sequence);
    }
    settled
}

/// What [`settle`] makes of a batch.
#[derive(Debug, Default, PartialEq, Eq)]
struct Settled {
    /// The newest volume asked for, if any was.
    pub volume: Option<Step>,
    /// The last mute state asked for, if any was.
    pub mute: Option<bool>,
    /// The newest change in the batch, of either kind; 0 for none.
    pub sequence: u64,
    /// The batch held the order to stop. What came with it still counts.
    pub stop: bool,
}

/// Everything waiting in `inbox`, or nothing after `poll`. `Err` when the
/// panel has gone.
fn waiting<T>(inbox: &Receiver<T>, poll: Duration) -> Result<Vec<T>, ()> {
    match inbox.recv_timeout(poll) {
        Ok(first) => {
            let mut batch = vec![first];
            batch.extend(inbox.try_iter());
            Ok(batch)
        }
        Err(RecvTimeoutError::Timeout) => Ok(Vec::new()),
        Err(RecvTimeoutError::Disconnected) => Err(()),
    }
}

/// The audio worker's loop, run by [`Workers::start`].
fn audio_worker(
    service: &dyn AudioService,
    inbox: Receiver<AudioMessage>,
    emit: impl Emit,
    poll: Duration,
) {
    let mut applied = 0;
    let mut last = None;
    loop {
        let reading = (applied, service.read());
        let mut listening = true;
        if last != Some(reading) {
            let (sequence, volume) = reading;
            listening = emit(Event::Volume { sequence, volume });
            last = Some(reading);
        }
        // While a change is being applied the slider keeps moving; what
        // piles up here is boiled down to one more change. With the panel
        // gone there is nothing more to wait for, but what it asked for
        // last is still owed.
        let batch = if listening {
            match waiting(&inbox, poll) {
                Ok(batch) => batch,
                Err(()) => return,
            }
        } else {
            inbox.try_iter().collect()
        };
        let settled = settle(batch);
        if let Some(muted) = settled.mute {
            service.set_muted(muted);
        }
        if let Some(step) = settled.volume {
            service.set_volume(step.value);
        }
        // A reading taken from here on has seen all of these.
        applied = applied.max(settled.sequence);
        if settled.stop || !listening {
            return;
        }
    }
}

/// The machine worker's loop, run by [`Workers::start`].
fn machine_worker(
    service: &dyn Machine,
    inbox: Receiver<MachineMessage>,
    emit: impl Emit,
    poll: Duration,
) {
    let mut applied = 0;
    let mut last = None;
    loop {
        let hardware = service.read();
        let mut listening = true;
        if last.as_ref() != Some(&(applied, hardware.clone())) {
            listening = emit(Event::Hardware {
                sequence: applied,
                hardware: hardware.clone(),
            });
            last = Some((applied, hardware.clone()));
        }
        // As for the volume: the last position of the slider is carried
        // out even when the panel has already gone.
        let batch = if listening {
            match waiting(&inbox, poll) {
                Ok(batch) => batch,
                Err(()) => return,
            }
        } else {
            inbox.try_iter().collect()
        };
        let mut stop = !listening;
        let steps = batch.into_iter().filter_map(|message| match message {
            MachineMessage::Brightness(step) => Some(step),
            MachineMessage::Stop => {
                stop = true;
                None
            }
        });
        if let Some(step) = volume::newest(steps.collect::<Vec<_>>()) {
            if let Some(backlight) = &hardware.backlight {
                service.set_backlight(&backlight.name, backlight.raw(step.value));
            }
            applied = step.sequence;
        }
        if stop {
            return;
        }
    }
}

/// The do-not-disturb worker's loop, run by [`Workers::start`].
fn dnd_worker(service: &dyn DndService, inbox: Receiver<bool>, emit: impl Emit) {
    if !service.available() {
        return;
    }
    if !emit(Event::Dnd(service.read())) {
        return;
    }
    while let Ok(first) = inbox.recv() {
        let on = inbox.try_iter().last().unwrap_or(first);
        service.set(on);
        if !emit(Event::Dnd(service.read())) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::Link;
    use std::sync::Mutex;

    fn step(sequence: u64, value: u32) -> Step {
        Step { sequence, value }
    }

    fn mute(sequence: u64, muted: bool) -> AudioMessage {
        AudioMessage::Mute { sequence, muted }
    }

    /// Events land in a channel the test reads.
    fn collector() -> (impl Emit + Clone, Receiver<Event>) {
        let (sender, events) = mpsc::channel();
        (move |event| sender.send(event).is_ok(), events)
    }

    fn next(events: &Receiver<Event>) -> Event {
        events
            .recv_timeout(Duration::from_secs(10))
            .expect("an event")
    }

    #[test]
    fn a_batch_settles_to_the_newest_volume_and_last_mute() {
        use AudioMessage::*;
        let batch = vec![
            Volume(step(1, 30)),
            mute(2, true),
            Volume(step(3, 35)),
            Volume(step(4, 41)),
            mute(5, false),
        ];
        let settled = settle(batch);
        assert_eq!(settled.volume, Some(step(4, 41)));
        assert_eq!(settled.mute, Some(false));
        assert_eq!(settled.sequence, 5);
        assert!(!settled.stop);
        assert_eq!(settle(vec![mute(9, true)]).mute, Some(true));
        assert_eq!(settle(Vec::new()), Settled::default());
        // Stopping does not discard what was asked for just before.
        let last = settle(vec![Volume(step(6, 70)), Stop]);
        assert_eq!((last.volume, last.stop), (Some(step(6, 70)), true));
        assert_eq!(last.sequence, 6);
    }

    #[derive(Default)]
    struct FakeAudio {
        state: Mutex<(u32, bool)>,
        calls: Mutex<Vec<String>>,
        /// How long a change takes, to let the slider get ahead.
        delay: Duration,
    }

    impl AudioService for Arc<FakeAudio> {
        fn read(&self) -> Option<Volume> {
            let (percent, muted) = *self.state.lock().unwrap();
            Some(Volume { percent, muted })
        }
        fn set_volume(&self, percent: u32) {
            thread::sleep(self.delay);
            self.state.lock().unwrap().0 = percent;
            self.calls.lock().unwrap().push(format!("volume {percent}"));
        }
        fn set_muted(&self, muted: bool) {
            self.state.lock().unwrap().1 = muted;
            self.calls.lock().unwrap().push(format!("mute {muted}"));
        }
    }

    #[test]
    fn a_fast_drag_does_not_run_a_command_per_position() {
        let audio = Arc::new(FakeAudio {
            state: Mutex::new((40, false)),
            delay: Duration::from_millis(40),
            ..FakeAudio::default()
        });
        let (emit, events) = collector();
        let (sender, inbox) = mpsc::channel();
        let worker = thread::spawn({
            let audio = audio.clone();
            move || audio_worker(&audio, inbox, emit, Duration::from_secs(60))
        });
        assert_eq!(
            next(&events),
            Event::Volume {
                sequence: 0,
                volume: Some(Volume {
                    percent: 40,
                    muted: false
                })
            }
        );

        // A hundred positions arrive faster than one can be applied.
        for i in 1..=100 {
            sender
                .send(AudioMessage::Volume(step(i, 40 + i as u32 / 2)))
                .unwrap();
        }
        // The last reading is for the last position, whatever came before.
        let last = loop {
            match next(&events) {
                Event::Volume {
                    sequence: 100,
                    volume,
                } => break volume,
                Event::Volume { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        };
        assert_eq!(last.unwrap().percent, 90);
        let calls = audio.calls.lock().unwrap().len();
        assert!(calls <= 5, "{calls} commands for one drag");

        drop(sender);
        worker.join().unwrap();
    }

    #[test]
    fn the_last_move_of_the_slider_is_carried_out_on_the_way_out() {
        let audio = Arc::new(FakeAudio {
            state: Mutex::new((40, false)),
            delay: Duration::from_millis(30),
            ..FakeAudio::default()
        });
        let (sender, inbox) = mpsc::channel();
        // The panel closes straight after a drag: its last positions and
        // the order to stop are all waiting when the worker next looks.
        for i in 1..=20 {
            sender
                .send(AudioMessage::Volume(step(i, 40 + i as u32)))
                .unwrap();
        }
        sender.send(mute(21, true)).unwrap();
        sender.send(AudioMessage::Stop).unwrap();
        // And the panel is no longer listening for events.
        audio_worker(&audio, inbox, |_| false, Duration::from_secs(60));
        assert_eq!(*audio.state.lock().unwrap(), (60, true));
    }

    #[test]
    fn the_last_brightness_is_carried_out_on_the_way_out() {
        let machine = Arc::new(FakeMachine {
            hardware: Mutex::new(Hardware {
                backlight: Some(Backlight {
                    name: "panel".to_string(),
                    brightness: 500,
                    max: 1000,
                }),
                ..Hardware::default()
            }),
        });
        let (sender, inbox) = mpsc::channel();
        sender
            .send(MachineMessage::Brightness(step(1, 70)))
            .unwrap();
        sender
            .send(MachineMessage::Brightness(step(2, 80)))
            .unwrap();
        sender.send(MachineMessage::Stop).unwrap();
        machine_worker(&machine, inbox, |_| true, Duration::from_secs(60));
        let hardware = machine.hardware.lock().unwrap();
        assert_eq!(hardware.backlight.as_ref().unwrap().brightness, 800);
    }

    #[test]
    fn the_worker_stops_when_the_panel_is_gone() {
        let audio = Arc::new(FakeAudio::default());
        let (_sender, inbox) = mpsc::channel();
        // Nobody is listening for events.
        audio_worker(&audio, inbox, |_| false, Duration::from_secs(60));
    }

    #[test]
    fn volume_changed_elsewhere_is_noticed() {
        let audio = Arc::new(FakeAudio::default());
        let (emit, events) = collector();
        let (sender, inbox) = mpsc::channel::<AudioMessage>();
        thread::spawn({
            let audio = audio.clone();
            move || audio_worker(&audio, inbox, emit, Duration::from_millis(10))
        });
        next(&events);
        *audio.state.lock().unwrap() = (75, true);
        assert_eq!(
            next(&events),
            Event::Volume {
                sequence: 0,
                volume: Some(Volume {
                    percent: 75,
                    muted: true
                })
            }
        );
        drop(sender);
    }

    struct FakeMachine {
        hardware: Mutex<Hardware>,
    }

    impl Machine for Arc<FakeMachine> {
        fn read(&self) -> Hardware {
            self.hardware.lock().unwrap().clone()
        }
        fn set_backlight(&self, name: &str, raw: u32) {
            let mut hardware = self.hardware.lock().unwrap();
            let backlight = hardware.backlight.as_mut().unwrap();
            assert_eq!(backlight.name, name);
            backlight.brightness = raw;
        }
    }

    #[test]
    fn brightness_goes_through_the_floor_check() {
        let machine = Arc::new(FakeMachine {
            hardware: Mutex::new(Hardware {
                backlight: Some(Backlight {
                    name: "panel".to_string(),
                    brightness: 500,
                    max: 1000,
                }),
                ..Hardware::default()
            }),
        });
        let (emit, events) = collector();
        let (sender, inbox) = mpsc::channel();
        thread::spawn({
            let machine = machine.clone();
            move || machine_worker(&machine, inbox, emit, Duration::from_secs(60))
        });
        next(&events);
        // The slider cannot go this low, but the worker does not rely on it.
        sender.send(MachineMessage::Brightness(step(1, 0))).unwrap();
        match next(&events) {
            Event::Hardware {
                sequence: 1,
                hardware,
            } => {
                assert_eq!(hardware.backlight.unwrap().brightness, 50);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[derive(Default)]
    struct FakeWifi {
        state: Mutex<Option<Wifi>>,
        calls: Mutex<Vec<String>>,
        notify: Mutex<Option<Notify>>,
    }

    impl FakeWifi {
        fn set(&self, wifi: Wifi) {
            *self.state.lock().unwrap() = Some(wifi);
            if let Some(notify) = self.notify.lock().unwrap().as_ref() {
                notify();
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn on(scanning: bool) -> Wifi {
        Wifi::On {
            link: Link::Disconnected,
            scanning,
            networks: Vec::new(),
        }
    }

    impl WifiService for FakeWifi {
        fn read(&self) -> Wifi {
            self.state.lock().unwrap().clone().unwrap_or(Wifi::Absent)
        }
        fn set_powered(&self, on: bool) {
            self.calls.lock().unwrap().push(format!("power {on}"));
        }
        fn scan(&self) {
            self.calls.lock().unwrap().push("scan".to_string());
        }
        fn join(&self, request: Request) -> Result<(), JoinError> {
            self.calls.lock().unwrap().push(format!(
                "join {} {}",
                request.id,
                request.passphrase.map_or(0, |secret| secret.len())
            ));
            Err(JoinError::Failed)
        }
        fn watch(&self, changed: Notify) {
            *self.notify.lock().unwrap() = Some(changed);
        }
    }

    fn start_wifi(service: &Arc<FakeWifi>) -> (Sender<WifiMessage>, Receiver<Event>) {
        let (emit, events) = collector();
        let (sender, inbox) = mpsc::channel();
        thread::spawn({
            let (service, own) = (service.clone(), sender.clone());
            move || wifi_worker(service, inbox, own, emit)
        });
        (sender, events)
    }

    #[test]
    fn wifi_reports_first_then_follows_changes() {
        let service = Arc::new(FakeWifi::default());
        let (_sender, events) = start_wifi(&service);
        assert_eq!(next(&events), Event::Wifi(Wifi::Absent));
        // The service appears and says so.
        service.set(Wifi::Off);
        assert_eq!(next(&events), Event::Wifi(Wifi::Off));
        assert!(service.calls().is_empty());
    }

    #[test]
    fn wifi_scans_once_when_found_on() {
        let service = Arc::new(FakeWifi::default());
        service.set(on(false));
        let (_sender, events) = start_wifi(&service);
        assert_eq!(next(&events), Event::Wifi(on(false)));
        service.set(on(true));
        assert_eq!(next(&events), Event::Wifi(on(true)));
        service.set(on(false));
        assert_eq!(next(&events), Event::Wifi(on(false)));
        assert_eq!(service.calls(), ["scan"]);
    }

    #[test]
    fn a_switch_that_does_not_take_is_reported_again() {
        let service = Arc::new(FakeWifi::default());
        service.set(Wifi::Off);
        let (sender, events) = start_wifi(&service);
        assert_eq!(next(&events), Event::Wifi(Wifi::Off));
        // The fake ignores the request, as a blocked radio would.
        sender.send(WifiMessage::Power(true)).ok().unwrap();
        assert_eq!(next(&events), Event::Wifi(Wifi::Off));
        assert_eq!(service.calls(), ["power true"]);
    }

    #[test]
    fn a_join_reports_its_result_without_holding_up_the_worker() {
        let service = Arc::new(FakeWifi::default());
        service.set(Wifi::Off);
        let (sender, events) = start_wifi(&service);
        next(&events);
        let request = Request {
            id: "/net/7".to_string(),
            passphrase: Some(crate::secret::Secret::new("eight chars".to_string())),
        };
        sender.send(WifiMessage::Join(request)).ok().unwrap();
        assert_eq!(
            next(&events),
            Event::Joined {
                id: "/net/7".to_string(),
                result: Err(JoinError::Failed)
            }
        );
        assert_eq!(service.calls(), ["join /net/7 11"]);
    }

    struct FakeDnd {
        installed: bool,
        answers: bool,
        state: Mutex<bool>,
    }

    impl DndService for Arc<FakeDnd> {
        fn available(&self) -> bool {
            self.installed
        }
        fn read(&self) -> Option<bool> {
            self.answers.then(|| *self.state.lock().unwrap())
        }
        fn set(&self, on: bool) {
            *self.state.lock().unwrap() = on;
        }
    }

    #[test]
    fn dnd_is_set_even_when_it_cannot_be_asked() {
        let dnd = Arc::new(FakeDnd {
            installed: true,
            answers: false,
            state: Mutex::new(false),
        });
        let (emit, events) = collector();
        let (sender, inbox) = mpsc::channel();
        thread::spawn({
            let dnd = dnd.clone();
            move || dnd_worker(&dnd, inbox, emit)
        });
        assert_eq!(next(&events), Event::Dnd(None));
        sender.send(true).unwrap();
        assert_eq!(next(&events), Event::Dnd(None));
        assert!(*dnd.state.lock().unwrap());
    }

    #[test]
    fn dnd_does_nothing_when_the_service_is_not_installed() {
        let dnd = Arc::new(FakeDnd {
            installed: false,
            answers: true,
            state: Mutex::new(false),
        });
        let (emit, events) = collector();
        let (_sender, inbox) = mpsc::channel();
        dnd_worker(&dnd, inbox, emit);
        assert!(events.try_recv().is_err());
    }
}
