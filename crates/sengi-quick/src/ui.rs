//! Glue between the Slint window and a [`Panel`]. Creating the surface and
//! running the event loop is left to the caller.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::panel::{NetworkRow, Panel, View};
use crate::secret::Secret;
use crate::services::{Command, Event};

slint::include_modules!();

/// How the panel was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Dismissed,
    /// Close, then lock the screen.
    Lock,
    /// Close, then open the session menu.
    Session,
}

fn item(row: &NetworkRow) -> NetworkItem {
    NetworkItem {
        id: row.id.as_str().into(),
        gone: row.gone,
        name: row.name.as_str().into(),
        secured: row.secured,
        known: row.known,
        level: row.level,
        busy: row.busy,
        confirm: row.confirm,
    }
}

/// Bring the list model in line with `rows`, touching only what changed.
/// Replacing the model would rebuild every row and reset the scrolling
/// each time a scan result came in.
fn sync(model: &VecModel<NetworkItem>, rows: &[NetworkRow]) {
    for (index, row) in rows.iter().enumerate() {
        let item = item(row);
        match model.row_data(index) {
            Some(shown) if shown == item => {}
            Some(_) => model.set_row_data(index, item),
            None => model.push(item),
        }
    }
    while model.row_count() > rows.len() {
        model.remove(model.row_count() - 1);
    }
}

fn show(window: &QuickWindow, networks: &VecModel<NetworkItem>, view: &View) {
    let text = |text: &str| SharedString::from(text);

    window.set_wifi_enabled(view.wifi.enabled);
    window.set_wifi_on(view.wifi.on);
    window.set_wifi_detail(text(&view.wifi.detail));
    window.set_wifi_level(view.wifi.level);
    window.set_network_current(view.wifi.current);
    window.set_wired(view.wifi.wired);
    sync(networks, &view.wifi.rows);
    window.set_wifi_note(text(&view.wifi.note));
    window.set_wifi_alert(text(&view.wifi.alert));
    let prompt = view.wifi.prompt.clone().unwrap_or_default();
    window.set_prompt_open(view.wifi.prompt.is_some());
    window.set_prompt_network(text(&prompt.name));
    window.set_prompt_problem(text(&prompt.problem));
    window.set_prompt_busy(prompt.busy);

    window.set_bluetooth_enabled(view.bluetooth.enabled);
    window.set_bluetooth_on(view.bluetooth.on);
    window.set_bluetooth_detail(text(&view.bluetooth.detail));

    window.set_volume_enabled(view.volume.enabled);
    window.set_volume(view.volume.value);
    window.set_volume_label(text(&view.volume.label));
    window.set_muted(view.volume.muted);

    window.set_brightness_visible(view.brightness.visible);
    window.set_brightness_minimum(view.brightness.minimum);
    window.set_brightness(view.brightness.value);
    window.set_brightness_label(text(&view.brightness.label));

    window.set_dnd_visible(view.dnd.visible);
    window.set_dnd_known(view.dnd.state.is_some());
    window.set_dnd_on(view.dnd.state == Some(true));

    window.set_battery_visible(view.battery.visible);
    window.set_battery_percent(view.battery.percent);
    window.set_battery_label(text(&view.battery.label));
    window.set_battery_detail(text(&view.battery.detail));
    window.set_battery_charging(view.battery.charging);
    window.set_battery_low(view.battery.low);
    window.set_battery_warning(view.battery.warning);
}

/// Connect `window` to `panel`.
///
/// What the user asks for goes to `send`, which must not block. `finish`
/// is called once, when the panel should close; closing the window is up
/// to the caller. The returned function takes the events of the services
/// and must be called on the UI thread.
pub fn attach(
    window: &QuickWindow,
    panel: Panel,
    send: impl Fn(Command) + 'static,
    finish: impl Fn(Exit) + 'static,
) -> impl Fn(Event) {
    let networks = Rc::new(VecModel::<NetworkItem>::default());
    window.set_networks(ModelRc::from(networks.clone()));
    show(window, &networks, &panel.view());

    let panel = Rc::new(RefCell::new(panel));
    let weak = window.as_weak();
    // Change the panel, pass on what it asks for, and show the result.
    let act = Rc::new({
        let (panel, networks, weak) = (panel.clone(), networks.clone(), weak.clone());
        move |change: &mut dyn FnMut(&mut Panel) -> Option<Command>| {
            let command = change(&mut panel.borrow_mut());
            if let Some(command) = command {
                send(command);
            }
            if let Some(window) = weak.upgrade() {
                show(&window, &networks, &panel.borrow().view());
            }
        }
    });
    // Only the first reason to close counts: closing the surface takes the
    // keyboard away, which would otherwise read as a dismissal.
    let finished = Cell::new(false);
    let finish = Rc::new(move |exit| {
        if !finished.replace(true) {
            finish(exit);
        }
    });

    window.on_wifi_toggled({
        let act = act.clone();
        move |on| act(&mut |panel| panel.toggle_wifi(on))
    });
    window.on_network_chosen({
        let act = act.clone();
        move |id| act(&mut |panel| panel.choose_network(id.as_str()))
    });
    window.on_network_confirmed({
        let act = act.clone();
        move |id| act(&mut |panel| panel.confirm_network(id.as_str()))
    });
    window.on_list_held({
        let act = act.clone();
        move |held, focused| {
            act(&mut |panel| {
                panel.hold_list(held, focused);
                None
            })
        }
    });
    window.on_list_step({
        let act = act.clone();
        move |delta| {
            let mut moved = false;
            act(&mut |panel| {
                moved = panel.move_selection(delta);
                None
            });
            moved
        }
    });
    window.on_list_activated({
        let act = act.clone();
        move || act(&mut |panel| panel.activate_selection())
    });
    window.on_passphrase_submitted({
        let act = act.clone();
        move |typed| {
            // From here on the passphrase is held only as a Secret.
            let mut secret = Some(Secret::new(typed.to_string()));
            act(&mut |panel| panel.submit_passphrase(secret.take()?));
        }
    });
    window.on_prompt_cancelled({
        let act = act.clone();
        move || {
            act(&mut |panel| {
                panel.cancel_prompt();
                None
            })
        }
    });
    window.on_bluetooth_toggled({
        let act = act.clone();
        move |on| act(&mut |panel| panel.toggle_bluetooth(on))
    });
    window.on_volume_changed({
        let act = act.clone();
        move |percent| act(&mut |panel| panel.set_volume(percent))
    });
    window.on_mute_clicked({
        let act = act.clone();
        move || act(&mut |panel| panel.toggle_mute())
    });
    window.on_brightness_changed({
        let act = act.clone();
        move |percent| act(&mut |panel| panel.set_brightness(percent))
    });
    window.on_dnd_set({
        let act = act.clone();
        move |on| act(&mut |panel| panel.set_dnd(on))
    });
    window.on_lock({
        let finish = finish.clone();
        move || finish(Exit::Lock)
    });
    window.on_session({
        let finish = finish.clone();
        move || finish(Exit::Session)
    });
    window.on_dismiss(move || finish(Exit::Dismissed));

    // Slint moves the focus on Tab and Shift+Tab and offers no other way
    // to ask for it, so the arrow keys are turned into those. The key goes
    // in after the handler that asked has returned.
    window.on_move_focus({
        let weak = weak.clone();
        move |direction| {
            let weak = weak.clone();
            slint::Timer::single_shot(Duration::ZERO, move || {
                let Some(window) = weak.upgrade() else { return };
                let key = if direction < 0 {
                    Key::Backtab
                } else {
                    Key::Tab
                };
                let text: SharedString = key.into();
                let window = window.window();
                window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
                window.dispatch_event(WindowEvent::KeyReleased { text });
            });
        }
    });

    move |event| {
        let mut event = Some(event);
        act(&mut |panel| {
            if let Some(event) = event.take() {
                panel.apply(event);
            }
            None
        })
    }
}
