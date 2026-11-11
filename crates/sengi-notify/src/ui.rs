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
use crate::notification::{Urgency, DEFAULT_ACTION};
use crate::store::{Closed, Limits, Popup, Reason, Store};

slint::include_modules!();

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
        // Not in the shared Slint tokens yet, so it is read here.
        if let Some(button) = sengi_tokens::Tokens::builtin().px("size.button") {
            window.global::<Metrics>().set_button(button as f32);
        }

        let ui = Rc::new_cyclic(|this| {
            RefCell::new(Ui {
                window: window.clone_strong(),
                handle,
                store: Store::new(Limits::default()),
                cards,
                rows: Vec::new(),
                history,
                signals,
                expiry: Timer::default(),
                grace: Timer::default(),
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
            ToUi::Notify(mut note) => {
                note.time = chrono::Local::now().format("%H:%M").to_string();
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
        let items: Vec<HistoryItem> = self
            .store
            .history()
            .map(|entry| HistoryItem {
                key: entry.key as i32,
                app: entry.note.app.as_str().into(),
                time: entry.note.time.as_str().into(),
                summary: entry.note.summary.as_str().into(),
                body: entry.note.body.as_str().into(),
                critical: entry.note.urgency == Urgency::Critical,
            })
            .collect();
        // Lines that moved under the pointer get the same guard as cards.
        let same = items.len() == self.history.row_count()
            && items
                .iter()
                .zip(self.history.iter())
                .all(|(new, old)| new.key == old.key);
        if !same && self.centre {
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
