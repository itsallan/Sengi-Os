//! The Bluetooth page in the window.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use slint::ComponentHandle;

use super::{
    Answer, Bluetooth, BluetoothService, Command, Device, Event, Model, Pairing, Prompt, Question,
};
use crate::app::{self, item, App, Controller};
use crate::held::Row;
use crate::network::{Block, Refusal, Words};
use crate::secret::Secret;
use crate::text;
use crate::ui::{BluetoothData, DialogSpec, Item};
use crate::worker::Handle;

pub struct BluetoothPage {
    service: Arc<dyn BluetoothService>,
    worker: RefCell<Option<Handle<Command>>>,
    model: RefCell<Model>,
    /// The dialog on screen is this page's, about pairing.
    asking: Cell<bool>,
}

fn quoted(name: &str) -> String {
    format!("\u{201c}{}\u{201d}", text::isolate(name))
}

/// A passkey as BlueZ and devices show it: six digits.
fn digits(passkey: u32) -> String {
    format!("{:06}", passkey % 1_000_000)
}

/// The dialog for where pairing stands.
fn pairing_dialog(name: &str, prompt: Option<&Prompt>) -> DialogSpec {
    let base = DialogSpec {
        title: format!("Pairing with {}", quoted(name)).into(),
        cancel: "Cancel".into(),
        ..DialogSpec::default()
    };
    match prompt.map(|prompt| &prompt.question) {
        None => DialogSpec {
            body: "Waiting for the device. Follow any instructions it shows.".into(),
            ..base
        },
        Some(Question::ConfirmPasskey(passkey)) => DialogSpec {
            body: "Check that the device shows this number. If it shows a different one, \
                   or none, cancel."
                .into(),
            code: digits(*passkey).into(),
            confirm: "Pair".into(),
            danger: true,
            ..base
        },
        Some(Question::EnterPin) => DialogSpec {
            body: "Enter the PIN of the device. It may be shown on the device or printed \
                   in its manual; 0000 and 1234 are common."
                .into(),
            input: true,
            masked: true,
            placeholder: "PIN".into(),
            minimum: 1,
            confirm: "Pair".into(),
            ..base
        },
        Some(Question::EnterPasskey) => DialogSpec {
            body: "Enter the number shown on the device.".into(),
            input: true,
            masked: true,
            placeholder: "Passkey".into(),
            minimum: 1,
            confirm: "Pair".into(),
            ..base
        },
        Some(Question::ShowPin(pin)) => DialogSpec {
            body: "Type this PIN on the device, then press Enter on it.".into(),
            code: pin.as_str().into(),
            ..base
        },
        Some(Question::ShowPasskey(passkey)) => DialogSpec {
            body: "Type this number on the device, then press Enter on it.".into(),
            code: digits(*passkey).into(),
            ..base
        },
        Some(Question::Authorize) => DialogSpec {
            body: "The device asks to pair. Go ahead?".into(),
            confirm: "Pair".into(),
            danger: true,
            ..base
        },
    }
}

impl BluetoothPage {
    pub fn attach(app: &Rc<App>, service: Arc<dyn BluetoothService>) -> Rc<BluetoothPage> {
        let page = Rc::new(BluetoothPage {
            service,
            worker: RefCell::default(),
            model: RefCell::default(),
            asking: Cell::new(false),
        });
        let data = app.window.global::<BluetoothData>();
        data.set_status("Looking for Bluetooth…".into());

        let on = |handler: fn(&Rc<BluetoothPage>, &Rc<App>)| {
            let (page, app) = (Rc::downgrade(&page), Rc::downgrade(app));
            move || {
                if let (Some(page), Some(app)) = (page.upgrade(), app.upgrade()) {
                    handler(&page, &app);
                }
            }
        };
        let on_id = |handler: fn(&Rc<BluetoothPage>, &Rc<App>, &str)| {
            let (page, app) = (Rc::downgrade(&page), Rc::downgrade(app));
            move |id: slint::SharedString| {
                if let (Some(page), Some(app)) = (page.upgrade(), app.upgrade()) {
                    handler(&page, &app, &id);
                }
            }
        };
        data.on_toggle(on_id(|page, app, id| {
            let command = page.model.borrow_mut().toggle(id);
            page.send_some(command);
            page.render(app);
        }));
        data.on_pair(on_id(|page, app, id| {
            let command = page.model.borrow_mut().pair(id);
            if command.is_some() {
                // Searching and pairing fight over the radio.
                page.send(Command::Search(false));
            }
            page.send_some(command);
            page.follow_pairing(app);
            page.render(app);
        }));
        data.on_remove(on_id(BluetoothPage::remove));
        data.on_search(on(|page, _| {
            let searching = page.model.borrow().searching();
            page.send(Command::Search(!searching));
        }));
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

    fn send_some(&self, command: Option<Command>) {
        if let Some(command) = command {
            self.send(command);
        }
    }

    pub fn on_event(self: &Rc<Self>, app: &Rc<App>, event: Event) {
        match event {
            Event::Reading(reading) => {
                let data = app.window.global::<BluetoothData>();
                let (mine, nearby) = (data.get_engaged_mine(), data.get_engaged_nearby());
                self.model.borrow_mut().update(reading, mine, nearby);
            }
            Event::Prompt(prompt) => {
                self.model.borrow_mut().prompted(prompt);
                self.follow_pairing(app);
            }
            Event::Paired { id, result } => {
                let command = self.model.borrow_mut().paired(&id, result);
                self.send_some(command);
                self.follow_pairing(app);
            }
            Event::Connected { id, result } => {
                let name = {
                    let mut model = self.model.borrow_mut();
                    model.connected(&id);
                    model.find(&id).map(|device| device.name.clone())
                };
                if let (Err(_), Some(name)) = (result, name) {
                    app.tell(
                        &format!("Could not connect to {}", quoted(&name)),
                        "Make sure the device is switched on and nearby. If it is paired \
                         with another computer or phone, it may be connected to that instead.",
                    );
                }
            }
            Event::Refused { what, refusal } => {
                self.model.borrow_mut().give_up_waiting();
                let why = match refusal {
                    Refusal::Denied => "You are not allowed to; BlueZ only takes requests \
                                        from members of the bluetooth group."
                        .to_string(),
                    Refusal::Failed => "The Bluetooth service did not accept the request.".into(),
                };
                app.tell(&format!("Could not {what}"), &why);
            }
        }
        self.render(app);
    }

    /// Put on screen whatever pairing now calls for.
    fn follow_pairing(self: &Rc<Self>, app: &Rc<App>) {
        let pairing = self.model.borrow().pairing.clone();
        match pairing {
            Pairing::Idle => {
                if self.asking.replace(false) {
                    app.close_dialog();
                }
            }
            Pairing::Running { name, prompt, .. } => {
                self.asking.set(true);
                let spec = pairing_dialog(&name, prompt.as_ref());
                let typed = spec.input;
                let (confirm, cancel) = (Rc::downgrade(self), Rc::downgrade(self));
                app.open_dialog(
                    spec,
                    move |app, text| {
                        let Some(page) = confirm.upgrade() else {
                            return;
                        };
                        let answer = if typed {
                            // From here on the PIN is wiped when dropped.
                            Answer::Typed(Secret::new(text))
                        } else {
                            Answer::Yes
                        };
                        page.model.borrow_mut().answer(answer);
                        // Answered: back to waiting for the device.
                        page.follow_pairing(app);
                    },
                    move |app| {
                        let Some(page) = cancel.upgrade() else { return };
                        page.asking.set(false);
                        let command = page.model.borrow_mut().cancel();
                        page.send_some(command);
                        page.render(app);
                    },
                );
                if typed {
                    app.wipe_dialog_field();
                }
            }
            Pairing::Failed { name, error } => {
                self.asking.set(false);
                self.model.borrow_mut().cancel();
                app.tell(
                    &format!("Could not pair with {}", quoted(&name)),
                    error.words(),
                );
            }
        }
    }

    fn remove(self: &Rc<Self>, app: &Rc<App>, id: &str) {
        let Some(name) = self.model.borrow().find(id).map(|d| d.name.clone()) else {
            return;
        };
        let (page, id) = (Rc::downgrade(self), id.to_string());
        app.ask(
            DialogSpec {
                title: format!("Remove {}?", quoted(&name)).into(),
                body: "The device is unpaired and will no longer connect to this computer. \
                       To use it again it has to be paired again."
                    .into(),
                confirm: "Remove".into(),
                cancel: "Cancel".into(),
                danger: true,
                ..DialogSpec::default()
            },
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.send(Command::Remove(id));
                }
            },
        );
    }

    fn render(&self, app: &Rc<App>) {
        let data = app.window.global::<BluetoothData>();
        let model = self.model.borrow();
        let Some(reading) = model.reading() else {
            return;
        };
        let (note, warning) = match (&reading.bluetooth, reading.block) {
            (_, Block::Hard) => (
                "Bluetooth is switched off by a switch or key on the computer. Use it to \
                 turn Bluetooth back on.",
                true,
            ),
            (Bluetooth::Denied, _) => (
                "You are not allowed to manage Bluetooth on this computer. BlueZ only \
                 answers members of the bluetooth group.",
                true,
            ),
            (Bluetooth::Absent, _) if reading.hardware => (
                "The Bluetooth service (BlueZ) is not running, so Bluetooth cannot be \
                 managed here.",
                true,
            ),
            (Bluetooth::Absent, _) => ("This computer has no Bluetooth adapter.", false),
            (Bluetooth::Off, Block::Soft) => (
                "Bluetooth is blocked, as by an aeroplane mode key. It has to be \
                 unblocked before it can be switched on.",
                true,
            ),
            _ => ("", false),
        };
        data.set_note(note.into());
        data.set_note_warning(warning);
        data.set_available(matches!(
            reading.bluetooth,
            Bluetooth::Off | Bluetooth::On { .. }
        ));
        data.set_powered(model.switch_on());
        data.set_searching(model.searching());
        let connected = model.mine().iter().filter(|d| d.connected).count();
        data.set_status(
            match (model.on(), connected) {
                (false, _) => "Off".to_string(),
                (true, 0) => "On".to_string(),
                (true, 1) => "On, one device connected".to_string(),
                (true, n) => format!("On, {n} devices connected"),
            }
            .into(),
        );

        let pairing_with = match &model.pairing {
            Pairing::Running { id, .. } => Some(id.as_str()),
            _ => None,
        };
        let row = |device: &Row<Device>| -> Item {
            let connecting = model.is_connecting(&device.id);
            let pairing = pairing_with == Some(device.id.as_str());
            let busy = connecting || pairing;
            let detail = match (device.gone, device.paired, device.connected) {
                (true, _, _) => "No longer in range",
                (_, true, true) => "Connected",
                (_, true, false) => "Not connected",
                (_, false, _) => "",
            };
            Item {
                icon: super::icon(device.kind).into(),
                action: match (busy || device.gone, device.paired, device.connected) {
                    (true, _, _) => "",
                    (_, false, _) => "Pair",
                    (_, true, true) => "Disconnect",
                    (_, true, false) => "Connect",
                }
                .into(),
                remove: if device.paired && !busy { "Remove" } else { "" }.into(),
                status: if connecting {
                    "Connecting…"
                } else if pairing {
                    "Pairing…"
                } else {
                    ""
                }
                .into(),
                checked: device.connected,
                enabled: !device.gone,
                ..item(&device.id, &device.name, detail)
            }
        };
        data.set_mine(app::model(model.mine().iter().map(row).collect()));
        data.set_nearby(app::model(model.nearby().iter().map(row).collect()));
    }
}

impl Controller for BluetoothPage {
    fn show(&self, _app: &Rc<App>) {
        let mut worker = self.worker.borrow_mut();
        if worker.is_none() {
            *worker = Some(super::start(self.service.clone(), |event| {
                app::post(move |app| {
                    if let Some(page) = app.page::<BluetoothPage>() {
                        page.on_event(app, event);
                    }
                });
            }));
        }
    }

    /// A search is for someone looking at the list.
    fn hide(&self, _app: &Rc<App>) {
        if self.model.borrow().searching() {
            self.send(Command::Search(false));
        }
    }

    fn shutdown(&self) {
        // The worker stops the search on its way out. BlueZ would end it
        // anyway when this process leaves the bus.
        if let Some(worker) = self.worker.borrow().as_ref() {
            worker.stop();
        }
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::Reply;

    fn prompt(question: Question) -> Prompt {
        Prompt {
            device: "/d".to_string(),
            question,
            reply: Reply::new(),
        }
    }

    #[test]
    fn a_passkey_is_shown_as_six_digits() {
        assert_eq!(digits(42), "000042");
        assert_eq!(digits(123456), "123456");
        assert_eq!(digits(4_000_000_000), "000000");
    }

    #[test]
    fn each_question_gets_the_dialog_it_needs() {
        let waiting = pairing_dialog("Mouse", None);
        assert!(waiting.confirm.is_empty() && !waiting.input);
        assert!(waiting.title.contains("\u{2068}Mouse\u{2069}"));

        let confirm = pairing_dialog("Mouse", Some(&prompt(Question::ConfirmPasskey(7))));
        assert_eq!(confirm.code, "000007");
        // Saying yes to a number is not to be the default button.
        assert!(confirm.danger && !confirm.confirm.is_empty());

        let pin = pairing_dialog("Mouse", Some(&prompt(Question::EnterPin)));
        assert!(pin.input && pin.masked);

        // A number to type on the device has nothing to confirm here.
        let shown = pairing_dialog("Mouse", Some(&prompt(Question::ShowPasskey(123456))));
        assert_eq!(shown.code, "123456");
        assert!(shown.confirm.is_empty());
        let shown = pairing_dialog("Mouse", Some(&prompt(Question::ShowPin("ab12".into()))));
        assert_eq!(shown.code, "ab12");
    }
}
