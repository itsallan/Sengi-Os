//! The Network page in the window: the model's state into the page's
//! properties, the page's callbacks into the model and the worker.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use slint::ComponentHandle;

use super::{
    level, strength, Block, Command, Event, Join, Link, Model, NetworkService, Problem, Refusal,
    Step, Wifi, Words,
};
use crate::app::{self, item, App, Controller};
use crate::secret::Secret;
use crate::text;
use crate::ui::{DialogSpec, Item, NetworkData};
use crate::worker::Handle;

pub struct NetworkPage {
    service: Arc<dyn NetworkService>,
    worker: RefCell<Option<Handle<Command>>>,
    model: RefCell<Model>,
    /// The dialog on screen is this page's question about joining.
    asking: Cell<bool>,
}

/// A name inside a sentence of ours, in quotes and with its direction
/// kept to itself.
fn quoted(name: &str) -> String {
    format!("\u{201c}{}\u{201d}", text::isolate(name))
}

fn refusal_words(what: &str, refusal: Refusal) -> String {
    match refusal {
        Refusal::Denied => format!(
            "You are not allowed to {what}. Wi-Fi is managed by iwd, which only takes \
             requests from members of the netdev group."
        ),
        Refusal::Failed => {
            format!("Could not {what}. The Wi-Fi service did not accept the request.")
        }
    }
}

impl NetworkPage {
    pub fn attach(app: &Rc<App>, service: Arc<dyn NetworkService>) -> Rc<NetworkPage> {
        let page = Rc::new(NetworkPage {
            service,
            worker: RefCell::default(),
            model: RefCell::default(),
            asking: Cell::new(false),
        });
        let data = app.window.global::<NetworkData>();
        data.set_status("Looking for Wi-Fi…".into());

        let on = |handler: fn(&Rc<NetworkPage>, &Rc<App>)| {
            let (page, app) = (Rc::downgrade(&page), Rc::downgrade(app));
            move || {
                if let (Some(page), Some(app)) = (page.upgrade(), app.upgrade()) {
                    handler(&page, &app);
                }
            }
        };
        let on_id = |handler: fn(&Rc<NetworkPage>, &Rc<App>, &str)| {
            let (page, app) = (Rc::downgrade(&page), Rc::downgrade(app));
            move |id: slint::SharedString| {
                if let (Some(page), Some(app)) = (page.upgrade(), app.upgrade()) {
                    handler(&page, &app, &id);
                }
            }
        };
        data.on_join(on_id(NetworkPage::join));
        data.on_forget(on_id(NetworkPage::forget));
        data.on_forget_connected(on(|page, app| {
            let id = page.model.borrow().connected().map(|n| n.id.clone());
            if let Some(id) = id {
                page.forget(app, &id);
            }
        }));
        data.on_disconnect(on(|page, _| page.send(Command::Disconnect)));
        data.on_scan(on(|page, _| page.send(Command::Scan)));
        data.on_released(on(|page, app| {
            page.model.borrow_mut().release();
            page.render(app);
        }));
        data.on_power({
            let (page, app) = (Rc::downgrade(&page), Rc::downgrade(app));
            move |on| {
                if let (Some(page), Some(app)) = (page.upgrade(), app.upgrade()) {
                    page.model.borrow_mut().want_power(on);
                    page.send(Command::Power(on));
                    page.render(&app);
                }
            }
        });
        page
    }

    fn send(&self, command: Command) {
        if let Some(worker) = self.worker.borrow().as_ref() {
            worker.send(command);
        }
    }

    pub fn on_event(self: &Rc<Self>, app: &Rc<App>, event: Event) {
        match event {
            Event::Reading(reading) => {
                let held = app.window.global::<NetworkData>().get_engaged();
                self.model.borrow_mut().update(reading, held);
                // Wi-Fi went away under a question about joining.
                if self.asking.get() && self.model.borrow().join == Join::Idle {
                    self.asking.set(false);
                    app.close_dialog();
                }
            }
            Event::Joined { id, result } => {
                self.model.borrow_mut().join.finished(&id, result);
                self.follow_join(app);
            }
            Event::Refused { what, refusal } => {
                self.model.borrow_mut().give_up_waiting();
                app.tell("That did not work", &refusal_words(what, refusal));
            }
        }
        self.render(app);
    }

    /// A row was activated.
    fn join(self: &Rc<Self>, app: &Rc<App>, id: &str) {
        let step = self.model.borrow_mut().choose(id);
        match step {
            Step::Send(request) => self.send(Command::Join(request)),
            Step::Nothing => {}
            Step::AskPassphrase | Step::ConfirmOpen | Step::Explain => self.follow_join(app),
        }
        self.render(app);
    }

    /// Put on screen whatever the joining steps now call for.
    fn follow_join(self: &Rc<Self>, app: &Rc<App>) {
        let join = self.model.borrow().join.clone();
        match join {
            Join::Idle | Join::Joining { .. } => {
                if self.asking.replace(false) {
                    app.close_dialog();
                }
            }
            Join::Asking { name, problem, .. } => {
                let was_refused = matches!(problem, Some(Problem::Rejected | Problem::Stale));
                let problem = problem.map_or("", |problem| problem.words());
                if self.asking.get() && app.dialog_open() {
                    app.update_dialog(|spec| spec.problem = problem.into());
                    app.set_dialog_busy(false);
                    // A passphrase the network refused is not worth keeping
                    // in the field; one that is merely too short is.
                    if was_refused {
                        app.wipe_dialog_field();
                    }
                    return;
                }
                self.asking.set(true);
                let (confirm, cancel) = (Rc::downgrade(self), Rc::downgrade(self));
                app.open_dialog(
                    DialogSpec {
                        title: format!("Join {}", quoted(&name)).into(),
                        body: "Enter the password for this network.".into(),
                        input: true,
                        masked: true,
                        placeholder: "Password".into(),
                        problem: problem.into(),
                        confirm: "Join".into(),
                        cancel: "Cancel".into(),
                        minimum: 8,
                        ..DialogSpec::default()
                    },
                    move |app, typed| {
                        // From here on the passphrase is wiped when dropped.
                        let secret = Secret::new(typed);
                        if let Some(page) = confirm.upgrade() {
                            page.submit(app, secret);
                        }
                    },
                    move |app| {
                        if let Some(page) = cancel.upgrade() {
                            page.cancel(app);
                        }
                    },
                );
            }
            Join::Confirming { id, name } => {
                self.asking.set(true);
                let (confirm, cancel) = (Rc::downgrade(self), Rc::downgrade(self));
                app.open_dialog(
                    DialogSpec {
                        title: "Join a network that is not encrypted?".into(),
                        body: format!(
                            "{} has no password. Anyone nearby can read what is sent over \
                             it, and anyone can set up a network with this name.",
                            quoted(&name)
                        )
                        .into(),
                        confirm: "Join".into(),
                        cancel: "Cancel".into(),
                        danger: true,
                        ..DialogSpec::default()
                    },
                    move |app, _| {
                        let Some(page) = confirm.upgrade() else {
                            return;
                        };
                        let request = page.model.borrow_mut().join.confirm(&id);
                        page.asking.set(false);
                        app.close_dialog();
                        if let Some(request) = request {
                            page.send(Command::Join(request));
                        }
                        page.render(app);
                    },
                    move |app| {
                        if let Some(page) = cancel.upgrade() {
                            page.cancel(app);
                        }
                    },
                );
            }
            Join::Failed { name, error } => {
                // The question is over; the news of how it ended would
                // otherwise wait behind it.
                if self.asking.replace(false) {
                    app.close_dialog();
                }
                self.model.borrow_mut().join.cancel();
                app.tell(&format!("Could not join {}", quoted(&name)), error.words());
            }
        }
    }

    fn submit(self: &Rc<Self>, app: &Rc<App>, secret: Secret) {
        let request = self.model.borrow_mut().join.submit(secret);
        match request {
            Some(request) => {
                app.update_dialog(|spec| spec.problem = "".into());
                app.set_dialog_busy(true);
                self.send(Command::Join(request));
            }
            None => self.follow_join(app),
        }
        self.render(app);
    }

    /// The question was dismissed.
    fn cancel(self: &Rc<Self>, app: &Rc<App>) {
        self.asking.set(false);
        let joining = matches!(self.model.borrow().join, Join::Joining { .. });
        if joining {
            // Under way with a passphrase typed: have iwd call it off. The
            // attempt ends as aborted and the steps return to idle then.
            self.send(Command::Disconnect);
        } else {
            self.model.borrow_mut().join.cancel();
        }
        self.render(app);
    }

    fn forget(self: &Rc<Self>, app: &Rc<App>, id: &str) {
        let name = {
            let model = self.model.borrow();
            if !model.can_forget(id) {
                return;
            }
            model.name_of(id).unwrap_or_default().to_string()
        };
        let (page, id) = (Rc::downgrade(self), id.to_string());
        app.ask(
            DialogSpec {
                title: format!("Forget {}?", quoted(&name)).into(),
                body: "Its password is removed from this computer and it is no longer \
                       joined automatically. To use it again you will need the password."
                    .into(),
                confirm: "Forget".into(),
                cancel: "Cancel".into(),
                danger: true,
                ..DialogSpec::default()
            },
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.send(Command::Forget(id));
                }
            },
        );
    }

    fn render(&self, app: &Rc<App>) {
        let data = app.window.global::<NetworkData>();
        let model = self.model.borrow();
        let Some(reading) = model.reading() else {
            return;
        };
        let (note, warning) = match (&reading.wifi, reading.block) {
            (_, Block::Hard) => (
                "Wi-Fi is switched off by a switch or key on the computer. Use it to turn \
                 Wi-Fi back on.",
                true,
            ),
            (Wifi::Denied, _) => (
                "You are not allowed to manage Wi-Fi on this computer. iwd only answers \
                 members of the netdev group.",
                true,
            ),
            (Wifi::Absent, _) if reading.hardware => (
                "The Wi-Fi service (iwd) is not running, so Wi-Fi cannot be managed here.",
                true,
            ),
            (Wifi::Absent, _) => ("This computer has no Wi-Fi adapter.", false),
            (Wifi::Off, Block::Soft) => (
                "Wi-Fi is blocked, as by an aeroplane mode key. It has to be unblocked \
                 before it can be switched on.",
                true,
            ),
            _ => ("", false),
        };
        data.set_note(note.into());
        data.set_note_warning(warning);
        data.set_available(matches!(reading.wifi, Wifi::Off | Wifi::On { .. }));
        data.set_powered(model.switch_on());

        let link = model.link();
        let connected = model.connected();
        data.set_scanning(model.scanning());
        data.set_status(
            match (link, connected) {
                (None, _) => "Off",
                (Some(_), Some(_)) => "Connected",
                (Some(Link::Connecting), None) => "Connecting…",
                (Some(_), None) => "Not connected",
            }
            .into(),
        );
        data.set_connected(connected.is_some());
        if let Some(network) = connected {
            data.set_name(network.name.as_str().into());
            data.set_saved(network.known);
            data.set_signal(
                format!(
                    "{} ({} dBm)",
                    strength(network.signal),
                    network.signal / 100
                )
                .into(),
            );
            data.set_security(network.security.words().into());
            data.set_address(if reading.addresses.is_empty() {
                "Waiting for an address…".into()
            } else {
                reading.addresses.join(", ").into()
            });
        }

        let target = model.join.target();
        let joining = matches!(model.join, Join::Joining { .. });
        let rows: Vec<Item> = model
            .listed()
            .iter()
            .map(|network| {
                let busy = joining && target == Some(network.id.as_str());
                let detail = if network.gone {
                    "No longer in range".to_string()
                } else if network.known {
                    format!("{} · Saved", network.security.words())
                } else {
                    network.security.words().to_string()
                };
                Item {
                    icon: "signal".into(),
                    level: if network.gone {
                        0
                    } else {
                        level(network.signal)
                    },
                    action: if busy || network.gone { "" } else { "Join" }.into(),
                    remove: if network.known && !busy && !network.gone {
                        "Forget"
                    } else {
                        ""
                    }
                    .into(),
                    status: if busy { "Joining…" } else { "" }.into(),
                    enabled: !network.gone,
                    ..item(&network.id, &network.name, &detail)
                }
            })
            .collect();
        data.set_networks(app::model(rows));

        let wired: Vec<Item> = reading
            .wired
            .iter()
            .map(|link| {
                let title = format!("Ethernet ({})", link.interface);
                let detail = if link.up {
                    "Connected"
                } else {
                    "Cable unplugged"
                };
                Item {
                    status: if !link.up {
                        "".into()
                    } else if link.addresses.is_empty() {
                        "No address yet".into()
                    } else {
                        link.addresses.join(", ").into()
                    },
                    icon: "wired".into(),
                    enabled: link.up,
                    ..item(&link.interface, &title, detail)
                }
            })
            .collect();
        data.set_wired(app::model(wired));
    }
}

impl Controller for NetworkPage {
    fn show(&self, _app: &Rc<App>) {
        let mut worker = self.worker.borrow_mut();
        match worker.as_ref() {
            // Coming back to the page is a reason to look again.
            Some(worker) => worker.send(Command::Scan),
            None => {
                *worker = Some(super::start(self.service.clone(), |event| {
                    app::post(move |app| {
                        if let Some(page) = app.page::<NetworkPage>() {
                            page.on_event(app, event);
                        }
                    });
                }));
            }
        }
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}
