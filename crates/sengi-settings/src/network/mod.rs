//! The Network page's rules: what Wi-Fi and the wired links look like to
//! the page, which networks are listed, and what the page does about the
//! user's choices. Nothing here talks to iwd or reads `/sys`.
//!
//! The networks, their order and the steps of joining one are
//! sengi-system's `wifi`, the same as the quick settings panel uses.
//! What is added here is the words for them, the held list, and a
//! reading that also has the cabled interfaces and the addresses.

pub mod iwd;
pub mod page;

use std::sync::Arc;

pub use sengi_system::links::Wired;
pub use sengi_system::services::{Block, Refusal};
pub use sengi_system::wifi::{
    arrange, check_passphrase, level, Join, JoinError, Link, Network, Problem, Request, Security,
    Wifi, MAX_NETWORKS,
};

use crate::held::Row;
use crate::worker::{self, Handle, Inbox, Next, Notify};

/// The page's words for a thing of sengi-system's.
pub trait Words {
    fn words(self) -> &'static str;
}

impl Words for Security {
    fn words(self) -> &'static str {
        match self {
            Security::Open => "Open, not encrypted",
            Security::Passphrase => "Password protected",
            Security::Enterprise => "Enterprise (802.1X)",
            Security::Other => "Unsupported security",
        }
    }
}

impl Words for JoinError {
    fn words(self) -> &'static str {
        match self {
            JoinError::Failed => "The network refused the connection.",
            JoinError::NeedsPassphrase => "The network needs a password.",
            JoinError::Aborted => "The attempt was called off.",
            JoinError::Timeout => "The network did not answer in time.",
            JoinError::NotConfigured => {
                "This is an enterprise network. It has to be set up with a provisioning \
                 file in /var/lib/iwd before it can be joined."
            }
            JoinError::NotSupported => "This kind of network is not supported.",
            JoinError::Denied => "You are not allowed to change Wi-Fi connections.",
            // Unavailable, and any reason added later.
            _ => "The Wi-Fi service is not answering.",
        }
    }
}

impl Words for Problem {
    fn words(self) -> &'static str {
        match self {
            Problem::TooShort => "A password has at least 8 characters.",
            Problem::TooLong => "A password has at most 63 characters.",
            Problem::NotPlain => "A password is made of plain letters, digits and symbols.",
            Problem::Rejected => "That password was not accepted. Try again.",
            Problem::Stale => "The saved password no longer works. Enter the new one.",
        }
    }
}

pub fn strength(signal: i16) -> &'static str {
    match level(signal) {
        4 => "Excellent",
        3 => "Good",
        2 => "Fair",
        _ => "Weak",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub wifi: Wifi,
    /// The Wi-Fi interface's addresses, while it is on a network.
    pub addresses: Vec<String>,
    /// There is Wi-Fi hardware, whatever iwd says.
    pub hardware: bool,
    pub block: Block,
    pub wired: Vec<Wired>,
}

pub trait NetworkService: Send + Sync + 'static {
    fn read(&self) -> Reading;
    fn set_powered(&self, on: bool) -> Result<(), Refusal>;
    /// Ask for a fresh look at what is in range. Returns at once.
    fn scan(&self);
    /// Returns when the network is joined or the attempt has failed, which
    /// can take half a minute. `read` and the rest stay usable meanwhile.
    fn join(&self, request: Request) -> Result<(), JoinError>;
    /// Leave the connected network, or call off a join under way.
    fn disconnect(&self) -> Result<(), Refusal>;
    /// Have the service drop what it stored about a network.
    fn forget(&self, id: &str) -> Result<(), Refusal>;
    fn watch(&self, changed: Notify);
}

#[derive(Debug)]
pub enum Command {
    Power(bool),
    Scan,
    Join(Request),
    Disconnect,
    Forget(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Reading(Reading),
    Joined {
        id: String,
        result: Result<(), JoinError>,
    },
    /// `what` could not be done.
    Refused {
        what: &'static str,
        refusal: Refusal,
    },
}

/// The worker's loop: carry out commands, read after each and whenever
/// the service says something changed. A join runs on a thread of its own
/// so that the list keeps updating, and can be called off, while it waits.
pub fn run(
    service: Arc<dyn NetworkService>,
    handle: Handle<Command>,
    mut inbox: Inbox<Command>,
    emit: impl Fn(Event) + Send + Sync + 'static,
) {
    let emit = Arc::new(emit);
    service.watch(handle.notifier());
    let first = service.read();
    let on = matches!(first.wifi, Wifi::On { .. });
    emit(Event::Reading(first));
    if on {
        service.scan();
    }
    while let Some(next) = inbox.wait() {
        let refused = |what: &'static str, result: Result<(), Refusal>| {
            if let Err(refusal) = result {
                emit(Event::Refused { what, refusal });
            }
        };
        match next {
            Next::Refresh => {}
            Next::Command(Command::Power(on)) => {
                refused("switch Wi-Fi", service.set_powered(on));
            }
            Next::Command(Command::Scan) => service.scan(),
            Next::Command(Command::Disconnect) => {
                refused("disconnect", service.disconnect());
            }
            Next::Command(Command::Forget(id)) => {
                refused("forget the network", service.forget(&id));
            }
            Next::Command(Command::Join(request)) => {
                let (service, emit, handle) = (service.clone(), emit.clone(), handle.clone());
                let spawned = std::thread::Builder::new()
                    .name("settings-join".to_string())
                    .spawn(move || {
                        let id = request.id.clone();
                        let result = service.join(request);
                        emit(Event::Joined { id, result });
                        handle.refresh();
                    });
                if spawned.is_err() {
                    eprintln!("sengi-settings: could not start a thread to join the network");
                }
            }
        }
        emit(Event::Reading(service.read()));
    }
}

/// Start the worker for `service`.
pub fn start(
    service: Arc<dyn NetworkService>,
    emit: impl Fn(Event) + Send + Sync + 'static,
) -> Handle<Command> {
    worker::spawn("network", move |handle, inbox| {
        run(service, handle, inbox, emit)
    })
}

/// What the page shows, and what it does about the user's choices.
#[derive(Debug, Default)]
pub struct Model {
    reading: Option<Reading>,
    /// The networks as listed, which while the list is held is not the
    /// order they were last read in.
    shown: Vec<Row<Network>>,
    pub join: Join,
    /// What the Wi-Fi switch was last set to and has not yet been seen
    /// to take effect.
    wanted: Option<bool>,
}

/// What the page should do after the user picked a network.
#[derive(Debug)]
pub enum Step {
    Nothing,
    Send(Request),
    AskPassphrase,
    ConfirmOpen,
    /// Say why it cannot be joined.
    Explain,
}

impl Model {
    pub fn reading(&self) -> Option<&Reading> {
        self.reading.as_ref()
    }

    /// The networks in range, when Wi-Fi is on.
    fn networks(&self) -> &[Network] {
        match self.reading.as_ref().map(|reading| &reading.wifi) {
            Some(Wifi::On { networks, .. }) => networks,
            _ => &[],
        }
    }

    /// Where the device stands, when Wi-Fi is on.
    pub fn link(&self) -> Option<Link> {
        match self.reading.as_ref()?.wifi {
            Wifi::On { link, .. } => Some(link),
            _ => None,
        }
    }

    pub fn scanning(&self) -> bool {
        matches!(
            self.reading.as_ref().map(|reading| &reading.wifi),
            Some(Wifi::On { scanning: true, .. })
        )
    }

    /// Take in a fresh reading. `held` is whether the pointer or the
    /// keyboard is on the list.
    pub fn update(&mut self, reading: Reading, held: bool) {
        let fresh: Vec<Row<Network>> = match &reading.wifi {
            Wifi::On { networks, .. } => networks
                .iter()
                .filter(|network| !network.connected)
                .cloned()
                .map(Row::new)
                .collect(),
            _ => Vec::new(),
        };
        let on = matches!(reading.wifi, Wifi::On { .. });
        if !on {
            self.join.reset();
        }
        if self.wanted == Some(on) {
            self.wanted = None;
        }
        self.shown = crate::held::merge(
            &self.shown,
            fresh,
            held && on,
            |network| &network.id,
            |network| network.gone = true,
            MAX_NETWORKS,
        );
        self.reading = Some(reading);
    }

    /// The list was let go of: it may take the service's order again.
    pub fn release(&mut self) {
        if let Some(reading) = self.reading.take() {
            self.update(reading, false);
        }
    }

    /// The networks to list: all in range but the connected one, which
    /// has a place of its own.
    pub fn listed(&self) -> &[Row<Network>] {
        &self.shown
    }

    pub fn connected(&self) -> Option<&Network> {
        self.networks().iter().find(|network| network.connected)
    }

    fn find(&self, id: &str) -> Option<&Network> {
        self.shown
            .iter()
            .map(|row| &row.item)
            .chain(self.networks())
            .find(|network| network.id == id)
    }

    /// Whether the row for `id` is of a network that has left.
    fn gone(&self, id: &str) -> bool {
        self.shown.iter().any(|row| row.gone && row.id == id)
    }

    pub fn name_of(&self, id: &str) -> Option<&str> {
        self.find(id).map(|network| network.name.as_str())
    }

    /// The switch was flipped. Until a reading agrees, the switch shows
    /// what was asked for.
    pub fn want_power(&mut self, on: bool) {
        self.wanted = Some(on);
    }

    /// A refusal arrived: whatever was hoped for is not coming.
    pub fn give_up_waiting(&mut self) {
        self.wanted = None;
    }

    pub fn switch_on(&self) -> bool {
        self.wanted.unwrap_or(matches!(
            self.reading.as_ref().map(|r| &r.wifi),
            Some(Wifi::On { .. })
        ))
    }

    /// A row was activated.
    pub fn choose(&mut self, id: &str) -> Step {
        let Some(network) = self.find(id).cloned() else {
            return Step::Nothing;
        };
        // A row that has left the air is kept in a held list and cannot
        // be joined.
        if self.gone(id) {
            return Step::Nothing;
        }
        if let Some(request) = self.join.choose(&network) {
            return Step::Send(request);
        }
        match self.join {
            Join::Asking { .. } => Step::AskPassphrase,
            Join::Confirming { .. } => Step::ConfirmOpen,
            Join::Failed { .. } => Step::Explain,
            _ => Step::Nothing,
        }
    }

    /// Whether `id` may be forgotten: iwd has it stored.
    pub fn can_forget(&self, id: &str) -> bool {
        self.find(id).is_some_and(|network| network.known)
    }
}

/// A made-up Wi-Fi service, for the tests and, in debug builds only, for
/// looking at states the machine at hand cannot produce. A release build
/// has no way to show an invented network.
#[cfg(any(test, debug_assertions))]
pub mod testing {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    pub fn network(id: &str, name: &str, security: Security, known: bool, signal: i16) -> Network {
        Network {
            id: id.to_string(),
            name: name.to_string(),
            security,
            known,
            connected: false,
            signal,
        }
    }

    pub fn reading(networks: Vec<Network>) -> Reading {
        Reading {
            wifi: Wifi::On {
                link: if networks.iter().any(|n| n.connected) {
                    Link::Connected
                } else {
                    Link::Disconnected
                },
                scanning: false,
                networks: arrange(networks),
            },
            addresses: Vec::new(),
            hardware: true,
            block: Block::None,
            wired: Vec::new(),
        }
    }

    /// A made-up service: a list of networks, the passphrase each wants,
    /// and a record of what was asked of it.
    #[derive(Default)]
    pub struct Fake {
        pub state: Mutex<FakeState>,
    }

    #[derive(Default)]
    pub struct FakeState {
        pub powered: bool,
        pub networks: Vec<Network>,
        /// The passphrase a network accepts, by id.
        pub accepts: Vec<(String, String)>,
        pub log: Vec<String>,
        pub deny: bool,
        pub notify: Option<Notify>,
        /// How long a join takes.
        pub delay: Duration,
        pub wired: Vec<Wired>,
    }

    impl Fake {
        pub fn with(networks: Vec<Network>) -> Arc<Fake> {
            let fake = Fake::default();
            {
                let mut state = fake.state.lock().unwrap();
                state.powered = true;
                state.networks = networks;
            }
            Arc::new(fake)
        }

        pub fn log(&self) -> Vec<String> {
            self.state.lock().unwrap().log.clone()
        }
    }

    impl NetworkService for Fake {
        fn read(&self) -> Reading {
            let state = self.state.lock().unwrap();
            if !state.powered {
                return Reading {
                    wifi: Wifi::Off,
                    addresses: Vec::new(),
                    hardware: true,
                    block: Block::None,
                    wired: Vec::new(),
                };
            }
            let mut reading = reading(state.networks.clone());
            reading.wired = state.wired.clone();
            if let Wifi::On {
                link: Link::Connected,
                ..
            } = reading.wifi
            {
                reading.addresses = vec!["192.168.1.42".to_string()];
            }
            reading
        }

        fn set_powered(&self, on: bool) -> Result<(), Refusal> {
            let mut state = self.state.lock().unwrap();
            if state.deny {
                return Err(Refusal::Denied);
            }
            state.powered = on;
            state.log.push(format!("power {on}"));
            Ok(())
        }

        fn scan(&self) {
            self.state.lock().unwrap().log.push("scan".to_string());
        }

        fn join(&self, request: Request) -> Result<(), JoinError> {
            let delay = self.state.lock().unwrap().delay;
            std::thread::sleep(delay);
            let mut state = self.state.lock().unwrap();
            state.log.push(format!(
                "join {} typed={}",
                request.id,
                request.passphrase.is_some()
            ));
            let wanted = state
                .accepts
                .iter()
                .find(|(id, _)| *id == request.id)
                .map(|(_, passphrase)| passphrase.clone());
            let Some(index) = state.networks.iter().position(|n| n.id == request.id) else {
                return Err(JoinError::Unavailable);
            };
            let known = state.networks[index].known;
            match (wanted, request.passphrase) {
                (Some(_), None) if !known => return Err(JoinError::NeedsPassphrase),
                (Some(wanted), Some(given)) if wanted != given.expose() => {
                    return Err(JoinError::Failed)
                }
                _ => {}
            }
            for network in state.networks.iter_mut() {
                network.connected = false;
            }
            state.networks[index].connected = true;
            state.networks[index].known = true;
            Ok(())
        }

        fn disconnect(&self) -> Result<(), Refusal> {
            let mut state = self.state.lock().unwrap();
            state.log.push("disconnect".to_string());
            for network in state.networks.iter_mut() {
                network.connected = false;
            }
            Ok(())
        }

        fn forget(&self, id: &str) -> Result<(), Refusal> {
            let mut state = self.state.lock().unwrap();
            if state.deny {
                return Err(Refusal::Denied);
            }
            state.log.push(format!("forget {id}"));
            for network in state.networks.iter_mut().filter(|n| n.id == id) {
                network.known = false;
                network.connected = false;
            }
            Ok(())
        }

        fn watch(&self, changed: Notify) {
            self.state.lock().unwrap().notify = Some(changed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::secret::Secret;
    use std::sync::mpsc;
    use std::time::Duration;

    fn secret(text: &str) -> Secret {
        Secret::new(text.to_string())
    }

    fn sample() -> Vec<Network> {
        vec![
            network("/n/cafe", "Cafe", Security::Open, false, -7100),
            network("/n/home", "Home", Security::Passphrase, true, -5200),
            network("/n/lab", "Lab", Security::Enterprise, false, -6000),
            network("/n/new", "New", Security::Passphrase, false, -6500),
            network("/n/wep", "Old", Security::Other, false, -5000),
        ]
    }

    fn model() -> Model {
        let mut model = Model::default();
        model.update(reading(sample()), false);
        model
    }

    #[test]
    fn signal_levels() {
        assert_eq!(level(-4000), 4);
        assert_eq!(level(-6000), 4);
        assert_eq!(level(-6001), 3);
        assert_eq!(level(-7500), 2);
        assert_eq!(level(-9000), 1);
        assert_eq!(level(i16::MIN), 1);
        assert_eq!(strength(-6500), "Good");
    }

    #[test]
    fn the_list_is_known_first_then_by_level_then_by_name() {
        let listed: Vec<String> = model().listed().iter().map(|n| n.name.clone()).collect();
        assert_eq!(listed, ["Home", "Lab", "Old", "New", "Cafe"]);
    }

    #[test]
    fn one_row_per_name_and_security_and_a_bounded_list() {
        let mut many = vec![
            network("/a/1", "Mesh", Security::Passphrase, false, -8000),
            network("/a/2", "Mesh", Security::Passphrase, false, -5000),
            network("/a/3", "Mesh", Security::Open, false, -5000),
        ];
        let arranged = arrange(many.clone());
        assert_eq!(arranged.len(), 2);
        assert!(arranged.iter().any(|n| n.id == "/a/2"));
        for i in 0..500 {
            many.push(network(
                &format!("/b/{i}"),
                &format!("n{i}"),
                Security::Open,
                false,
                -6000,
            ));
        }
        assert_eq!(arrange(many).len(), MAX_NETWORKS);
    }

    #[test]
    fn the_connected_network_is_not_in_the_list() {
        let mut networks = sample();
        networks[1].connected = true;
        let mut model = Model::default();
        model.update(reading(networks), false);
        assert_eq!(model.connected().unwrap().name, "Home");
        assert!(model.listed().iter().all(|n| n.name != "Home"));
        assert!(model.can_forget("/n/home"));
        assert!(!model.can_forget("/n/cafe"));
        assert!(!model.can_forget("/n/nowhere"));
    }

    #[test]
    fn a_known_network_joins_at_once() {
        let mut model = model();
        let Step::Send(request) = model.choose("/n/home") else {
            panic!("expected a request");
        };
        assert_eq!(request.id, "/n/home");
        assert!(request.passphrase.is_none());
        assert_eq!(model.join.target(), Some("/n/home"));
        // Nothing else can be started while it runs.
        assert!(matches!(model.choose("/n/new"), Step::Nothing));
        model.join.finished("/n/home", Ok(()));
        assert_eq!(model.join, Join::Idle);
    }

    #[test]
    fn a_new_secured_network_asks_then_joins() {
        let mut model = model();
        assert!(matches!(model.choose("/n/new"), Step::AskPassphrase));
        // Too short: asked again, nothing sent.
        assert!(model.join.submit(secret("short")).is_none());
        assert!(matches!(
            model.join,
            Join::Asking {
                problem: Some(Problem::TooShort),
                ..
            }
        ));
        let request = model.join.submit(secret("correct horse")).unwrap();
        assert_eq!(request.passphrase.unwrap().expose(), "correct horse");
        model.join.finished("/n/new", Ok(()));
        assert_eq!(model.join, Join::Idle);
    }

    #[test]
    fn a_wrong_passphrase_asks_again_and_says_so() {
        let mut model = model();
        model.choose("/n/new");
        model.join.submit(secret("wrong password")).unwrap();
        model.join.finished("/n/new", Err(JoinError::Failed));
        assert_eq!(
            model.join,
            Join::Asking {
                id: "/n/new".to_string(),
                name: "New".to_string(),
                problem: Some(Problem::Rejected)
            }
        );
        // And the second try can succeed.
        assert!(model.join.submit(secret("right password")).is_some());
    }

    #[test]
    fn a_saved_passphrase_that_stopped_working_is_asked_for_anew() {
        let mut model = model();
        assert!(matches!(model.choose("/n/home"), Step::Send(_)));
        model
            .join
            .finished("/n/home", Err(JoinError::NeedsPassphrase));
        assert!(matches!(
            model.join,
            Join::Asking {
                problem: Some(Problem::Stale),
                ..
            }
        ));
    }

    #[test]
    fn an_open_network_is_only_joined_after_a_yes() {
        let mut model = model();
        assert!(matches!(model.choose("/n/cafe"), Step::ConfirmOpen));
        let mut declined = model.join.clone();
        declined.cancel();
        assert_eq!(declined, Join::Idle);
        // A yes to some other network's question is not a yes to this one.
        assert!(model.join.confirm("/n/home").is_none());
        let request = model.join.confirm("/n/cafe").unwrap();
        assert_eq!(request.id, "/n/cafe");
        assert!(request.passphrase.is_none());
        // A failure of an open network is not a passphrase problem.
        model.join.finished("/n/cafe", Err(JoinError::Failed));
        assert!(matches!(
            model.join,
            Join::Failed {
                error: JoinError::Failed,
                ..
            }
        ));
        // Nothing to confirm when nothing was asked.
        assert!(Join::Idle.confirm("/n/cafe").is_none());
    }

    #[test]
    fn networks_that_cannot_be_joined_say_why() {
        let mut model = model();
        assert!(matches!(model.choose("/n/lab"), Step::Explain));
        assert!(matches!(
            model.join,
            Join::Failed {
                error: JoinError::NotConfigured,
                ..
            }
        ));
        model.join.cancel();
        assert!(matches!(model.choose("/n/wep"), Step::Explain));
        assert!(matches!(model.choose("/n/nowhere"), Step::Nothing));
    }

    #[test]
    fn cancelling_does_not_pretend_a_running_join_is_over() {
        let mut model = model();
        model.choose("/n/home");
        model.join.cancel();
        assert!(matches!(model.join, Join::Joining { .. }));
        // The service reports the abort, and then it is over.
        model.join.finished("/n/home", Err(JoinError::Aborted));
        assert_eq!(model.join, Join::Idle);
    }

    #[test]
    fn the_end_of_another_join_is_ignored() {
        let mut model = model();
        model.choose("/n/home");
        model.join.finished("/n/other", Err(JoinError::Failed));
        assert!(matches!(model.join, Join::Joining { .. }));
        let mut idle = Join::Idle;
        idle.finished("/n/home", Ok(()));
        assert_eq!(idle, Join::Idle);
    }

    #[test]
    fn passphrase_rules() {
        assert_eq!(check_passphrase("12345678"), Ok(()));
        assert_eq!(check_passphrase("1234567"), Err(Problem::TooShort));
        assert_eq!(check_passphrase(&"x".repeat(63)), Ok(()));
        assert_eq!(check_passphrase(&"x".repeat(64)), Err(Problem::TooLong));
        assert_eq!(check_passphrase("pässword1"), Err(Problem::NotPlain));
        assert_eq!(check_passphrase("tab\tinside"), Err(Problem::NotPlain));
    }

    #[test]
    fn a_held_list_keeps_its_rows_and_marks_the_ones_that_left() {
        let mut model = model();
        let before: Vec<String> = model.listed().iter().map(|n| n.id.clone()).collect();
        // Cafe gets loud, Lab leaves, a new one appears.
        let mut next = sample();
        next[0].signal = -4000;
        next.remove(2);
        next.push(network("/n/zed", "Zed", Security::Open, false, -4000));
        model.update(reading(next), true);
        let held: Vec<String> = model.listed().iter().map(|n| n.id.clone()).collect();
        assert_eq!(&held[..before.len()], &before[..]);
        assert_eq!(held.last().unwrap(), "/n/zed");
        let lab = model.listed().iter().find(|n| n.id == "/n/lab").unwrap();
        assert!(lab.gone);
        // A row that has gone cannot be joined.
        assert!(matches!(model.choose("/n/lab"), Step::Nothing));
        model.release();
        assert!(model.listed().iter().all(|n| n.id != "/n/lab"));
        assert_eq!(model.listed()[0].name, "Home");
    }

    #[test]
    fn wifi_going_off_ends_whatever_was_being_asked() {
        let mut model = model();
        model.choose("/n/new");
        model.update(
            Reading {
                wifi: Wifi::Off,
                addresses: Vec::new(),
                hardware: true,
                block: Block::None,
                wired: Vec::new(),
            },
            true,
        );
        assert_eq!(model.join, Join::Idle);
        assert!(model.listed().is_empty());
        assert!(!model.switch_on());
    }

    #[test]
    fn the_switch_shows_what_was_asked_until_the_service_agrees() {
        let mut model = model();
        assert!(model.switch_on());
        model.want_power(false);
        assert!(!model.switch_on());
        // A reading from before the change does not flip it back.
        model.update(reading(sample()), false);
        assert!(!model.switch_on());
        // A refusal does.
        model.give_up_waiting();
        assert!(model.switch_on());
    }

    fn events(
        fake: &Arc<Fake>,
    ) -> (
        Handle<Command>,
        mpsc::Receiver<Event>,
        std::thread::JoinHandle<()>,
    ) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let (handle, inbox) = worker::channel();
        let thread = std::thread::spawn({
            let (service, handle) = (fake.clone(), handle.clone());
            move || {
                run(service, handle, inbox, move |event| {
                    let _ = tx.lock().unwrap().send(event);
                })
            }
        });
        (handle, rx, thread)
    }

    fn next_joined(rx: &mpsc::Receiver<Event>) -> Result<(), JoinError> {
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Joined { result, .. } => return result,
                _ => continue,
            }
        }
    }

    #[test]
    fn the_worker_reads_scans_and_joins() {
        let fake = Fake::with(sample());
        fake.state.lock().unwrap().accepts = vec![("/n/new".into(), "right password".into())];
        let (handle, rx, thread) = events(&fake);
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Reading(_)
        ));
        handle.send(Command::Join(Request {
            id: "/n/new".into(),
            passphrase: Some(secret("wrong password")),
        }));
        assert_eq!(next_joined(&rx), Err(JoinError::Failed));
        handle.send(Command::Join(Request {
            id: "/n/new".into(),
            passphrase: Some(secret("right password")),
        }));
        assert_eq!(next_joined(&rx), Ok(()));
        handle.send(Command::Forget("/n/new".into()));
        handle.send(Command::Disconnect);
        handle.stop();
        thread.join().unwrap();
        assert_eq!(
            fake.log(),
            [
                "scan",
                "join /n/new typed=true",
                "join /n/new typed=true",
                "forget /n/new",
                "disconnect"
            ]
        );
        // The passphrase is in none of what was recorded.
        assert!(fake.log().iter().all(|line| !line.contains("password")));
    }

    #[test]
    fn the_worker_reports_a_refusal_and_does_not_scan_when_off() {
        let fake = Fake::with(sample());
        {
            let mut state = fake.state.lock().unwrap();
            state.powered = false;
            state.deny = true;
        }
        let (handle, rx, thread) = events(&fake);
        handle.send(Command::Power(true));
        handle.stop();
        thread.join().unwrap();
        let all: Vec<Event> = rx.try_iter().collect();
        assert!(all.contains(&Event::Refused {
            what: "switch Wi-Fi",
            refusal: Refusal::Denied
        }));
        assert!(fake.log().is_empty());
    }

    #[test]
    fn a_change_outside_makes_the_worker_read_again() {
        let fake = Fake::with(sample());
        let (handle, rx, thread) = events(&fake);
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        fake.state.lock().unwrap().networks.clear();
        let notify = fake.state.lock().unwrap().notify.take().unwrap();
        notify();
        let Event::Reading(reading) = rx.recv_timeout(Duration::from_secs(5)).unwrap() else {
            panic!("expected a reading");
        };
        assert!(matches!(reading.wifi, Wifi::On { networks, .. } if networks.is_empty()));
        handle.stop();
        thread.join().unwrap();
    }
}
