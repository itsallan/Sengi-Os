//! The host, driven by hand: events in, and a record of what it asked for.

use super::*;
use crate::tray::pace::{BASE_INTERVAL, BURST, MAX_INTERVAL, READS_PER_MINUTE};
use crate::tray::watcher::{Claim, MAX_PER_SENDER};

const KDE: &str = "org.kde.StatusNotifierItem";
const ITEM: &str = "/StatusNotifierItem";

/// Writes down what the host asks for.
#[derive(Default)]
struct Recorder {
    log: Vec<String>,
    views: Vec<View>,
    publishes: usize,
    calls: Vec<Call>,
    reads: Vec<Refresh>,
    menus: Vec<MenuRequest>,
    shown: Option<Vec<Entry>>,
}

impl Recorder {
    fn take(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }
}

fn names(parts: Parts) -> String {
    if parts == Parts::ALL {
        return "all".into();
    }
    [
        (Parts::TITLE, "title"),
        (Parts::TOOLTIP, "tooltip"),
        (Parts::STATUS, "status"),
        (Parts::ICON, "icon"),
        (Parts::ATTENTION_ICON, "attention"),
        (Parts::OVERLAY_ICON, "overlay"),
        (Parts::IDENTITY, "identity"),
    ]
    .iter()
    .filter(|(part, _)| parts.contains(*part))
    .map(|(_, name)| *name)
    .collect::<Vec<_>>()
    .join("+")
}

impl Backend for Recorder {
    fn watch(&mut self, address: &Address) {
        self.log.push(format!("watch {}", address.service()));
    }
    fn unwatch(&mut self, address: &Address) {
        self.log.push(format!("unwatch {}", address.service()));
    }
    fn watch_owner(&mut self, name: &str) {
        self.log.push(format!("watch owner {name}"));
    }
    fn unwatch_owner(&mut self, name: &str) {
        self.log.push(format!("unwatch owner {name}"));
    }
    fn refresh(&mut self, request: Refresh) {
        self.log.push(format!(
            "read {} of {}",
            names(request.parts),
            request.address.service()
        ));
        self.reads.push(request);
    }
    fn call(&mut self, call: Call) {
        self.log.push(format!("call {:?}", call.method));
        self.calls.push(call);
    }
    fn unregistered(&mut self, address: &Address) {
        self.log.push(format!("unregistered {}", address.service()));
    }
    fn adopt_watcher(&mut self) {
        self.log.push("adopt".into());
    }
    fn publish(&mut self, views: Vec<View>) {
        let labels: Vec<String> = views.iter().map(|view| view.label.clone()).collect();
        self.log.push(format!("publish [{}]", labels.join(", ")));
        self.publishes += 1;
        self.views = views;
    }
    fn watch_menu(&mut self, bus: &str, path: &str) {
        self.log.push(format!("watch menu {bus}{path}"));
    }
    fn unwatch_menu(&mut self, bus: &str, path: &str) {
        self.log.push(format!("unwatch menu {bus}{path}"));
    }
    fn menu(&mut self, request: MenuRequest) {
        self.log.push(format!("menu {:?}", request.action));
        self.menus.push(request);
    }
    fn show_menu(&mut self, _serial: u64, place: &Place, entries: &[Entry]) {
        self.log
            .push(format!("show menu at {} {}", place.output, place.x));
        self.shown = Some(entries.to_vec());
    }
    fn update_menu(&mut self, entries: &[Entry]) {
        self.log.push("update menu".into());
        self.shown = Some(entries.to_vec());
    }
    fn close_menu(&mut self) {
        self.log.push("close menu".into());
        self.shown = None;
    }
}

struct Rig {
    host: Host<Recorder>,
    registry: Arc<Mutex<Registry>>,
    now: Instant,
}

fn rig() -> Rig {
    let registry = Arc::new(Mutex::new(Registry::default()));
    Rig {
        host: Host::new(Recorder::default(), registry.clone(), vec![16]),
        registry,
        now: Instant::now(),
    }
}

fn snapshot(title: &str) -> Snapshot {
    Snapshot {
        owner: ":1.50".into(),
        interface: KDE,
        properties: Properties {
            title: title.into(),
            ..Properties::default()
        },
        icons: Vec::new(),
        over: Refused::NONE,
    }
}

fn place() -> Place {
    Place {
        output: "eDP-1".into(),
        x: 900,
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

impl Rig {
    fn send(&mut self, event: Event) -> Vec<String> {
        self.host.handle(event, self.now);
        self.host.backend().take()
    }

    fn wait(&mut self, time: Duration) -> Vec<String> {
        self.now += time;
        self.host.tick(self.now);
        self.host.backend().take()
    }

    /// Registers the way the watcher does, then tells the host.
    fn register(&mut self, service: &str, sender: &str) -> (Address, Vec<String>) {
        let (Claim::Own(address) | Claim::Named(address)) =
            Address::claim(service, sender).unwrap();
        let address = self
            .registry
            .lock()
            .unwrap()
            .register(address, sender)
            .unwrap()
            .unwrap();
        let log = self.send(Event::Registered(address.clone()));
        (address, log)
    }

    fn answer(&mut self, address: &Address, snapshot: Snapshot) -> Vec<String> {
        self.send(Event::Refreshed {
            address: address.clone(),
            outcome: Ok(Box::new(snapshot)),
        })
    }

    fn fail(&mut self, address: &Address, failure: Failure) -> Vec<String> {
        self.send(Event::Refreshed {
            address: address.clone(),
            outcome: Err(failure),
        })
    }

    /// A registered item that has been read once, some time ago.
    fn item(&mut self, sender: &str, snapshot: Snapshot) -> (Address, u32) {
        let (address, _) = self.register(ITEM, sender);
        let snapshot = Snapshot {
            owner: sender.into(),
            ..snapshot
        };
        self.answer(&address, snapshot);
        // Long enough ago that the next change is read at once.
        self.now += Duration::from_secs(5);
        let id = self.host.items.last().unwrap().id;
        (address, id)
    }

    fn signal(&mut self, sender: &str, member: &str) -> Vec<String> {
        self.send(Event::Signal {
            sender: sender.into(),
            path: ITEM.into(),
            member: member.into(),
            menu: false,
        })
    }

    fn press(&mut self, id: u32, button: Button) -> Vec<String> {
        self.send(Event::Pressed {
            id,
            button,
            place: place(),
        })
    }

    fn labels(&mut self) -> Vec<String> {
        let views = &self.host.backend().views;
        views.iter().map(|view| view.label.clone()).collect()
    }
}

fn with_menu(title: &str, is_menu: bool) -> Snapshot {
    let mut snapshot = snapshot(title);
    snapshot.properties.menu = Some("/MenuBar".into());
    snapshot.properties.is_menu = is_menu;
    snapshot
}

fn row(id: i32, label: &str) -> Entry {
    Entry {
        id,
        label: label.into(),
        enabled: true,
        ..Entry::default()
    }
}

#[test]
fn a_registered_item_is_watched_read_and_shown() {
    let mut rig = rig();
    let (address, log) = rig.register(ITEM, ":1.50");
    assert_eq!(
        log,
        [
            "watch :1.50/StatusNotifierItem",
            "read all of :1.50/StatusNotifierItem"
        ]
    );
    // The first read has nothing to build on and asks for the one size.
    let read = &rig.host.backend().reads[0];
    assert_eq!(
        (read.previous.as_ref(), read.pixels.as_slice()),
        (None, &[16][..])
    );
    // Nothing is shown until it has answered.
    assert!(rig.host.backend().views.is_empty());
    assert_eq!(rig.answer(&address, snapshot("Chat")), ["publish [Chat]"]);
    let view = &rig.host.backend().views[0];
    assert_eq!((view.letter.as_str(), view.attention), ("C", false));
    assert_eq!(rig.host.deadline(), None);
}

#[test]
fn registering_again_reads_the_item_again_but_adds_nothing() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", snapshot("Chat"));
    let log = rig.send(Event::Registered(address.clone()));
    assert_eq!(log, ["read all of :1.50/StatusNotifierItem"]);
    assert_eq!(rig.host.items.len(), 1);
    // This time there is something to build on.
    assert!(rig.host.backend().reads[1].previous.is_some());
}

#[test]
fn new_items_go_on_the_left_so_the_others_stay_put() {
    let mut rig = rig();
    for (n, title) in ["One", "Two", "Three"].into_iter().enumerate() {
        rig.item(&format!(":1.{n}"), snapshot(title));
    }
    // Leftmost first: the oldest is nearest the clock and never moves.
    assert_eq!(rig.labels(), ["Three", "Two", "One"]);
}

#[test]
fn long_names_are_cut_for_the_label_beside_the_tray() {
    let mut rig = rig();
    rig.item(":1.50", snapshot(&"x".repeat(90)));
    let label = rig.labels().remove(0);
    assert_eq!(label.chars().count(), text::MAX_LABEL);
    assert!(label.ends_with('\u{2026}'));
}

#[test]
fn an_item_whose_owner_leaves_disappears() {
    let mut rig = rig();
    rig.item(":1.50", snapshot("Chat"));
    rig.item(":1.51", snapshot("Sync"));
    let log = rig.send(Event::OwnerLost(":1.50".into()));
    assert_eq!(
        log,
        [
            "unwatch :1.50/StatusNotifierItem",
            "unregistered :1.50/StatusNotifierItem",
            "publish [Sync]"
        ]
    );
    assert_eq!(rig.registry.lock().unwrap().len(), 1);
    // A name we never heard of is nothing to us.
    assert!(rig.send(Event::OwnerLost(":1.99".into())).is_empty());
}

#[test]
fn an_item_that_has_left_the_bus_is_dropped_when_a_read_finds_out() {
    let mut rig = rig();
    let (address, _) = rig.register(ITEM, ":1.50");
    let log = rig.fail(&address, Failure::Gone);
    assert_eq!(
        log,
        [
            "unwatch :1.50/StatusNotifierItem",
            "unregistered :1.50/StatusNotifierItem"
        ]
    );
    assert_eq!(rig.registry.lock().unwrap().len(), 0);
}

#[test]
fn an_item_that_keeps_timing_out_is_dropped() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", snapshot("Stuck"));
    for round in 1..=MAX_FAILURES {
        rig.now += Duration::from_secs(60);
        assert_eq!(rig.signal(":1.50", "NewIcon").len(), 1);
        let log = rig.fail(&address, Failure::Unreachable);
        if round < MAX_FAILURES {
            // Until then it keeps the look it had.
            assert!(log.is_empty(), "{log:?}");
            assert_eq!(rig.host.backend().views.len(), 1);
        } else {
            assert!(log.contains(&"publish []".to_string()));
        }
    }
    assert!(rig.host.items.is_empty());
}

#[test]
fn an_item_that_was_slow_to_answer_is_tried_again() {
    let mut rig = rig();
    // Still starting up: the first read times out.
    let (address, _) = rig.register(ITEM, ":1.50");
    assert!(rig.fail(&address, Failure::Unreachable).is_empty());
    assert_eq!(rig.host.deadline(), Some(rig.now + RETRY_DELAY));
    assert!(rig.wait(RETRY_DELAY / 2).is_empty());
    assert_eq!(
        rig.wait(RETRY_DELAY),
        ["read all of :1.50/StatusNotifierItem"]
    );
    // This time it answers, and that is the end of the retries.
    assert_eq!(rig.answer(&address, snapshot("Slow")), ["publish [Slow]"]);
    assert_eq!(rig.host.deadline(), None);
}

#[test]
fn a_partial_read_that_fails_is_asked_for_again() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", snapshot("Chat"));
    assert_eq!(
        rig.signal(":1.50", "NewToolTip"),
        ["read tooltip of :1.50/StatusNotifierItem"]
    );
    rig.fail(&address, Failure::Unreachable);
    // The retry reads what the failed read was after, not everything.
    assert_eq!(
        rig.wait(RETRY_DELAY),
        ["read tooltip of :1.50/StatusNotifierItem"]
    );
}

#[test]
fn a_signal_reads_only_what_it_names() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", snapshot("Chat"));
    for (member, parts) in [
        ("NewToolTip", "tooltip"),
        ("NewTitle", "title"),
        ("NewIcon", "icon"),
        ("NewStatus", "status"),
        ("NewAttentionIcon", "attention"),
        ("NewOverlayIcon", "overlay"),
        ("NewMenu", "all"),
    ] {
        let log = rig.signal(":1.50", member);
        assert_eq!(log, [format!("read {parts} of :1.50/StatusNotifierItem")]);
        rig.answer(&address, snapshot("Chat"));
        rig.now += Duration::from_secs(5);
    }
    // Signals that are not news, and a label the bar does not draw.
    assert!(rig.signal(":1.50", "SomethingElse").is_empty());
    assert!(rig.signal(":1.50", "XAyatanaNewLabel").is_empty());
}

#[test]
fn changes_that_arrive_together_are_read_together() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", snapshot("Chat"));
    assert_eq!(rig.signal(":1.50", "NewIcon").len(), 1);
    // While that read is under way, two more things change.
    assert!(rig.signal(":1.50", "NewToolTip").is_empty());
    assert!(rig.signal(":1.50", "NewTitle").is_empty());
    rig.answer(&address, snapshot("Chat"));
    assert_eq!(
        rig.wait(BASE_INTERVAL),
        ["read title+tooltip of :1.50/StatusNotifierItem"]
    );
}

#[test]
fn passive_items_are_hidden_and_attention_is_marked() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", snapshot("Chat"));
    let mut attention = snapshot("Chat");
    attention.properties.status = Status::NeedsAttention;
    rig.signal(":1.50", "NewStatus");
    assert_eq!(rig.answer(&address, attention), ["publish [Chat]"]);
    assert!(rig.host.backend().views[0].attention);

    rig.now += Duration::from_secs(5);
    let mut passive = snapshot("Chat");
    passive.properties.status = Status::Passive;
    rig.signal(":1.50", "NewStatus");
    assert_eq!(rig.answer(&address, passive), ["publish []"]);
}

#[test]
fn a_status_signal_changes_nothing_on_screen_by_itself() {
    let mut rig = rig();
    rig.item(":1.50", snapshot("Chat"));
    let before = rig.host.backend().publishes;
    // What the signal carries is not looked at; it is a reason to read.
    assert_eq!(
        rig.signal(":1.50", "NewStatus"),
        ["read status of :1.50/StatusNotifierItem"]
    );
    assert_eq!(rig.host.backend().publishes, before);
}

/// Sends one signal a millisecond for `millis`, answering every read
/// straight away with whatever `answer` makes of the read's number.
fn flood(
    rig: &mut Rig,
    member: &str,
    millis: u64,
    mut answer: impl FnMut(usize) -> Snapshot,
) -> usize {
    let mut reads = 0;
    for _ in 0..millis {
        rig.now += ms(1);
        rig.host.tick(rig.now);
        rig.host.handle(
            Event::Signal {
                sender: ":1.50".into(),
                path: ITEM.into(),
                member: member.into(),
                menu: false,
            },
            rig.now,
        );
        for line in rig.host.backend().take() {
            if !line.starts_with("read") {
                continue;
            }
            reads += 1;
            let address = rig.host.items[0].address.clone();
            let snapshot = answer(reads);
            rig.host.handle(
                Event::Refreshed {
                    address,
                    outcome: Ok(Box::new(snapshot)),
                },
                rig.now,
            );
        }
    }
    reads
}

#[test]
fn an_item_spamming_new_icon_is_read_at_a_bounded_rate() {
    let mut rig = rig();
    rig.item(":1.50", snapshot("Spam"));
    // Ten seconds of a signal every millisecond.
    let reads = flood(&mut rig, "NewIcon", 10_000, |_| snapshot("Spam"));
    assert!((3..=8).contains(&reads), "{reads} reads");
    // And it leaves a deadline only while a read is owed.
    assert!(rig.host.deadline().is_some());
    rig.wait(Duration::from_secs(20));
    let address = rig.host.items[0].address.clone();
    rig.answer(&address, snapshot("Spam"));
    assert_eq!(rig.host.deadline(), None);
}

#[test]
fn an_item_flapping_its_status_cannot_flood_the_bars() {
    let mut rig = rig();
    rig.item(":1.50", snapshot("Flap"));
    let before = rig.host.backend().publishes;
    // Ten thousand NewStatus in ten seconds, and the item really is
    // Passive on every other read, so each read does change the tray.
    let reads = flood(&mut rig, "NewStatus", 10_000, |read| {
        let mut snapshot = snapshot("Flap");
        if read % 2 == 1 {
            snapshot.properties.status = Status::Passive;
        }
        snapshot
    });
    let publishes = rig.host.backend().publishes - before;
    assert!(publishes <= reads, "{publishes} publishes, {reads} reads");
    assert!(publishes <= 8, "{publishes} publishes in ten seconds");
}

#[test]
fn the_interval_between_reads_runs_from_the_end_of_the_read() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", snapshot("Slow"));
    assert_eq!(rig.signal(":1.50", "NewIcon").len(), 1);
    rig.now += ms(100);
    assert!(rig.signal(":1.50", "NewIcon").is_empty());
    // The app takes 1.9 s to answer, just inside every timeout.
    rig.now += ms(1800);
    assert!(rig.answer(&address, snapshot("Slow")).is_empty());
    // It is not read again the moment it answers.
    assert_eq!(rig.host.deadline(), Some(rig.now + BASE_INTERVAL));
    assert!(rig.wait(BASE_INTERVAL - ms(1)).is_empty());
    assert_eq!(rig.wait(ms(1)).len(), 1);
}

#[test]
fn a_slow_item_is_given_longer_and_longer_between_reads() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", snapshot("Slow"));
    // It signals constantly and takes 1.9 s over every read. Count how
    // much of two minutes a worker spends on it.
    let (mut busy, mut until) = (Duration::ZERO, None::<Instant>);
    rig.host.backend().reads.clear();
    for _ in 0..120_000 {
        rig.now += ms(1);
        if until.is_some_and(|until| until <= rig.now) {
            until = None;
            rig.answer(&address, snapshot("Slow"));
        }
        rig.host.tick(rig.now);
        rig.signal(":1.50", "NewIcon");
        let started = std::mem::take(&mut rig.host.backend().reads).len();
        // Never a second read while the first is still out.
        assert!(started <= 1 && (started == 0 || until.is_none()));
        if started == 1 {
            until = Some(rig.now + ms(1900));
            busy += ms(1900);
        }
    }
    assert!(busy > MAX_INTERVAL, "{busy:?}");
    assert!(busy < Duration::from_secs(40), "{busy:?} of 120 s");
}

#[test]
fn an_icon_drawn_again_unchanged_is_still_the_same_icon() {
    let mut rig = rig();
    let pixels = |shade: u8| {
        Arc::new(Pixels {
            size: 16,
            data: vec![shade; 16 * 16 * 4],
        })
    };
    let with_icon = |shade: u8| Snapshot {
        icons: vec![(16, pixels(shade))],
        ..snapshot("Chat")
    };
    let (address, _) = rig.item(":1.50", with_icon(7));
    let first = rig.host.backend().views[0].icons[0].1.clone();

    // The app says its icon is new, and sends the same picture again.
    rig.signal(":1.50", "NewIcon");
    assert!(rig.answer(&address, with_icon(7)).is_empty());
    let held = rig.host.items[0].snapshot.as_ref().unwrap().icons[0]
        .1
        .clone();
    assert!(Arc::ptr_eq(&first, &held));

    // A picture that really is different replaces it.
    rig.now += Duration::from_secs(5);
    rig.signal(":1.50", "NewIcon");
    assert_eq!(rig.answer(&address, with_icon(9)), ["publish [Chat]"]);
    assert!(!Arc::ptr_eq(
        &first,
        &rig.host.backend().views[0].icons[0].1
    ));
}

#[test]
fn a_change_of_sizes_draws_every_icon_again() {
    let mut rig = rig();
    rig.item(":1.50", snapshot("One"));
    rig.item(":1.51", snapshot("Two"));
    let log = rig.send(Event::Pixels(vec![16, 32]));
    assert_eq!(
        log,
        [
            "read icon+attention+overlay of :1.50/StatusNotifierItem",
            "read icon+attention+overlay of :1.51/StatusNotifierItem"
        ]
    );
    assert_eq!(rig.host.backend().reads.last().unwrap().pixels, [16, 32]);
    assert!(rig.send(Event::Pixels(vec![16, 32])).is_empty());
}

#[test]
fn signals_are_matched_to_items_by_owner_and_path() {
    let mut rig = rig();
    // Registered under a well-known name, answered by :1.50.
    let (address, _) = rig.register("org.example.Chat", ":1.50");
    rig.answer(&address, snapshot("Chat"));
    rig.now += Duration::from_secs(5);
    assert_eq!(rig.signal(":1.50", "NewIcon").len(), 1);
    rig.answer(&address, snapshot("Chat"));
    rig.now += Duration::from_secs(5);
    // Someone else, or another object.
    assert!(rig.signal(":1.77", "NewIcon").is_empty());
    let stray = rig.send(Event::Signal {
        sender: ":1.50".into(),
        path: "/Other".into(),
        member: "NewIcon".into(),
        menu: false,
    });
    assert!(stray.is_empty());
}

#[test]
fn a_name_that_changes_hands_is_followed_to_its_new_owner() {
    let mut rig = rig();
    let (address, _) = rig.register("org.example.Chat", ":1.50");
    rig.answer(&address, snapshot("Chat"));
    rig.now += Duration::from_secs(5);
    let log = rig.send(Event::OwnerChanged {
        name: "org.example.Chat".into(),
        owner: ":1.77".into(),
    });
    // It may be another program entirely: everything is read again.
    assert_eq!(log, ["read all of org.example.Chat/StatusNotifierItem"]);
    let mut renewed = snapshot("Chat");
    renewed.owner = ":1.77".into();
    rig.answer(&address, renewed);
    rig.now += Duration::from_secs(5);
    // Signals from the new owner are heard, the old one's are not.
    assert_eq!(rig.signal(":1.77", "NewIcon").len(), 1);
    assert!(rig.signal(":1.50", "NewTitle").is_empty());
}

#[test]
fn a_deferred_read_starts_when_it_is_due() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", snapshot("Chat"));
    assert_eq!(rig.signal(":1.50", "NewIcon").len(), 1);
    rig.answer(&address, snapshot("Chat"));
    // A second change right behind the first has to wait.
    assert!(rig.signal(":1.50", "NewIcon").is_empty());
    assert_eq!(rig.host.deadline(), Some(rig.now + BASE_INTERVAL));
    assert!(rig.wait(ms(10)).is_empty());
    assert_eq!(
        rig.wait(BASE_INTERVAL),
        ["read icon of :1.50/StatusNotifierItem"]
    );
    assert_eq!(rig.host.deadline(), None);
}

/// A read that found these answers over their limit.
fn over(title: &str, what: Refused) -> Snapshot {
    Snapshot {
        over: what,
        ..snapshot(title)
    }
}

#[test]
fn an_oversized_icon_costs_the_icon_not_the_item() {
    let mut rig = rig();
    // An honest app whose only icon is one enormous pixmap.
    let (address, _) = rig.register(ITEM, ":1.50");
    let log = rig.answer(&address, over("Big", Refused::ICON_PIXMAP));
    assert_eq!(log, ["watch owner :1.50", "publish [Big]"]);
    // It is in the tray, with a letter for a picture.
    let view = &rig.host.backend().views[0];
    assert_eq!((view.letter.as_str(), view.icons.len()), ("B", 0));

    // From now on its reads are told not to fetch the pixmap.
    rig.now += Duration::from_secs(5);
    rig.signal(":1.50", "NewIcon");
    let read = rig.host.backend().reads.last().unwrap();
    assert!(read.refused.contains(Refused::ICON_PIXMAP));
    assert!(rig.answer(&address, snapshot("Big")).is_empty());
    assert_eq!(rig.host.items.len(), 1);
}

#[test]
fn a_second_oversized_answer_gets_the_connection_ignored_until_it_leaves() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", over("Greedy", Refused::ICON_PIXMAP));
    rig.item(":1.51", snapshot("Good"));
    rig.host.backend().take();
    rig.signal(":1.50", "NewToolTip");
    let log = rig.answer(&address, over("Greedy", Refused::TOOLTIP));
    assert_eq!(
        log,
        [
            "unwatch :1.50/StatusNotifierItem",
            "unregistered :1.50/StatusNotifierItem",
            "watch owner :1.50",
            "publish [Good]"
        ]
    );
    // It cannot come straight back.
    let again = Address::claim(ITEM, ":1.50").unwrap();
    let (Claim::Own(again) | Claim::Named(again)) = again;
    assert!(rig
        .registry
        .lock()
        .unwrap()
        .register(again, ":1.50")
        .is_err());
    // Once the name is gone from the bus it is forgiven, and both the
    // watch for the ban and the one for the refusal are taken down.
    assert_eq!(
        rig.send(Event::OwnerLost(":1.50".into())),
        ["unwatch owner :1.50", "unwatch owner :1.50"]
    );
    assert!(!rig.registry.lock().unwrap().is_banned(":1.50"));
    assert!(rig.host.refusals.is_empty());
}

#[test]
fn text_over_its_limit_is_let_off_once() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", over("", Refused::TEXT));
    assert_eq!(rig.host.items.len(), 1);
    // A title is asked for again when it changes; a second one that long
    // is a second offence.
    rig.signal(":1.50", "NewTitle");
    rig.answer(&address, over("", Refused::TEXT));
    assert!(rig.host.items.is_empty());
    assert!(rig.registry.lock().unwrap().is_banned(":1.50"));
}

#[test]
fn an_oversized_menu_costs_the_menu_not_the_item() {
    let mut rig = rig();
    let (_, id) = rig.item(":1.50", with_menu("Big menu", false));
    rig.press(id, Button::Right);
    let log = rig.send(Event::Oversized {
        owner: ":1.50".into(),
        what: Refused::MENU,
    });
    assert_eq!(
        log,
        [
            "watch owner :1.50",
            "menu Closed",
            "unwatch menu :1.50/MenuBar",
            "close menu"
        ]
    );
    // The item stays and still takes clicks; its menu is not asked for
    // again, so a right click is passed to the app to deal with.
    assert_eq!(rig.labels(), ["Big menu"]);
    assert_eq!(
        rig.press(id, Button::Right),
        [format!("call {:?}", Method::ContextMenu)]
    );
    assert_eq!(rig.press(id, Button::Left).len(), 1);
    // Sending another one is the second offence.
    rig.send(Event::Oversized {
        owner: ":1.50".into(),
        what: Refused::MENU,
    });
    assert!(rig.host.items.is_empty());
}

#[test]
fn megabytes_nobody_asked_for_get_the_connection_ignored_at_once() {
    let mut rig = rig();
    rig.item(":1.50", with_menu("Greedy", false));
    let log = rig.send(Event::Misbehaved {
        owner: ":1.50".into(),
    });
    assert!(log.contains(&"watch owner :1.50".to_string()), "{log:?}");
    assert!(rig.host.items.is_empty());
}

#[test]
fn a_ban_reaches_an_item_adopted_under_a_well_known_name() {
    let mut rig = rig();
    // A guest of another watcher: the item is known by the name it is
    // listed under, and its reads are answered by :1.50.
    rig.send(Event::WatcherOwner { ours: false });
    rig.send(Event::Announced(
        "org.example.App/StatusNotifierItem".into(),
    ));
    let address = rig.host.items[0].address.clone();
    rig.answer(&address, snapshot("App"));
    let log = rig.send(Event::Misbehaved {
        owner: ":1.50".into(),
    });
    assert!(rig.host.items.is_empty(), "{log:?}");
    // Both names are held against it, so it is not asked again on its
    // next signal or adopted again when the watcher lists it.
    assert!(log.contains(&"watch owner :1.50".to_string()));
    assert!(log.contains(&"watch owner org.example.App".to_string()));
    assert!(rig
        .send(Event::Announced(
            "org.example.App/StatusNotifierItem".into()
        ))
        .is_empty());
    assert!(rig.signal(":1.50", "NewIcon").is_empty());
}

#[test]
fn a_ban_pushed_off_the_end_of_the_list_is_no_longer_watched() {
    let mut rig = rig();
    let mut log = Vec::new();
    for n in 0..70 {
        log.extend(rig.send(Event::Misbehaved {
            owner: format!(":1.{n}"),
        }));
    }
    let count = |what: &str| log.iter().filter(|line| line.starts_with(what)).count();
    // Seventy were watched for; the six that fell off are let go.
    assert_eq!(count("watch owner"), 70);
    assert_eq!(count("unwatch owner"), 6);
    assert!(log.contains(&"unwatch owner :1.0".to_string()));
}

#[test]
fn an_item_appears_and_disappears_no_faster_than_the_flip_interval() {
    let mut rig = rig();
    rig.item(":1.49", snapshot("Left of it"));
    let (address, _) = rig.item(":1.50", snapshot("Flapper"));
    rig.host.backend().take();
    let before = rig.host.backend().publishes;
    let status = |status: Status| {
        let mut snapshot = snapshot("Flapper");
        snapshot.properties.status = status;
        snapshot
    };
    // Passive and Active by turns every 300 ms for a minute. Each read
    // finds the other status.
    let mut reads = 0;
    for step in 0..200 {
        rig.now += ms(300);
        rig.host.tick(rig.now);
        rig.signal(":1.50", "NewStatus");
        if rig.host.backend().reads.len() > reads {
            reads = rig.host.backend().reads.len();
            let next = if step % 2 == 0 {
                Status::Passive
            } else {
                Status::Active
            };
            rig.answer(&address, status(next));
        }
    }
    // The tray changed at most once per flip interval, not once per read.
    let flips = rig.host.backend().publishes - before;
    assert!(
        flips <= 60 / FLIP_INTERVAL.as_secs() as usize + 1,
        "{flips}"
    );
    assert!(flips >= 2, "{flips}");
}

#[test]
fn a_held_status_is_shown_once_the_interval_is_up() {
    let mut rig = rig();
    let (address, _) = rig.register(ITEM, ":1.50");
    rig.answer(&address, snapshot("Chat"));
    assert_eq!(rig.labels(), ["Chat"]);
    // It hides straight after appearing: held for now.
    rig.now += Duration::from_secs(1);
    rig.signal(":1.50", "NewStatus");
    let mut passive = snapshot("Chat");
    passive.properties.status = Status::Passive;
    assert!(rig.answer(&address, passive).is_empty());
    assert_eq!(rig.labels(), ["Chat"]);
    // And goes when the time is up, with nothing more said by the app.
    assert_eq!(rig.host.deadline(), Some(rig.now + Duration::from_secs(4)));
    assert_eq!(rig.wait(Duration::from_secs(4)), ["publish []"]);
    assert_eq!(rig.host.deadline(), None);
}

#[test]
fn an_item_changing_just_slower_than_the_spacing_has_a_minutes_allowance() {
    let mut rig = rig();
    let (address, _) = rig.item(":1.50", snapshot("Animated"));
    rig.host.backend().reads.clear();
    // A fresh icon every 300 ms for three minutes, each read answered at
    // once.
    for _ in 0..600 {
        rig.now += ms(300);
        rig.host.tick(rig.now);
        rig.signal(":1.50", "NewIcon");
        if rig.host.items[0].pacer.in_flight() {
            rig.answer(&address, snapshot("Animated"));
        }
    }
    let reads = rig.host.backend().reads.len();
    assert!(
        reads <= (3 * READS_PER_MINUTE + BURST) as usize,
        "{reads} reads"
    );
    assert!(reads >= READS_PER_MINUTE as usize, "{reads} reads");
}

#[test]
fn one_connection_holds_no_more_than_its_share_of_the_tray() {
    let mut rig = rig();
    for n in 0..MAX_PER_SENDER {
        rig.register(&format!("/item/{n}"), ":1.50");
    }
    let more = Address::claim("/item/more", ":1.50").unwrap();
    let (Claim::Own(more) | Claim::Named(more)) = more;
    assert!(rig
        .registry
        .lock()
        .unwrap()
        .register(more, ":1.50")
        .is_err());
    assert_eq!(rig.host.items.len(), MAX_PER_SENDER);
}

#[test]
fn clicks_become_calls_on_the_item() {
    let mut rig = rig();
    let (_, id) = rig.item(":1.50", snapshot("Chat"));
    rig.press(id, Button::Left);
    rig.press(id, Button::Middle);
    // No menu of its own: the app is asked to show one.
    rig.press(id, Button::Right);
    let calls = &rig.host.backend().calls;
    let methods: Vec<_> = calls.iter().map(|call| call.method.clone()).collect();
    assert_eq!(
        methods,
        [
            Method::Activate(place()),
            Method::SecondaryActivate,
            Method::ContextMenu,
        ]
    );
    assert_eq!(calls[0].bus, ":1.50");
    assert_eq!(calls[0].path, ITEM);
    assert_eq!(calls[0].interface, KDE);
    // A click on something that is not there does nothing.
    assert!(rig.press(999, Button::Left).is_empty());
}

#[test]
fn an_app_that_does_not_answer_gets_one_activate_not_one_per_click() {
    let mut rig = rig();
    let (_, hung) = rig.item(":1.50", snapshot("Hung"));
    let (_, well) = rig.item(":1.51", snapshot("Well"));
    assert_eq!(rig.press(hung, Button::Left).len(), 1);
    for _ in 0..50 {
        assert!(rig.press(hung, Button::Left).is_empty());
    }
    // Its neighbour is not held up by it.
    assert_eq!(rig.press(well, Button::Left).len(), 1);
    // When the call does come back, the next click goes through.
    rig.send(Event::Activated {
        id: hung,
        fallback: None,
    });
    assert_eq!(rig.press(hung, Button::Left).len(), 1);
}

fn scrolls(rig: &mut Rig) -> Vec<(i32, bool)> {
    let calls = std::mem::take(&mut rig.host.backend().calls);
    calls
        .into_iter()
        .filter_map(|call| match call.method {
            Method::Scroll { delta, horizontal } => Some((delta, horizontal)),
            _ => None,
        })
        .collect()
}

#[test]
fn scrolling_is_added_up_and_sent_at_a_bounded_rate() {
    let mut rig = rig();
    let (_, id) = rig.item(":1.50", snapshot("Volume"));
    let scroll = |rig: &mut Rig, delta: f32, horizontal: bool| {
        rig.send(Event::Scrolled {
            id,
            delta,
            horizontal,
        });
    };
    // A wheel notch goes out at once.
    scroll(&mut rig, 120.0, false);
    assert_eq!(scrolls(&mut rig), [(120, false)]);
    // A burst right behind it is held and sent as one call.
    for _ in 0..30 {
        scroll(&mut rig, -7.5, false);
        scroll(&mut rig, 2.0, true);
    }
    assert!(scrolls(&mut rig).is_empty());
    assert_eq!(rig.host.deadline(), Some(rig.now + SCROLL_INTERVAL));
    rig.wait(SCROLL_INTERVAL);
    assert_eq!(scrolls(&mut rig), [(-225, false), (60, true)]);
    assert_eq!(rig.host.deadline(), None);
}

#[test]
fn small_movements_add_up_instead_of_rounding_away() {
    let mut rig = rig();
    let (_, id) = rig.item(":1.50", snapshot("Volume"));
    let mut sent = 0;
    // A slow touchpad drag: 400 steps of a third of a unit.
    for _ in 0..400 {
        rig.now += ms(20);
        rig.host.tick(rig.now);
        rig.send(Event::Scrolled {
            id,
            delta: 1.0 / 3.0,
            horizontal: false,
        });
        sent += scrolls(&mut rig)
            .iter()
            .map(|(delta, _)| delta)
            .sum::<i32>();
    }
    assert!((132..=134).contains(&sent), "{sent}");
    // Nonsense is not added to the pile.
    rig.send(Event::Scrolled {
        id,
        delta: f32::NAN,
        horizontal: false,
    });
    rig.send(Event::Scrolled {
        id,
        delta: f32::INFINITY,
        horizontal: false,
    });
    rig.wait(Duration::from_secs(1));
    assert!(scrolls(&mut rig).iter().all(|(delta, _)| delta.abs() <= 1));
}

#[test]
fn a_right_click_fetches_and_shows_the_menu() {
    let mut rig = rig();
    let (_, id) = rig.item(":1.50", with_menu("Chat", false));
    assert_eq!(
        rig.press(id, Button::Right),
        ["watch menu :1.50/MenuBar", "menu Show(0)"]
    );
    let serial = rig.host.backend().menus[0].serial;
    let entries = vec![row(1, "Open"), row(2, "Quit")];
    let log = rig.send(Event::MenuLayout {
        serial,
        entries: Some(entries.clone()),
    });
    assert_eq!(log, ["show menu at eDP-1 900"]);
    assert_eq!(rig.host.backend().shown, Some(entries));

    // Choosing a row tells the app, then that the menu has closed.
    let log = rig.send(Event::MenuClicked { serial, id: 2 });
    assert_eq!(
        log,
        [
            "menu Click(2)",
            "menu Closed",
            "unwatch menu :1.50/MenuBar",
            "close menu"
        ]
    );
    // The helper exiting afterwards is old news.
    assert!(rig.send(Event::MenuClosed { serial }).is_empty());
}

#[test]
fn a_left_click_opens_the_menu_of_an_item_that_is_only_a_menu() {
    let mut rig = rig();
    let (_, id) = rig.item(":1.50", with_menu("Indicator", true));
    assert_eq!(rig.press(id, Button::Left)[1], "menu Show(0)");
    rig.send(Event::MenuClosed { serial: 1 });

    // And of one that turns out to have no Activate.
    let (_, id) = rig.item(":1.51", with_menu("Appindicator", false));
    assert_eq!(rig.press(id, Button::Left).len(), 1);
    let log = rig.send(Event::Activated {
        id,
        fallback: Some(place()),
    });
    assert_eq!(log, ["watch menu :1.51/MenuBar", "menu Show(0)"]);
}

#[test]
fn only_rows_that_were_shown_and_can_be_chosen_are_clicked() {
    let mut rig = rig();
    let (_, id) = rig.item(":1.50", with_menu("Chat", false));
    let open = |rig: &mut Rig| {
        rig.press(id, Button::Right);
        let serial = rig.host.backend().menus.last().unwrap().serial;
        let disabled = Entry {
            enabled: false,
            ..row(2, "Greyed")
        };
        let sub = Entry {
            submenu: true,
            children: vec![row(4, "Inner")],
            ..row(3, "More")
        };
        rig.send(Event::MenuLayout {
            serial,
            entries: Some(vec![row(1, "Open"), disabled, sub]),
        });
        serial
    };
    for (clicked, expected) in [(2, false), (3, false), (99, false), (4, true), (1, true)] {
        let serial = open(&mut rig);
        let log = rig.send(Event::MenuClicked {
            serial,
            id: clicked,
        });
        assert_eq!(
            log.contains(&format!("menu Click({clicked})")),
            expected,
            "row {clicked}"
        );
        assert!(log.contains(&"close menu".to_string()));
    }
    // A click from a menu that is no longer the current one is ignored.
    let stale = open(&mut rig);
    open(&mut rig);
    assert!(rig
        .send(Event::MenuClicked {
            serial: stale,
            id: 1
        })
        .is_empty());
}

#[test]
fn opening_a_submenu_asks_the_app_to_fill_it() {
    let mut rig = rig();
    let (_, id) = rig.item(":1.50", with_menu("Chat", false));
    rig.press(id, Button::Right);
    let serial = rig.host.backend().menus[0].serial;
    let sub = Entry {
        submenu: true,
        ..row(3, "More")
    };
    rig.send(Event::MenuLayout {
        serial,
        entries: Some(vec![row(1, "Open"), sub.clone()]),
    });
    // Not a submenu, or not a row at all.
    assert!(rig.send(Event::MenuOpened { serial, id: 1 }).is_empty());
    assert!(rig.send(Event::MenuOpened { serial, id: 77 }).is_empty());
    // The request waits its turn behind the read that has just ended.
    rig.now += Duration::from_secs(1);
    assert_eq!(
        rig.send(Event::MenuOpened { serial, id: 3 }),
        ["menu Show(3)"]
    );

    let filled = Entry {
        children: vec![row(4, "Inner")],
        ..sub
    };
    let log = rig.send(Event::MenuLayout {
        serial,
        entries: Some(vec![row(1, "Open"), filled]),
    });
    assert_eq!(log, ["update menu"]);
}

#[test]
fn a_menu_has_one_request_under_way_at_a_time() {
    let mut rig = rig();
    let (_, id) = rig.item(":1.50", with_menu("Dead", false));
    rig.press(id, Button::Right);
    let serial = rig.host.backend().menus[0].serial;
    let sub = |id| Entry {
        submenu: true,
        ..row(id, "More")
    };
    rig.send(Event::MenuLayout {
        serial,
        entries: Some(vec![sub(3), sub(4)]),
    });
    rig.now += Duration::from_secs(1);
    assert_eq!(rig.send(Event::MenuOpened { serial, id: 3 }).len(), 1);
    // The app has stopped answering. Hovering back and forth and a flood
    // of change signals queue nothing more behind the request.
    for n in 0..100 {
        let id = 3 + n % 2;
        assert!(rig.send(Event::MenuOpened { serial, id }).is_empty());
        let changed = rig.send(Event::Signal {
            sender: ":1.50".into(),
            path: "/MenuBar".into(),
            member: "LayoutUpdated".into(),
            menu: true,
        });
        assert!(changed.is_empty());
    }
    // When it does answer, the row asked about last is asked about once.
    rig.send(Event::MenuLayout {
        serial,
        entries: Some(vec![sub(3), sub(4)]),
    });
    assert_eq!(rig.wait(BASE_INTERVAL), ["menu Show(4)"]);
}

#[test]
fn a_menu_that_changes_while_open_is_read_again_at_a_bounded_rate() {
    let mut rig = rig();
    let (_, id) = rig.item(":1.50", with_menu("Chat", false));
    rig.press(id, Button::Right);
    let serial = rig.host.backend().menus[0].serial;
    rig.send(Event::MenuLayout {
        serial,
        entries: Some(vec![row(1, "Open")]),
    });
    let changed = |rig: &mut Rig, sender: &str, path: &str| {
        rig.send(Event::Signal {
            sender: sender.into(),
            path: path.into(),
            member: "LayoutUpdated".into(),
            menu: true,
        })
    };
    // Too soon after the first read: put off, not dropped.
    assert!(changed(&mut rig, ":1.50", "/MenuBar").is_empty());
    assert!(changed(&mut rig, ":1.50", "/MenuBar").is_empty());
    assert_eq!(rig.wait(BASE_INTERVAL), ["menu Reload"]);
    // The same layout again is not sent to the screen twice.
    let log = rig.send(Event::MenuLayout {
        serial,
        entries: Some(vec![row(1, "Open")]),
    });
    assert!(log.is_empty());
    // Another app's menu, or another menu of this app, is not ours.
    rig.now += Duration::from_secs(5);
    assert!(changed(&mut rig, ":1.77", "/MenuBar").is_empty());
    assert!(changed(&mut rig, ":1.50", "/Other").is_empty());
    assert_eq!(changed(&mut rig, ":1.50", "/MenuBar"), ["menu Reload"]);
}

#[test]
fn an_empty_or_unreadable_menu_is_not_shown() {
    let mut rig = rig();
    let (_, id) = rig.item(":1.50", with_menu("Chat", false));
    for entries in [None, Some(Vec::new())] {
        rig.press(id, Button::Right);
        let serial = rig.host.backend().menus.last().unwrap().serial;
        let log = rig.send(Event::MenuLayout { serial, entries });
        assert_eq!(
            log,
            ["menu Closed", "unwatch menu :1.50/MenuBar", "close menu"]
        );
    }
}

#[test]
fn the_menu_goes_when_its_item_does_or_another_is_opened() {
    let mut rig = rig();
    let (_, first) = rig.item(":1.50", with_menu("One", false));
    let (_, second) = rig.item(":1.51", with_menu("Two", false));
    rig.press(first, Button::Right);
    let log = rig.press(second, Button::Right);
    assert_eq!(
        log,
        [
            "menu Closed",
            "unwatch menu :1.50/MenuBar",
            "close menu",
            "watch menu :1.51/MenuBar",
            "menu Show(0)"
        ]
    );
    let log = rig.send(Event::OwnerLost(":1.51".into()));
    assert!(log.contains(&"close menu".to_string()), "{log:?}");
}

#[test]
fn items_that_do_not_fit_are_reached_through_a_menu_of_their_names() {
    let mut rig = rig();
    let (_, plain) = rig.item(":1.50", snapshot("Plain"));
    let (_, indicator) = rig.item(":1.51", with_menu("Indicator", false));
    let (_, shown) = rig.item(":1.52", snapshot("Shown"));
    let more = |rig: &mut Rig| {
        rig.send(Event::More {
            place: place(),
            hidden: vec![plain, indicator, 999],
        })
    };
    assert_eq!(more(&mut rig), ["show menu at eDP-1 900"]);
    let listed = rig.host.backend().shown.clone().unwrap();
    let labels: Vec<_> = listed.iter().map(|entry| entry.label.as_str()).collect();
    assert_eq!(labels, ["Plain", "Indicator"]);
    let serial = rig.host.next_serial - 1;

    // Choosing one with a menu opens that menu where the list was.
    let log = rig.send(Event::MenuClicked {
        serial,
        id: indicator as i32,
    });
    assert_eq!(
        log,
        ["close menu", "watch menu :1.51/MenuBar", "menu Show(0)"]
    );
    // Choosing one without does what a click on it would.
    more(&mut rig);
    let serial = rig.host.next_serial - 1;
    let log = rig.send(Event::MenuClicked {
        serial,
        id: plain as i32,
    });
    assert_eq!(
        log.last().unwrap(),
        &format!("call {:?}", Method::Activate(place()))
    );
    // Only what was listed can be chosen from it.
    more(&mut rig);
    let serial = rig.host.next_serial - 1;
    let log = rig.send(Event::MenuClicked {
        serial,
        id: shown as i32,
    });
    assert_eq!(log, ["close menu"]);
    // With nothing hidden there is no menu.
    assert!(rig
        .send(Event::More {
            place: place(),
            hidden: Vec::new()
        })
        .is_empty());
}

#[test]
fn with_another_watcher_its_items_are_adopted() {
    let mut rig = rig();
    assert_eq!(rig.send(Event::WatcherOwner { ours: false }), ["adopt"]);
    let log = rig.send(Event::Announced(":1.50/StatusNotifierItem".into()));
    assert_eq!(
        log,
        [
            "watch :1.50/StatusNotifierItem",
            "read all of :1.50/StatusNotifierItem"
        ]
    );
    // Listed again, or nonsense.
    assert!(rig
        .send(Event::Announced(":1.50/StatusNotifierItem".into()))
        .is_empty());
    assert!(rig
        .send(Event::Announced("not a service".into()))
        .is_empty());

    // It is the other watcher's place to announce that one has left.
    let log = rig.send(Event::Withdrawn(":1.50/StatusNotifierItem".into()));
    assert_eq!(log, ["unwatch :1.50/StatusNotifierItem"]);
    assert_eq!(rig.registry.lock().unwrap().len(), 0);
}

#[test]
fn taking_over_the_watcher_name_keeps_the_items() {
    let mut rig = rig();
    rig.send(Event::WatcherOwner { ours: false });
    rig.send(Event::Announced(":1.50/StatusNotifierItem".into()));
    assert!(rig.send(Event::WatcherOwner { ours: true }).is_empty());
    assert_eq!(rig.host.items.len(), 1);
    assert_eq!(
        rig.registry.lock().unwrap().services(),
        [":1.50/StatusNotifierItem"]
    );
    // From here on announcements from elsewhere are not believed.
    assert!(rig
        .send(Event::Announced(":1.51/StatusNotifierItem".into()))
        .is_empty());
    assert!(rig
        .send(Event::Withdrawn(":1.50/StatusNotifierItem".into()))
        .is_empty());
}
