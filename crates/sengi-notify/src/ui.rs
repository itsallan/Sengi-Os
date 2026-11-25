//! The UI thread: keeps the Slint window in step with the [`Store`], moves
//! the layer surface between its three states (hidden, a strip of cards,
//! the full-output notification centre) and runs the one expiry timer.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sengi_layer::{Anchor, Handle, KeyboardMode, Rect};
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};

use crate::cli::{Command, Dnd};
use crate::dbus::{Signal, ToUi};
use crate::history::{self, Line};
use crate::notification::{Urgency, DEFAULT_ACTION};
use crate::store::{Closed, Entry, Limits, Popup, Reason, Store};

slint::include_modules!();

/// How long the history file waits for more changes before it is written,
/// so that a burst of notifications costs one write.
const SAVE_DELAY: Duration = Duration::from_secs(2);

/// How often "2 min ago" is brought up to date while the centre is open.
const AGO_EVERY: Duration = Duration::from_secs(30);

/// How long the countdowns stay stopped after the pointer has left the
/// cards, so that brushing past the edge does not cost the card its time.
const HOVER_GRACE: Duration = Duration::from_secs(1);

/// Cards that may be fading out at once. Any beyond that are removed on
/// the spot, so a client that opens and closes notifications as fast as
/// it can does not get a strip full of ghosts.
const LEAVING_MAX: usize = 2;

/// A card in the Slint list. `rows` runs parallel to the `cards` model.
struct Row {
    serial: i32,
    generation: u64,
    id: u32,
    version: u64,
    leaving: bool,
}

/// What the layer surface was last asked to be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Hidden,
    Strip { width: u32, height: u32 },
    Centre,
}

pub struct Ui {
    window: NotifyWindow,
    handle: Handle,
    store: Store,
    cards: Rc<VecModel<Card>>,
    rows: Vec<Row>,
    history: Rc<VecModel<HistoryItem>>,
    signals: mpsc::Sender<Signal>,
    expiry: Timer,
    grace: Timer,
    /// Writes the history file a moment after the last change.
    save: Timer,
    /// Refreshes the times in the centre while it is open.
    ago: Timer,
    /// Where the history is kept between runs, if anywhere.
    file: Option<std::path::PathBuf>,
    saved: u64,
    started: Instant,
    last_serial: i32,
    centre: bool,
    hover: bool,
    shape: Shape,
    /// The height of the output, once known. The strip is never taller.
    tallest: Option<u32>,
    /// Until when clicks on the cards, and on the centre, are ignored
    /// because what is under the pointer has just changed.
    cards_guard: u64,
    centre_guard: u64,
    /// The do-not-disturb state and unread count last put on the bus.
    announced: (bool, u32),
    this: Weak<RefCell<Ui>>,
}

fn card(popup: &Popup, serial: i32, leaving: bool) -> Card {
    let note = &popup.note;
    let actions: Vec<ActionItem> = note
        .actions
        .iter()
        .map(|action| ActionItem {
            key: action.key.as_str().into(),
            label: action.label.as_str().into(),
        })
        .collect();
    Card {
        serial,
        app: note.app.as_str().into(),
        time: note.time.as_str().into(),
        summary: note.summary.as_str().into(),
        body: note.body.as_str().into(),
        critical: note.urgency == Urgency::Critical,
        actions: ModelRc::new(VecModel::from(actions)),
        leaving,
    }
}

/// The lines of the centre for these history entries, newest first, as of
/// `now` in seconds since the epoch: a heading for each application, and
/// its notifications under it.
fn history_items(entries: &[&Entry], now: u64) -> Vec<HistoryItem> {
    let lines = history::group(entries.iter().map(|entry| entry.note.app.as_str()));
    lines
        .iter()
        .map(|line| match *line {
            Line::Group { first, count } => {
                let app = entries[first].note.app.as_str();
                HistoryItem {
                    key: -1,
                    header: true,
                    count: count as i32,
                    group: app.into(),
                    app: if app.is_empty() { "Other" } else { app }.into(),
                    ..HistoryItem::default()
                }
            }
            Line::Entry(index) => {
                let note = &entries[index].note;
                HistoryItem {
                    key: entries[index].key as i32,
                    app: note.app.as_str().into(),
                    time: if note.at == 0 {
                        note.time.clone()
                    } else {
                        history::relative(now, note.at)
                    }
                    .into(),
                    summary: note.summary.as_str().into(),
                    body: note.body.as_str().into(),
                    critical: note.urgency == Urgency::Critical,
                    ..HistoryItem::default()
                }
            }
        })
        .collect()
}

/// Whether a click at `now` falls inside a guard that lasts until `until`.
fn guarded(now: u64, until: u64) -> bool {
    now < until
}

impl Ui {
    /// Wires `window` up. The surface starts hidden and stays hidden until
    /// there is something to show.
    pub fn attach(
        window: &NotifyWindow,
        handle: Handle,
        signals: mpsc::Sender<Signal>,
    ) -> Rc<RefCell<Ui>> {
        let cards = Rc::new(VecModel::default());
        let history = Rc::new(VecModel::default());
        window.set_cards(ModelRc::from(cards.clone()));
        window.set_history(ModelRc::from(history.clone()));

        let limits = Limits::default();
        let file = history::path();
        let mut store = Store::new(limits);
        if let Some(file) = &file {
            store.restore(history::load(file, limits.history));
        }
        let saved = store.changes();

        let ui = Rc::new_cyclic(|this| {
            RefCell::new(Ui {
                window: window.clone_strong(),
                handle,
                store,
                cards,
                rows: Vec::new(),
                history,
                signals,
                expiry: Timer::default(),
                grace: Timer::default(),
                save: Timer::default(),
                ago: Timer::default(),
                file,
                saved,
                started: Instant::now(),
                last_serial: 0,
                centre: false,
                hover: false,
                shape: Shape::Hidden,
                tallest: None,
                cards_guard: 0,
                centre_guard: 0,
                announced: (false, 0),
                this: this.clone(),
            })
        });

        let with = |act: fn(&mut Ui)| {
            let ui = Rc::downgrade(&ui);
            move || {
                if let Some(ui) = ui.upgrade() {
                    act(&mut ui.borrow_mut());
                }
            }
        };
        window.on_cards_resized(with(Ui::reshape));
        window.on_open_centre(with(|ui| ui.set_centre(true)));
        window.on_close_centre(with(|ui| ui.set_centre(false)));
        window.on_clear_all(with(|ui| {
            if ui.centre_is_guarded() {
                return;
            }
            let mut closed = Vec::new();
            ui.store.clear(&mut closed);
            ui.refresh(closed);
        }));

        let weak = Rc::downgrade(&ui);
        window.on_card_clicked({
            let weak = weak.clone();
            move |serial| {
                if let Some(ui) = weak.upgrade() {
                    ui.borrow_mut().dismiss(serial, Some(DEFAULT_ACTION));
                }
            }
        });
        window.on_card_closed({
            let weak = weak.clone();
            move |serial| {
                if let Some(ui) = weak.upgrade() {
                    ui.borrow_mut().dismiss(serial, None);
                }
            }
        });
        window.on_card_action({
            let weak = weak.clone();
            move |serial, key| {
                if let Some(ui) = weak.upgrade() {
                    ui.borrow_mut().dismiss(serial, Some(key.as_str()));
                }
            }
        });
        window.on_hover_changed({
            let weak = weak.clone();
            move |hover| {
                if let Some(ui) = weak.upgrade() {
                    ui.borrow_mut().set_hover(hover);
                }
            }
        });
        window.on_history_dismissed({
            let weak = weak.clone();
            move |key| {
                if let Some(ui) = weak.upgrade() {
                    ui.borrow_mut().forget(key);
                }
            }
        });
        window.on_group_cleared({
            let weak = weak.clone();
            move |app| {
                if let Some(ui) = weak.upgrade() {
                    ui.borrow_mut().forget_app(app.as_str());
                }
            }
        });
        // What was remembered shows the first time the centre opens, and
        // the lines are in order from the start.
        ui.borrow_mut().sync_history();
        window.on_dnd_toggled(move |dnd| {
            if let Some(ui) = weak.upgrade() {
                let mut ui = ui.borrow_mut();
                if ui.centre_is_guarded() {
                    // The switch has already moved; put it back.
                    let dnd = ui.store.dnd();
                    ui.window.set_dnd(dnd);
                } else {
                    ui.set_dnd(dnd);
                }
            }
        });
        ui
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn guard_time(&self) -> u64 {
        self.window.get_click_guard().max(0) as u64
    }

    fn centre_is_guarded(&self) -> bool {
        guarded(self.now(), self.centre_guard)
    }

    /// The output the surface is on has this height. The strip of cards is
    /// kept within it whatever the cards add up to.
    pub fn set_output_height(&mut self, height: Option<u32>) {
        self.tallest = height.filter(|height| *height > 0);
        self.reshape();
    }

    /// Handles a message from the bus thread.
    pub fn receive(&mut self, message: ToUi) {
        let mut closed = Vec::new();
        let now = self.now();
        match message {
            ToUi::Notify(note) => {
                let mut note = *note;
                note.time = chrono::Local::now().format("%H:%M").to_string();
                note.at = history::unix_now();
                self.store.add(note, now, &mut closed);
            }
            ToUi::Close(id) => {
                self.store.close(id, Reason::Closed, now, &mut closed);
            }
            ToUi::Command(Command::ToggleCentre) => {
                self.set_centre(!self.centre);
                return;
            }
            ToUi::Command(Command::DismissAll) => self.store.dismiss_all(&mut closed),
            ToUi::Command(Command::Dnd(dnd)) => {
                let dnd = match dnd {
                    Dnd::On => true,
                    Dnd::Off => false,
                    Dnd::Toggle => !self.store.dnd(),
                };
                self.set_dnd(dnd);
                return;
            }
            // These end the program and are dealt with before they get here.
            ToUi::NameLost | ToUi::BusClosed => return,
        }
        self.refresh(closed);
    }

    fn set_dnd(&mut self, dnd: bool) {
        let mut closed = Vec::new();
        let now = self.now();
        self.store.set_dnd(dnd, now, &mut closed);
        self.window.set_dnd(dnd);
        self.refresh(closed);
    }

    pub fn close_centre(&mut self) {
        self.set_centre(false);
    }

    fn set_centre(&mut self, open: bool) {
        if self.centre == open {
            return;
        }
        self.centre = open;
        // Seen now, and whatever comes in while it is open is seen too.
        self.store.mark_read();
        if open {
            self.centre_guard = self.now() + self.guard_time();
            self.window.set_centre_focus(-1);
        }
        self.window.set_centre_open(open);
        if open {
            self.window.invoke_take_keyboard();
            // The times were last worked out when the centre was closed.
            self.sync_history();
            let this = self.this.clone();
            self.ago.start(TimerMode::Repeated, AGO_EVERY, move || {
                if let Some(ui) = this.upgrade() {
                    ui.borrow_mut().sync_history();
                }
            });
        } else {
            self.ago.stop();
        }
        self.pause();
        self.arm();
        self.announce();
        self.reshape();
    }

    fn set_hover(&mut self, hover: bool) {
        if hover {
            self.grace.stop();
            self.hover = true;
            self.pause();
            self.arm();
            return;
        }
        let this = self.this.clone();
        self.grace
            .start(TimerMode::SingleShot, HOVER_GRACE, move || {
                if let Some(ui) = this.upgrade() {
                    let mut ui = ui.borrow_mut();
                    ui.hover = false;
                    ui.pause();
                    ui.arm();
                }
            });
    }

    /// The countdowns stand still while the pointer is on the cards and
    /// while the centre covers them.
    fn pause(&mut self) {
        let now = self.now();
        self.store.set_paused(self.hover || self.centre, now);
    }

    /// A card was clicked, closed or had one of its buttons pressed.
    fn dismiss(&mut self, serial: i32, action: Option<&str>) {
        // What is under the pointer has only just got there. The click was
        // aimed at whatever was there before.
        if guarded(self.now(), self.cards_guard) {
            return;
        }
        let Some(id) = self
            .rows
            .iter()
            .find(|row| row.serial == serial && !row.leaving)
            .map(|row| row.id)
        else {
            return;
        };
        let Some(note) = self.store.active(id) else {
            return;
        };
        // Only an action the notification really offers is passed on.
        let invoked = action.filter(|key| {
            if *key == DEFAULT_ACTION {
                note.has_default
            } else {
                note.actions.iter().any(|action| action.key == *key)
            }
        });
        if let Some(key) = invoked {
            let _ = self.signals.send(Signal::Action {
                id,
                key: key.to_string(),
                sender: note.sender.clone(),
            });
        }
        let mut closed = Vec::new();
        let now = self.now();
        self.store.close(id, Reason::Dismissed, now, &mut closed);
        self.refresh(closed);
    }

    /// A line in the centre was dismissed. Slint knows it by the low bits
    /// of its key.
    fn forget(&mut self, key: i32) {
        if self.centre_is_guarded() {
            return;
        }
        let Some(key) = self
            .store
            .history()
            .map(|entry| entry.key)
            .find(|entry| *entry as i32 == key)
        else {
            return;
        };
        let mut closed = Vec::new();
        let now = self.now();
        self.store.forget(key, now, &mut closed);
        self.refresh(closed);
    }

    /// "Clear" on the heading of a group.
    fn forget_app(&mut self, app: &str) {
        if self.centre_is_guarded() {
            return;
        }
        let mut closed = Vec::new();
        let now = self.now();
        self.store.forget_app(app, now, &mut closed);
        self.refresh(closed);
    }

    /// Writes the history file soon, if it differs from what is in it.
    fn schedule_save(&mut self) {
        if self.file.is_none() || self.store.changes() == self.saved || self.save.running() {
            return;
        }
        let this = self.this.clone();
        self.save.start(TimerMode::SingleShot, SAVE_DELAY, move || {
            if let Some(ui) = this.upgrade() {
                ui.borrow_mut().save_now();
            }
        });
    }

    /// Writes the history file now. A failure is not worth more than a line
    /// on standard error: the history is only a convenience.
    pub fn save_now(&mut self) {
        let Some(file) = &self.file else {
            return;
        };
        if self.store.changes() == self.saved {
            return;
        }
        self.saved = self.store.changes();
        let text = history::encode(self.store.history().map(|entry| &entry.note));
        if let Err(error) = history::save(file, &text) {
            eprintln!("sengi-notify: could not save the history: {error}");
        }
    }

    /// Brings everything in line with the store after a change.
    fn refresh(&mut self, closed: Vec<Closed>) {
        for closed in closed {
            let _ = self.signals.send(Signal::Closed {
                id: closed.id,
                reason: closed.reason as u32,
                sender: closed.sender,
                token: closed.token,
            });
        }
        self.sync_cards();
        self.sync_history();
        self.schedule_save();
        self.window.set_queued(self.store.queued() as i32);
        self.arm();
        self.announce();
        self.reshape();
    }

    /// Tells the bus side when do-not-disturb or the unread count changed.
    fn announce(&mut self) {
        if self.centre {
            self.store.mark_read();
        }
        let state = (self.store.dnd(), self.store.unread() as u32);
        if state != self.announced {
            self.announced = state;
            let _ = self.signals.send(Signal::State {
                dnd: state.0,
                unread: state.1,
            });
        }
    }

    fn sync_cards(&mut self) {
        let mut moved = false;

        // Cards the store no longer shows start to leave.
        let mut leaving = self.rows.iter().filter(|row| row.leaving).count();
        let mut index = 0;
        while index < self.rows.len() {
            let (generation, already) = {
                let row = &self.rows[index];
                (row.generation, row.leaving)
            };
            let shown = self.store.shown();
            let stays = already || shown.iter().any(|popup| popup.generation == generation);
            if stays {
                index += 1;
                continue;
            }
            moved = true;
            if leaving >= LEAVING_MAX {
                self.rows.remove(index);
                self.cards.remove(index);
                continue;
            }
            leaving += 1;
            let row = &mut self.rows[index];
            row.leaving = true;
            let serial = row.serial;
            if let Some(mut data) = self.cards.row_data(index) {
                data.leaving = true;
                self.cards.set_row_data(index, data);
            }
            self.remove_later(serial);
            index += 1;
        }

        // The store only ever adds at the top and removes, so walking both
        // lists in order finds each card either in place or new.
        let mut index = 0;
        for popup in self.store.shown() {
            while self.rows.get(index).is_some_and(|row| row.leaving) {
                index += 1;
            }
            match self.rows.get_mut(index) {
                Some(row) if row.generation == popup.generation => {
                    if row.version != popup.version {
                        row.version = popup.version;
                        self.cards
                            .set_row_data(index, card(popup, row.serial, false));
                        moved = true;
                    }
                }
                _ => {
                    self.last_serial = self.last_serial.wrapping_add(1);
                    let serial = self.last_serial;
                    self.rows.insert(
                        index,
                        Row {
                            serial,
                            generation: popup.generation,
                            id: popup.note.id,
                            version: popup.version,
                            leaving: false,
                        },
                    );
                    self.cards.insert(index, card(popup, serial, false));
                    moved = true;
                }
            }
            index += 1;
        }

        // A card that appeared, changed or shifted is not what the user
        // was aiming at a moment ago.
        if moved {
            self.cards_guard = self.now() + self.guard_time();
        }
    }

    /// Takes a leaving card out of the list once it has faded.
    fn remove_later(&self, serial: i32) {
        let this = self.this.clone();
        let wait = Duration::from_millis(self.window.get_leave_time().max(0) as u64);
        Timer::single_shot(wait, move || {
            let Some(ui) = this.upgrade() else {
                return;
            };
            let mut ui = ui.borrow_mut();
            if let Some(index) = ui.rows.iter().position(|row| row.serial == serial) {
                ui.rows.remove(index);
                ui.cards.remove(index);
                ui.reshape();
            }
        });
    }

    fn sync_history(&mut self) {
        let entries: Vec<_> = self.store.history().collect();
        let items = history_items(&entries, history::unix_now());
        // Lines that moved under the pointer get the same guard as cards.
        // Only the lines themselves count: new words in "2 min ago" do not.
        let same_line = |new: &HistoryItem, old: &HistoryItem| {
            new.key == old.key && new.header == old.header && new.group == old.group
        };
        let same = items.len() == self.history.row_count()
            && items
                .iter()
                .zip(self.history.iter())
                .all(|(new, old)| same_line(new, &old));
        if same {
            // Updated where they are, so that the list keeps its place.
            for (index, item) in items.into_iter().enumerate() {
                if self.history.row_data(index).as_ref() != Some(&item) {
                    self.history.set_row_data(index, item);
                }
            }
            return;
        }
        if self.centre {
            self.centre_guard = self.now() + self.guard_time();
        }
        self.history.set_vec(items);
    }

    /// Sets the one timer for the next card to expire, or stops it. With
    /// nothing counting down the process has no reason to wake.
    fn arm(&mut self) {
        let Some(deadline) = self.store.next_deadline() else {
            self.expiry.stop();
            return;
        };
        let this = self.this.clone();
        let wait = Duration::from_millis(deadline.saturating_sub(self.now()));
        self.expiry.start(TimerMode::SingleShot, wait, move || {
            let Some(ui) = this.upgrade() else {
                return;
            };
            let mut ui = ui.borrow_mut();
            let mut closed = Vec::new();
            let now = ui.now();
            ui.store.expire(now, &mut closed);
            ui.refresh(closed);
        });
    }

    /// Fits the surface to what is on screen.
    fn reshape(&mut self) {
        let shape = if self.centre {
            Shape::Centre
        } else if self.rows.is_empty() {
            Shape::Hidden
        } else {
            let top = self.window.get_strip_top();
            let wanted = (top + self.window.get_cards_height()).ceil() as u32;
            Shape::Strip {
                width: self.window.get_strip_width().ceil() as u32,
                // Never zero, which would ask for a stretched surface, and
                // never taller than the screen.
                height: wanted.min(self.tallest.unwrap_or(u32::MAX)).max(1),
            }
        };
        if shape == self.shape {
            return;
        }
        self.shape = shape;

        let handle = &self.handle;
        let applied = match shape {
            Shape::Hidden => {
                handle.hide();
                Ok(())
            }
            Shape::Strip { width, height } => {
                handle.set_keyboard(KeyboardMode::None);
                // Only the cards take clicks. The margin above them and
                // anything the surface is ahead of the cards by does not.
                let top = self.window.get_strip_top().round() as i32;
                let cards = Rect::new(
                    0,
                    top,
                    self.window.get_cards_width().round() as i32,
                    height as i32 - top,
                );
                handle.set_input_region(Some(&[cards]));
                handle.show();
                handle.set_anchor_and_size(Anchor::TOP | Anchor::RIGHT, width, height)
            }
            Shape::Centre => {
                handle.set_keyboard(KeyboardMode::Exclusive);
                handle.set_input_region(None);
                handle.show();
                handle.set_anchor_and_size(
                    Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                    0,
                    0,
                )
            }
        };
        if let Err(error) = applied {
            eprintln!("sengi-notify: could not place the surface: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::notification::Notification;

    fn entry(key: u64, app: &str, summary: &str, at: u64) -> Entry {
        Entry {
            key,
            note: Notification {
                app: app.to_string(),
                summary: summary.to_string(),
                body: format!("{summary}, a little more to read."),
                at,
                ..Notification::default()
            },
        }
    }

    #[test]
    fn the_centre_lists_a_heading_per_app_with_a_count_and_relative_times() {
        let now = 1_000_000;
        let entries = [
            entry(1, "Mail", "Newest", now - 30),
            entry(2, "Chat", "Hi", now - 125),
            entry(3, "Mail", "Older", now - 3 * 3_600),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();
        let items = history_items(&refs, now);
        let summary: Vec<(bool, i32, String, String)> = items
            .iter()
            .map(|item| {
                (
                    item.header,
                    item.count,
                    item.app.to_string(),
                    item.time.to_string(),
                )
            })
            .collect();
        let line = |header, count, app: &str, time: &str| {
            (header, count, app.to_string(), time.to_string())
        };
        assert_eq!(
            summary,
            [
                line(true, 2, "Mail", ""),
                line(false, 0, "Mail", "now"),
                line(false, 0, "Mail", "3 hours ago"),
                line(true, 1, "Chat", ""),
                line(false, 0, "Chat", "2 min ago"),
            ]
        );
        assert_eq!(items[0].group.as_str(), "Mail");
    }

    #[test]
    fn a_click_inside_the_guard_is_ignored_and_one_after_it_is_not() {
        assert!(guarded(0, 400));
        assert!(guarded(399, 400));
        assert!(!guarded(400, 400));
        assert!(!guarded(1_000, 400));
        // Nothing has moved yet.
        assert!(!guarded(0, 0));
    }
}
