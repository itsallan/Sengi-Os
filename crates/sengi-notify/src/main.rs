//! sengi-notify, the Sengi OS notification service.
//!
//! It owns `org.freedesktop.Notifications` on the session bus, shows what
//! arrives as cards under the top bar, and keeps a short history for the
//! notification centre. The bus is served from its own thread; the cards
//! are a Slint window on a layer surface that sengi-layer runs, mapped only
//! while there is something to see.

mod cli;
mod dbus;
mod intake;
mod markup;
mod notification;
mod rate;
mod store;
mod ui;
mod wire;

use std::cell::{Cell, RefCell};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use sengi_layer::{Anchor, App, Config, Layer};

use cli::Invocation;
use dbus::{Deliver, Link, Pending, StartError, ToUi};

/// Used if the tokens ever lose `timing.notification-timeout`.
const FALLBACK_TIMEOUT: u64 = 5_000;

/// Exit statuses that say why the service is not running, for whatever
/// supervises it. 1 is any other failure and 2 a bad command line.
const EXIT_NAME_TAKEN: u8 = 3;
const EXIT_NAME_LOST: u8 = 4;
const EXIT_BUS_CLOSED: u8 = 5;

/// How long a call that arrives before the UI loop exists waits for it.
const UI_WAIT: Duration = Duration::from_secs(10);

fn default_timeout() -> u64 {
    sengi_tokens::Tokens::builtin()
        .px("timing.notification-timeout")
        .map_or(FALLBACK_TIMEOUT, u64::from)
}

/// Where the bus thread finds the way into the UI loop, once there is one.
type Gate = Arc<(Mutex<Option<sengi_layer::Sender<ToUi>>>, Condvar)>;

/// Calls can arrive from the moment the name is ours, which is a little
/// before the compositor connection and the UI loop exist. They wait here.
fn through(gate: Gate) -> Deliver {
    Arc::new(move |message| {
        let (slot, opened) = &*gate;
        let Ok(slot) = slot.lock() else {
            return false;
        };
        let Ok((slot, _)) = opened.wait_timeout_while(slot, UI_WAIT, |slot| slot.is_none()) else {
            return false;
        };
        slot.as_ref().is_some_and(|ui| ui.send(message).is_ok())
    })
}

fn serve() -> Result<ExitCode, Box<dyn std::error::Error>> {
    // The name comes first. If someone else has it there is nothing for
    // this process to do, and no reason to talk to the compositor at all.
    let gate = Gate::default();
    let pending = Pending::default();
    let link = Link::new(through(gate.clone()), pending.clone());
    let (signals, signals_out) = mpsc::channel();
    let (ready, started) = mpsc::channel();
    let timeout = default_timeout();
    thread::Builder::new()
        .name("bus".into())
        .spawn(move || dbus::serve(link, signals_out, timeout, ready))?;
    match started.recv() {
        Ok(Ok(())) => {}
        Ok(Err(StartError::NameTaken)) => {
            eprintln!("sengi-notify: a notification service is already running");
            return Ok(ExitCode::from(EXIT_NAME_TAKEN));
        }
        Ok(Err(error)) => return Err(error.to_string().into()),
        Err(_) => return Err("the bus thread ended before it started".into()),
    }

    let app = App::new()?;
    let handle = app.handle();
    // Nothing to show yet, so no surface yet.
    handle.hide();

    let ui: Rc<RefCell<Option<Rc<RefCell<ui::Ui>>>>> = Rc::default();
    let exit = Rc::new(Cell::new(0));
    let to_ui = {
        let (ui, exit, handle) = (ui.clone(), exit.clone(), handle.clone());
        handle.clone().channel(move |message| {
            pending.handled();
            match message {
                ToUi::NameLost => {
                    eprintln!("sengi-notify: another notification service took over");
                    exit.set(EXIT_NAME_LOST);
                    handle.quit();
                }
                ToUi::BusClosed => {
                    eprintln!("sengi-notify: the session bus is gone");
                    exit.set(EXIT_BUS_CLOSED);
                    handle.quit();
                }
                message => {
                    if let Some(ui) = ui.borrow().as_ref() {
                        ui.borrow_mut().receive(message);
                    }
                }
            }
        })?
    };
    if let Ok(mut slot) = gate.0.lock() {
        *slot = Some(to_ui);
    }
    gate.1.notify_all();

    let config = Config::new("sengi-notify")
        .layer(Layer::Overlay)
        .anchor(Anchor::TOP | Anchor::RIGHT);
    app.run(config, move |context| {
        let window = ui::NotifyWindow::new()?;
        let attached = ui::Ui::attach(&window, context.handle(), signals.clone());
        let surface = context.surface();
        // The centre holds the keyboard while it is open. If something
        // takes it away, a screen locker for one, the centre closes.
        surface.on_focus_changed({
            let attached = Rc::downgrade(&attached);
            move |focused| {
                let Some(attached) = attached.upgrade() else {
                    return;
                };
                if focused {
                    return;
                }
                // Hiding the surface ourselves also ends up here, with the
                // UI already at work; then there is nothing to add.
                if let Ok(mut ui) = attached.try_borrow_mut() {
                    ui.close_centre();
                };
            }
        });
        // The strip of cards is never taller than the screen it is on.
        let height = |output: Option<sengi_layer::Output>| {
            output
                .and_then(|output| output.size)
                .map(|(_, height)| height)
        };
        attached
            .borrow_mut()
            .set_output_height(height(surface.output()));
        surface.on_output_changed({
            let attached = Rc::downgrade(&attached);
            move |output| {
                let Some(attached) = attached.upgrade() else {
                    return;
                };
                if let Ok(mut ui) = attached.try_borrow_mut() {
                    ui.set_output_height(height(Some(output)));
                };
            }
        });
        *ui.borrow_mut() = Some(attached);
        Ok(window)
    })?;
    Ok(ExitCode::from(exit.get()))
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let invocation = match cli::parse(&arguments) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("sengi-notify: {error}");
            eprint!("{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let unreachable = |error: zbus::Error| -> Box<dyn std::error::Error> {
        format!("could not reach the service: {error}").into()
    };
    let result: Result<ExitCode, Box<dyn std::error::Error>> = match invocation {
        Invocation::Help => {
            print!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Invocation::Version => {
            println!("sengi-notify {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Invocation::Serve => serve(),
        Invocation::Send(command) => dbus::send(command)
            .map(|()| ExitCode::SUCCESS)
            .map_err(unreachable),
        Invocation::DndStatus => dbus::dnd_status()
            .map(|on| {
                println!("{}", if on { "on" } else { "off" });
                ExitCode::SUCCESS
            })
            .map_err(unreachable),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("sengi-notify: {error}");
            ExitCode::FAILURE
        }
    }
}
