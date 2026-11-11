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

/// A card in the Slint list. `rows` runs parallel to the `cards` model.
struct Row {
    serial: i32,
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
    started: Instant,
    last_serial: i32,
    centre: bool,
    hover: bool,
    shape: Shape,
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
                started: Instant::now(),
                last_serial: 0,
                centre: false,
                hover: false,
                shape: Shape::Hidden,
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
        window.on_close_centre(with(|ui| ui.set_centre(false)));
        window.on_clear_all(with(|ui| {
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
                    let mut ui = ui.borrow_mut();
                    ui.hover = hover;
                    ui.pause();
                    ui.arm();
                }
            }
        });
        window.on_history_dismissed({
            let weak = weak.clone();
            move |key| {
                if let Some(ui) = weak.upgrade() {
                    let mut ui = ui.borrow_mut();
                    let mut closed = Vec::new();
                    let now = ui.now();
                    ui.store.forget(key as u64, now, &mut closed);
                    ui.refresh(closed);
                }
            }
        });
        window.on_dnd_toggled(move |dnd| {
            if let Some(ui) = weak.upgrade() {
                ui.borrow_mut().set_dnd(dnd);
            }
        });
        ui
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
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
            ToUi::NameLost => {
                eprintln!("sengi-notify: another notification service took over");
                self.handle.quit();
                return;
            }
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
        self.window.set_centre_open(open);
        if open {
            self.window.invoke_take_keyboard();
        }
        self.pause();
        self.arm();
        self.reshape();
    }

    /// The countdowns stand still while the pointer is on the cards and
    /// while the centre covers them.
    fn pause(&mut self) {
        let now = self.now();
        self.store.set_paused(self.hover || self.centre, now);
    }

    /// A card was clicked, closed or had one of its buttons pressed.
    fn dismiss(&mut self, serial: i32, action: Option<&str>) {
        let Some(id) = self
            .rows
            .iter()
            .find(|row| row.serial == serial && !row.leaving)
            .map(|row| row.id)
        else {
            return;
        };
        // Only an action the notification really offers is passed on.
        let invoked = action.filter(|key| {
            self.store.active(id).is_some_and(|note| {
                if *key == DEFAULT_ACTION {
                    note.has_default
                } else {
                    note.actions.iter().any(|action| action.key == *key)
                }
            })
        });
        if let Some(key) = invoked {
            let _ = self.signals.send(Signal::Action {
                id,
                key: key.to_string(),
            });
        }
        let mut closed = Vec::new();
        let now = self.now();
        self.store.close(id, Reason::Dismissed, now, &mut closed);
        self.refresh(closed);
    }

    /// Brings everything in line with the store after a change.
    fn refresh(&mut self, closed: Vec<Closed>) {
        for Closed { id, reason } in closed {
            let _ = self.signals.send(Signal::Closed {
                id,
                reason: reason as u32,
            });
        }
        self.sync_cards();
        self.sync_history();
        self.window.set_queued(self.store.queued() as i32);
        self.arm();
        self.reshape();
    }

    fn sync_cards(&mut self) {
        // Cards the store no longer shows start to leave.
        for index in 0..self.rows.len() {
            let row = &mut self.rows[index];
            if row.leaving
                || self
                    .store
                    .shown()
                    .iter()
                    .any(|popup| popup.note.id == row.id)
            {
                continue;
            }
            row.leaving = true;
            let serial = row.serial;
            if let Some(mut data) = self.cards.row_data(index) {
                data.leaving = true;
                self.cards.set_row_data(index, data);
            }
            self.remove_later(serial);
        }

        // The store only ever adds at the top and removes, so walking both
        // lists in order finds each card either in place or new.
        let mut index = 0;
        for popup in self.store.shown() {
            while self.rows.get(index).is_some_and(|row| row.leaving) {
                index += 1;
            }
            match self.rows.get_mut(index) {
                Some(row) if row.id == popup.note.id => {
                    if row.version != popup.version {
                        row.version = popup.version;
                        self.cards
                            .set_row_data(index, card(popup, row.serial, false));
                    }
                }
                _ => {
                    self.last_serial = self.last_serial.wrapping_add(1);
                    let serial = self.last_serial;
                    self.rows.insert(
                        index,
                        Row {
                            serial,
                            id: popup.note.id,
                            version: popup.version,
                            leaving: false,
                        },
                    );
                    self.cards.insert(index, card(popup, serial, false));
                }
            }
            index += 1;
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
            Shape::Strip {
                width: self.window.get_strip_width().ceil() as u32,
                // Never zero, which would ask for a stretched surface.
                height: ((top + self.window.get_cards_height()).ceil() as u32).max(1),
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
