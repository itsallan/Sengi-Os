//! The Bluetooth page's rules: which devices are listed where, and the
//! steps of pairing one, including the questions BlueZ asks on the way.
//! Nothing here talks to BlueZ; sengi-system's `pairing` does, with the
//! agent, and `page` puts it in the window.

pub mod bluez;
pub mod page;

use std::sync::Arc;
use std::time::{Duration, Instant};

pub use sengi_system::pairing::{
    arrange, Adapter as Bluetooth, Answer, Device, Kind, PairError, Prompt, Prompter, Question,
    Reply, MAX_DEVICES,
};

use crate::held::Row;
use crate::network::{Block, Refusal, Words};
use crate::worker::{self, Handle, Inbox, Notify, Waited};

/// A search for devices stops by itself after this long. It keeps the
/// radio busy, and nobody watches the list for longer.
pub const SEARCH_TIME: Duration = Duration::from_secs(30);

/// The name of the icon for a kind of device.
pub fn icon(kind: Kind) -> &'static str {
    match kind {
        Kind::Audio => "headphones",
        Kind::Keyboard => "keyboard",
        Kind::Mouse => "mouse",
        Kind::Computer => "display",
        Kind::Other => "bluetooth",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub bluetooth: Bluetooth,
    /// There is Bluetooth hardware, whatever BlueZ says.
    pub hardware: bool,
    pub block: Block,
}

impl Words for PairError {
    fn words(self) -> &'static str {
        match self {
            PairError::AuthenticationFailed => {
                "The PIN or passkey did not match, or the device refused."
            }
            PairError::Canceled => "Pairing was cancelled.",
            PairError::Timeout => "The device did not answer in time.",
            PairError::Unreachable => {
                "The device could not be reached. Make sure it is switched on, nearby \
                 and in pairing mode."
            }
            PairError::Denied => "You are not allowed to pair Bluetooth devices.",
            // Unavailable, and any reason added later.
            _ => "The Bluetooth service is not answering.",
        }
    }
}

pub trait BluetoothService: Send + Sync + 'static {
    fn read(&self) -> Reading;
    fn set_powered(&self, on: bool) -> Result<(), Refusal>;
    /// Start or stop looking for devices. Returns at once.
    fn search(&self, on: bool);
    /// Returns when the device is paired or the attempt has failed, which
    /// takes as long as the user does. Questions arrive through the
    /// prompter meanwhile.
    fn pair(&self, id: &str) -> Result<(), PairError>;
    fn cancel_pairing(&self, id: &str);
    /// May take ten seconds or more to fail.
    fn connect(&self, id: &str) -> Result<(), Refusal>;
    fn disconnect(&self, id: &str) -> Result<(), Refusal>;
    /// Unpair the device and drop what BlueZ stored about it.
    fn remove(&self, id: &str) -> Result<(), Refusal>;
    fn watch(&self, changed: Notify);
    /// Stop the watches started with `watch`.
    fn unwatch(&self);
    fn on_prompt(&self, prompter: Prompter);
}

#[derive(Debug)]
pub enum Command {
    Power(bool),
    Search(bool),
    Pair(String),
    CancelPairing(String),
    Connect(String),
    Disconnect(String),
    Remove(String),
}

pub enum Event {
    Reading(Reading),
    Prompt(Option<Prompt>),
    Paired {
        id: String,
        result: Result<(), PairError>,
    },
    /// Connecting to `id` ended.
    Connected {
        id: String,
        result: Result<(), Refusal>,
    },
    Refused {
        what: &'static str,
        refusal: Refusal,
    },
}

/// The worker's loop. Pairing and connecting each run on a thread of
/// their own: they take as long as the user or the radio does, and the
/// lists must keep updating meanwhile.
pub fn run(
    service: Arc<dyn BluetoothService>,
    handle: Handle<Command>,
    mut inbox: Inbox<Command>,
    search_time: Duration,
    emit: impl Fn(Event) + Send + Sync + 'static,
) {
    let emit = Arc::new(emit);
    service.watch(handle.notifier());
    service.on_prompt({
        let emit = emit.clone();
        Arc::new(move |prompt| emit(Event::Prompt(prompt)))
    });
    emit(Event::Reading(service.read()));
    // When the search under way has run its time.
    let mut search_ends: Option<Instant> = None;
    loop {
        let next = match inbox.wait_until(search_ends) {
            Waited::Command(command) => Some(command),
            Waited::Refresh => None,
            Waited::TimedOut => {
                search_ends = None;
                service.search(false);
                emit(Event::Reading(service.read()));
                continue;
            }
            Waited::Stopped => break,
        };
        let refused = |what: &'static str, result: Result<(), Refusal>| {
            if let Err(refusal) = result {
                emit(Event::Refused { what, refusal });
            }
        };
        let background = |name: &str, task: Box<dyn FnOnce() -> Event + Send>| {
            let (emit, handle) = (emit.clone(), handle.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("settings-{name}"))
                .spawn(move || {
                    emit(task());
                    handle.refresh();
                });
            if spawned.is_err() {
                eprintln!("sengi-settings: could not start a thread to {name}");
            }
        };
        match next {
            None => {}
            Some(Command::Power(on)) => {
                refused("switch Bluetooth", service.set_powered(on));
            }
            Some(Command::Search(on)) => {
                service.search(on);
                // Started again, the clock starts again.
                search_ends = on.then(|| Instant::now() + search_time);
            }
            Some(Command::Pair(id)) => {
                let service = service.clone();
                background(
                    "pair",
                    Box::new(move || {
                        let result = service.pair(&id);
                        Event::Paired { id, result }
                    }),
                );
            }
            Some(Command::CancelPairing(id)) => service.cancel_pairing(&id),
            Some(Command::Connect(id)) => {
                let service = service.clone();
                background(
                    "connect",
                    Box::new(move || {
                        let result = service.connect(&id);
                        Event::Connected { id, result }
                    }),
                );
            }
            Some(Command::Disconnect(id)) => {
                refused("disconnect the device", service.disconnect(&id));
            }
            Some(Command::Remove(id)) => {
                refused("remove the device", service.remove(&id));
            }
        }
        emit(Event::Reading(service.read()));
    }
    // Whatever happens to the page, a search does not outlive it.
    service.search(false);
}

pub fn start(
    service: Arc<dyn BluetoothService>,
    emit: impl Fn(Event) + Send + Sync + 'static,
) -> Handle<Command> {
    worker::spawn("bluetooth", move |handle, inbox| {
        run(service, handle, inbox, SEARCH_TIME, emit)
    })
}

/// Where the user is in pairing a device.
#[derive(Clone, Default)]
pub enum Pairing {
    #[default]
    Idle,
    /// BlueZ is working on it, and may be asking something.
    Running {
        id: String,
        name: String,
        prompt: Option<Prompt>,
    },
    Failed {
        name: String,
        error: PairError,
    },
}

#[derive(Default)]
pub struct Model {
    reading: Option<Reading>,
    /// The two lists as shown, which while held keep their rows in place.
    mine: Vec<Row<Device>>,
    nearby: Vec<Row<Device>>,
    pub pairing: Pairing,
    /// Devices a connection is being made to.
    connecting: Vec<String>,
    wanted: Option<bool>,
}

impl Model {
    pub fn reading(&self) -> Option<&Reading> {
        self.reading.as_ref()
    }

    pub fn on(&self) -> bool {
        matches!(
            self.reading.as_ref().map(|r| &r.bluetooth),
            Some(Bluetooth::On { .. })
        )
    }

    pub fn searching(&self) -> bool {
        matches!(
            self.reading.as_ref().map(|r| &r.bluetooth),
            Some(Bluetooth::On {
                searching: true,
                ..
            })
        )
    }

    /// Take in a fresh reading. `held_mine` and `held_nearby` are whether
    /// the pointer or the keyboard is on each list.
    pub fn update(&mut self, reading: Reading, held_mine: bool, held_nearby: bool) {
        let devices = match &reading.bluetooth {
            Bluetooth::On { devices, .. } => devices.clone(),
            _ => Vec::new(),
        };
        let on = matches!(reading.bluetooth, Bluetooth::On { .. });
        if self.wanted == Some(on) {
            self.wanted = None;
        }
        if !on {
            self.connecting.clear();
        }
        let (mine, nearby): (Vec<Row<Device>>, Vec<Row<Device>>) = devices
            .into_iter()
            .map(Row::new)
            .partition(|device| device.paired);
        let merge = |shown: &[Row<Device>], fresh: Vec<Row<Device>>, held: bool| {
            crate::held::merge(
                shown,
                fresh,
                held && on,
                |device| &device.id,
                |device| device.gone = true,
                MAX_DEVICES,
            )
        };
        self.mine = merge(&self.mine, mine, held_mine);
        self.nearby = merge(&self.nearby, nearby, held_nearby);
        self.reading = Some(reading);
    }

    pub fn release(&mut self) {
        if let Some(reading) = self.reading.take() {
            self.update(reading, false, false);
        }
    }

    pub fn mine(&self) -> &[Row<Device>] {
        &self.mine
    }

    pub fn nearby(&self) -> &[Row<Device>] {
        &self.nearby
    }

    pub fn find(&self, id: &str) -> Option<&Row<Device>> {
        self.mine
            .iter()
            .chain(&self.nearby)
            .find(|device| device.id == id)
    }

    pub fn want_power(&mut self, on: bool) {
        self.wanted = Some(on);
    }

    pub fn give_up_waiting(&mut self) {
        self.wanted = None;
    }

    pub fn switch_on(&self) -> bool {
        self.wanted.unwrap_or(self.on())
    }

    pub fn is_connecting(&self, id: &str) -> bool {
        self.connecting.iter().any(|other| other == id)
    }

    /// A paired device's row was activated: connect it, or disconnect it.
    pub fn toggle(&mut self, id: &str) -> Option<Command> {
        let device = self
            .find(id)
            .filter(|device| device.paired && !device.gone)?;
        if device.connected {
            return Some(Command::Disconnect(id.to_string()));
        }
        if self.is_connecting(id) {
            return None;
        }
        self.connecting.push(id.to_string());
        Some(Command::Connect(id.to_string()))
    }

    pub fn connected(&mut self, id: &str) {
        self.connecting.retain(|other| other != id);
    }

    /// A nearby device's row was activated: pair with it.
    pub fn pair(&mut self, id: &str) -> Option<Command> {
        if matches!(self.pairing, Pairing::Running { .. }) {
            return None;
        }
        let device = self
            .find(id)
            .filter(|device| !device.paired && !device.gone)?;
        self.pairing = Pairing::Running {
            id: id.to_string(),
            name: device.name.clone(),
            prompt: None,
        };
        Some(Command::Pair(id.to_string()))
    }

    /// BlueZ asked something, or withdrew what it was asking. A question
    /// about a device that is not the one being paired is refused: nobody
    /// here started it.
    pub fn prompted(&mut self, prompt: Option<Prompt>) {
        let Pairing::Running {
            id, prompt: slot, ..
        } = &mut self.pairing
        else {
            if let Some(prompt) = prompt {
                prompt.reply.answer(Answer::No);
            }
            return;
        };
        match prompt {
            Some(prompt) if prompt.device != *id => prompt.reply.answer(Answer::No),
            // A question that replaces another withdraws it.
            other => {
                if let Some(old) = std::mem::replace(slot, other) {
                    old.reply.answer(Answer::Cancelled);
                }
            }
        }
    }

    /// The user answered the question on screen.
    pub fn answer(&mut self, answer: Answer) {
        if let Pairing::Running { prompt, .. } = &mut self.pairing {
            // A number that is only being shown stays up until BlueZ is
            // done; there is nothing to say to it.
            if prompt.as_ref().is_some_and(|p| p.question.wants_answer()) {
                if let Some(prompt) = prompt.take() {
                    prompt.reply.answer(answer);
                }
            }
        }
    }

    /// Whether the service is in the middle of something the user began
    /// and is owed the end of: a pairing, or a connection being made.
    pub fn busy(&self) -> bool {
        matches!(self.pairing, Pairing::Running { .. }) || !self.connecting.is_empty()
    }

    /// The user backed out. The attempt goes on until the service says it
    /// has ended; this only tells it to stop.
    pub fn cancel(&mut self) -> Option<Command> {
        match &mut self.pairing {
            Pairing::Running { id, prompt, .. } => {
                if let Some(prompt) = prompt.take() {
                    prompt.reply.answer(Answer::No);
                }
                Some(Command::CancelPairing(id.clone()))
            }
            Pairing::Failed { .. } => {
                self.pairing = Pairing::Idle;
                None
            }
            Pairing::Idle => None,
        }
    }

    /// The service has finished pairing `finished`. On success the device
    /// is to be connected, which is the command returned.
    pub fn paired(&mut self, finished: &str, result: Result<(), PairError>) -> Option<Command> {
        let Pairing::Running { id, name, prompt } = &mut self.pairing else {
            return None;
        };
        if id != finished {
            return None;
        }
        if let Some(prompt) = prompt.take() {
            prompt.reply.answer(Answer::Cancelled);
        }
        let (id, name) = (std::mem::take(id), std::mem::take(name));
        match result {
            Ok(()) => {
                self.pairing = Pairing::Idle;
                self.connecting.push(id.clone());
                Some(Command::Connect(id))
            }
            Err(PairError::Canceled) => {
                self.pairing = Pairing::Idle;
                None
            }
            Err(error) => {
                self.pairing = Pairing::Failed { name, error };
                None
            }
        }
    }
}

/// A made-up Bluetooth service, for the tests and, in debug builds only,
/// for looking at states the machine at hand cannot produce.
#[cfg(any(test, debug_assertions))]
pub mod testing {
    use super::*;
    use std::sync::Mutex;

    pub fn device(id: &str, name: &str, kind: Kind, paired: bool, connected: bool) -> Device {
        Device {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            paired,
            connected,
        }
    }

    #[derive(Default)]
    pub struct FakeState {
        pub powered: bool,
        pub searching: bool,
        pub devices: Vec<Device>,
        /// Devices that turn up once a search is on.
        pub hidden: Vec<Device>,
        /// What pairing with a device asks, and the typed answer it wants.
        pub asks: Vec<(String, Question, Option<String>)>,
        pub log: Vec<String>,
        pub deny: bool,
        pub notify: Option<Notify>,
        pub prompter: Option<Prompter>,
        pub delay: Duration,
    }

    #[derive(Default)]
    pub struct Fake {
        pub state: Mutex<FakeState>,
    }

    impl Fake {
        pub fn with(devices: Vec<Device>) -> Arc<Fake> {
            let fake = Fake::default();
            {
                let mut state = fake.lock();
                state.powered = true;
                state.devices = devices;
            }
            Arc::new(fake)
        }

        pub fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.state.lock().unwrap_or_else(|p| p.into_inner())
        }

        pub fn log(&self) -> Vec<String> {
            self.lock().log.clone()
        }

        fn with_device(&self, id: &str, change: impl FnOnce(&mut Device)) -> bool {
            let found = match self.lock().devices.iter_mut().find(|d| d.id == id) {
                Some(device) => {
                    change(device);
                    true
                }
                None => false,
            };
            found
        }
    }

    impl BluetoothService for Fake {
        fn read(&self) -> Reading {
            let state = self.lock();
            Reading {
                bluetooth: if state.powered {
                    Bluetooth::On {
                        searching: state.searching,
                        devices: arrange(state.devices.clone()),
                    }
                } else {
                    Bluetooth::Off
                },
                hardware: true,
                block: Block::None,
            }
        }

        fn set_powered(&self, on: bool) -> Result<(), Refusal> {
            let mut state = self.lock();
            if state.deny {
                return Err(Refusal::Denied);
            }
            state.log.push(format!("power {on}"));
            state.powered = on;
            Ok(())
        }

        fn search(&self, on: bool) {
            let mut state = self.lock();
            state.log.push(format!("search {on}"));
            state.searching = on && state.powered;
            if state.searching {
                let found = std::mem::take(&mut state.hidden);
                state.devices.extend(found);
            }
        }

        fn pair(&self, id: &str) -> Result<(), PairError> {
            let (delay, ask, prompter) = {
                let mut state = self.lock();
                state.log.push(format!("pair {id}"));
                let ask = state
                    .asks
                    .iter()
                    .find(|(device, _, _)| device == id)
                    .cloned();
                (state.delay, ask, state.prompter.clone())
            };
            std::thread::sleep(delay);
            if !self.with_device(id, |_| {}) {
                return Err(PairError::Unreachable);
            }
            if let (Some((_, question, wanted)), Some(prompter)) = (ask, prompter) {
                let reply = Reply::new();
                let silent = !question.wants_answer();
                prompter(Some(Prompt {
                    device: id.to_string(),
                    question,
                    reply: reply.clone(),
                }));
                let answer = if silent {
                    std::thread::sleep(delay);
                    Answer::Yes
                } else {
                    reply.wait_blocking(Duration::from_secs(60))
                };
                let ok = match (&answer, &wanted) {
                    (Answer::Yes, None) => true,
                    (Answer::Typed(typed), Some(wanted)) => typed.expose() == wanted,
                    (Answer::Cancelled | Answer::No, _) => {
                        self.lock().log.push("declined".to_string());
                        return Err(PairError::Canceled);
                    }
                    _ => false,
                };
                self.lock().log.push(format!("answered {answer:?}"));
                if !ok {
                    return Err(PairError::AuthenticationFailed);
                }
            }
            self.with_device(id, |device| device.paired = true);
            Ok(())
        }

        fn cancel_pairing(&self, id: &str) {
            self.lock().log.push(format!("cancel {id}"));
        }

        fn connect(&self, id: &str) -> Result<(), Refusal> {
            self.lock().log.push(format!("connect {id}"));
            let delay = self.lock().delay;
            std::thread::sleep(delay);
            if self.with_device(id, |device| device.connected = true) {
                Ok(())
            } else {
                Err(Refusal::Failed)
            }
        }

        fn disconnect(&self, id: &str) -> Result<(), Refusal> {
            self.lock().log.push(format!("disconnect {id}"));
            self.with_device(id, |device| device.connected = false);
            Ok(())
        }

        fn remove(&self, id: &str) -> Result<(), Refusal> {
            let mut state = self.lock();
            if state.deny {
                return Err(Refusal::Denied);
            }
            state.log.push(format!("remove {id}"));
            state.devices.retain(|device| device.id != id);
            Ok(())
        }

        fn watch(&self, changed: Notify) {
            self.lock().notify = Some(changed);
        }

        fn unwatch(&self) {
            self.lock().notify = None;
        }

        fn on_prompt(&self, prompter: Prompter) {
            self.lock().prompter = Some(prompter);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::secret::Secret;
    use std::sync::{mpsc, Mutex};

    fn sample() -> Vec<Device> {
        vec![
            device("/d/mouse", "Travel mouse", Kind::Mouse, false, false),
            device("/d/head", "Trail headphones", Kind::Audio, true, false),
            device("/d/keys", "Keyboard K3", Kind::Keyboard, true, true),
            device("/d/phone", "A phone", Kind::Other, false, false),
        ]
    }

    fn reading(devices: Vec<Device>) -> Reading {
        Reading {
            bluetooth: Bluetooth::On {
                searching: false,
                devices: arrange(devices),
            },
            hardware: true,
            block: Block::None,
        }
    }

    fn model() -> Model {
        let mut model = Model::default();
        model.update(reading(sample()), false, false);
        model
    }

    fn prompt(device: &str, question: Question) -> (Prompt, Reply) {
        let reply = Reply::new();
        (
            Prompt {
                device: device.to_string(),
                question,
                reply: reply.clone(),
            },
            reply,
        )
    }

    fn answered(reply: &Reply) -> Answer {
        reply.wait_blocking(Duration::from_millis(1))
    }

    #[test]
    fn paired_devices_and_nearby_ones_are_listed_apart_by_name() {
        let model = model();
        let names = |devices: &[Row<Device>]| -> Vec<String> {
            devices.iter().map(|d| d.name.clone()).collect()
        };
        assert_eq!(names(model.mine()), ["Keyboard K3", "Trail headphones"]);
        assert_eq!(names(model.nearby()), ["A phone", "Travel mouse"]);
    }

    #[test]
    fn icons_by_kind() {
        assert_eq!(icon(Kind::from_icon("audio-headset")), "headphones");
        assert_eq!(icon(Kind::from_icon("input-keyboard")), "keyboard");
        assert_eq!(icon(Kind::from_icon("phone")), "bluetooth");
    }

    #[test]
    fn a_paired_device_connects_and_disconnects() {
        let mut model = model();
        assert!(matches!(model.toggle("/d/head"), Some(Command::Connect(id)) if id == "/d/head"));
        assert!(model.is_connecting("/d/head"));
        // A second press while it is connecting does nothing.
        assert!(model.toggle("/d/head").is_none());
        model.connected("/d/head");
        assert!(!model.is_connecting("/d/head"));
        assert!(matches!(
            model.toggle("/d/keys"),
            Some(Command::Disconnect(_))
        ));
        // A device that is not paired is not connected this way.
        assert!(model.toggle("/d/mouse").is_none());
        assert!(model.toggle("/d/nowhere").is_none());
    }

    #[test]
    fn pairing_with_a_passkey_to_confirm() {
        let mut model = model();
        assert!(matches!(model.pair("/d/mouse"), Some(Command::Pair(_))));
        // Only one at a time.
        assert!(model.pair("/d/phone").is_none());
        let (asked, reply) = prompt("/d/mouse", Question::ConfirmPasskey(123456));
        model.prompted(Some(asked));
        model.answer(Answer::Yes);
        assert!(matches!(answered(&reply), Answer::Yes));
        // Paired: now it is to be connected.
        assert!(matches!(
            model.paired("/d/mouse", Ok(())),
            Some(Command::Connect(_))
        ));
        assert!(matches!(model.pairing, Pairing::Idle));
        assert!(model.is_connecting("/d/mouse"));
    }

    #[test]
    fn a_wrong_pin_fails_and_says_so() {
        let mut model = model();
        model.pair("/d/mouse");
        let (asked, reply) = prompt("/d/mouse", Question::EnterPin);
        model.prompted(Some(asked));
        model.answer(Answer::Typed(Secret::new("1234".to_string())));
        assert!(matches!(answered(&reply), Answer::Typed(pin) if pin.expose() == "1234"));
        assert!(model
            .paired("/d/mouse", Err(PairError::AuthenticationFailed))
            .is_none());
        assert!(matches!(
            model.pairing,
            Pairing::Failed {
                error: PairError::AuthenticationFailed,
                ..
            }
        ));
        // Acknowledged: back to idle, and it can be tried again.
        assert!(model.cancel().is_none());
        assert!(model.pair("/d/mouse").is_some());
    }

    #[test]
    fn cancelling_refuses_the_question_and_tells_the_service() {
        let mut model = model();
        model.pair("/d/mouse");
        let (asked, reply) = prompt("/d/mouse", Question::ConfirmPasskey(1));
        model.prompted(Some(asked));
        assert!(matches!(model.cancel(), Some(Command::CancelPairing(id)) if id == "/d/mouse"));
        assert!(matches!(answered(&reply), Answer::No));
        // Still running until the service says it has ended.
        assert!(matches!(model.pairing, Pairing::Running { .. }));
        assert!(model.paired("/d/mouse", Err(PairError::Canceled)).is_none());
        assert!(matches!(model.pairing, Pairing::Idle));
    }

    #[test]
    fn a_question_about_another_device_is_refused() {
        let mut model = model();
        // Nothing is being paired at all.
        let (asked, reply) = prompt("/d/phone", Question::Authorize);
        model.prompted(Some(asked));
        assert!(matches!(answered(&reply), Answer::No));
        // Something else is.
        model.pair("/d/mouse");
        let (asked, reply) = prompt("/d/phone", Question::ConfirmPasskey(7));
        model.prompted(Some(asked));
        assert!(matches!(answered(&reply), Answer::No));
        assert!(matches!(
            model.pairing,
            Pairing::Running { prompt: None, .. }
        ));
    }

    #[test]
    fn a_number_that_is_only_shown_takes_no_answer() {
        let mut model = model();
        model.pair("/d/mouse");
        let (asked, reply) = prompt("/d/mouse", Question::ShowPasskey(424242));
        model.prompted(Some(asked));
        model.answer(Answer::Yes);
        assert!(matches!(answered(&reply), Answer::Cancelled));
        assert!(matches!(
            model.pairing,
            Pairing::Running {
                prompt: Some(_),
                ..
            }
        ));
        // BlueZ withdraws it when the device has typed the number.
        model.prompted(None);
        assert!(matches!(
            model.pairing,
            Pairing::Running { prompt: None, .. }
        ));
    }

    #[test]
    fn the_end_of_pairing_withdraws_a_question_left_open() {
        let mut model = model();
        model.pair("/d/mouse");
        let (asked, reply) = prompt("/d/mouse", Question::ConfirmPasskey(1));
        model.prompted(Some(asked));
        model.paired("/d/mouse", Err(PairError::Timeout));
        assert!(matches!(answered(&reply), Answer::Cancelled));
        // The end of some other pairing changes nothing.
        let mut other = Model::default();
        other.update(reading(sample()), false, false);
        other.pair("/d/mouse");
        assert!(other.paired("/d/phone", Ok(())).is_none());
        assert!(matches!(other.pairing, Pairing::Running { .. }));
    }

    #[test]
    fn the_page_is_busy_while_a_pairing_or_a_connection_is_owed_its_end() {
        let mut model = model();
        assert!(!model.busy());
        model.toggle("/d/head");
        assert!(model.busy());
        model.connected("/d/head");
        assert!(!model.busy());
        model.pair("/d/mouse");
        assert!(model.busy());
        // Paired: now it is being connected.
        assert!(model.paired("/d/mouse", Ok(())).is_some());
        assert!(model.busy());
        model.connected("/d/mouse");
        assert!(!model.busy());
    }

    #[test]
    fn held_lists_keep_their_rows() {
        let mut model = model();
        let mut next = sample();
        next.retain(|d| d.id != "/d/phone");
        next.push(device("/d/aaa", "Aardvark", Kind::Other, false, false));
        model.update(reading(next), false, true);
        let nearby: Vec<(&str, bool)> = model
            .nearby()
            .iter()
            .map(|d| (d.id.as_str(), d.gone))
            .collect();
        assert_eq!(
            nearby,
            [("/d/phone", true), ("/d/mouse", false), ("/d/aaa", false)]
        );
        // A row that has gone cannot be paired with.
        assert!(model.pair("/d/phone").is_none());
        model.release();
        assert_eq!(model.nearby()[0].id, "/d/aaa");
    }

    #[test]
    fn the_switch_shows_what_was_asked_until_the_service_agrees() {
        let mut model = model();
        model.want_power(false);
        assert!(!model.switch_on());
        model.give_up_waiting();
        assert!(model.switch_on());
    }

    type Events = mpsc::Receiver<Event>;

    /// How long a search lasts in these tests.
    const SEARCH: Duration = Duration::from_millis(300);

    fn worker(fake: &Arc<Fake>) -> (Handle<Command>, Events, std::thread::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (handle, inbox) = worker::channel();
        let thread = std::thread::spawn({
            let (service, handle) = (fake.clone(), handle.clone());
            move || {
                run(service, handle, inbox, SEARCH, move |event| {
                    let _ = tx.lock().unwrap().send(event);
                })
            }
        });
        (handle, rx, thread)
    }

    fn until<T>(rx: &Events, mut pick: impl FnMut(Event) -> Option<T>) -> T {
        loop {
            let event = rx.recv_timeout(Duration::from_secs(5)).expect("an event");
            if let Some(found) = pick(event) {
                return found;
            }
        }
    }

    #[test]
    fn the_worker_pairs_through_a_question_and_back() {
        let fake = Fake::with(sample());
        fake.lock().asks = vec![
            ("/d/mouse".into(), Question::ConfirmPasskey(123456), None),
            ("/d/phone".into(), Question::EnterPin, Some("0000".into())),
        ];
        let (handle, rx, thread) = worker(&fake);

        handle.send(Command::Pair("/d/mouse".into()));
        let asked = until(&rx, |event| match event {
            Event::Prompt(Some(prompt)) => Some(prompt),
            _ => None,
        });
        assert_eq!(asked.question, Question::ConfirmPasskey(123456));
        asked.reply.answer(Answer::Yes);
        let result = until(&rx, |event| match event {
            Event::Paired { result, .. } => Some(result),
            _ => None,
        });
        assert_eq!(result, Ok(()));

        // The wrong PIN, then called off at the question.
        for (answer, expected) in [
            (
                Answer::Typed(Secret::new("9999".into())),
                Err(PairError::AuthenticationFailed),
            ),
            (Answer::No, Err(PairError::Canceled)),
        ] {
            handle.send(Command::Pair("/d/phone".into()));
            let asked = until(&rx, |event| match event {
                Event::Prompt(Some(prompt)) => Some(prompt),
                _ => None,
            });
            asked.reply.answer(answer);
            let result = until(&rx, |event| match event {
                Event::Paired { result, .. } => Some(result),
                _ => None,
            });
            assert_eq!(result, expected);
        }

        handle.send(Command::Connect("/d/mouse".into()));
        until(&rx, |event| match event {
            Event::Connected { result, .. } => Some(result),
            _ => None,
        })
        .unwrap();
        handle.send(Command::Remove("/d/mouse".into()));
        handle.stop();
        thread.join().unwrap();
        let log = fake.log();
        assert!(log.contains(&"remove /d/mouse".to_string()));
        // The PIN is in none of what was recorded.
        assert!(log.iter().all(|line| !line.contains("9999")), "{log:?}");
        // The worker's last act is to stop any search.
        assert_eq!(log.last().unwrap(), "search false");
    }

    #[test]
    fn a_search_finds_devices_and_a_refusal_is_reported() {
        let fake = Fake::with(sample());
        fake.lock().hidden = vec![device("/d/new", "New thing", Kind::Other, false, false)];
        let (handle, rx, thread) = worker(&fake);
        handle.send(Command::Search(true));
        until(&rx, |event| match event {
            Event::Reading(Reading {
                bluetooth:
                    Bluetooth::On {
                        searching: true,
                        devices,
                    },
                ..
            }) if devices.iter().any(|d| d.id == "/d/new") => Some(()),
            _ => None,
        });
        handle.send(Command::Search(false));
        fake.lock().deny = true;
        handle.send(Command::Power(false));
        assert!(
            until(&rx, |event| match event {
                Event::Refused { what, refusal } => Some((what, refusal)),
                _ => None,
            }) == ("switch Bluetooth", Refusal::Denied)
        );
        handle.stop();
        thread.join().unwrap();
        assert_eq!(fake.log(), ["search true", "search false", "search false"]);
    }

    #[test]
    fn a_search_ends_by_itself_and_starting_again_restarts_its_one_clock() {
        let fake = Fake::with(sample());
        let (handle, rx, thread) = worker(&fake);
        let searching = |wanted: bool| {
            move |event| match event {
                Event::Reading(Reading {
                    bluetooth: Bluetooth::On { searching, .. },
                    ..
                }) if searching == wanted => Some(Instant::now()),
                _ => None,
            }
        };
        handle.send(Command::Search(true));
        until(&rx, searching(true));
        // Pressed again most of the way through: the time starts over,
        // and nothing is left behind to end this search early.
        std::thread::sleep(SEARCH * 2 / 3);
        let restarted = Instant::now();
        handle.send(Command::Search(true));
        let ended = until(&rx, searching(false));
        assert!(ended.duration_since(restarted) >= SEARCH, "ended early");
        // One clock: once it has run out nothing else stops a search.
        handle.send(Command::Search(true));
        until(&rx, searching(true));
        handle.send(Command::Search(false));
        until(&rx, searching(false));
        std::thread::sleep(SEARCH * 2);
        handle.stop();
        thread.join().unwrap();
        assert_eq!(
            fake.log(),
            [
                "search true",
                "search true",
                "search false",
                "search true",
                "search false",
                "search false"
            ]
        );
    }
}
