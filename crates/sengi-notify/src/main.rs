//! sengi-notify, the Sengi OS notification service.
//!
//! It owns `org.freedesktop.Notifications` on the session bus, shows what
//! arrives as cards under the top bar, and keeps a short history for the
//! notification centre. The bus is served from its own thread; the cards
//! are a Slint window on a layer surface that sengi-layer runs, mapped only
//! while there is something to see.

mod cli;
mod dbus;
mod markup;
mod notification;
mod rate;
mod store;
mod ui;
mod wire;

use std::cell::RefCell;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use sengi_layer::{Anchor, App, Config, Layer};

use cli::Invocation;
use dbus::{Pending, StartError};

/// Used if the tokens ever lose `timing.notification-timeout`.
const FALLBACK_TIMEOUT: u64 = 5_000;

fn default_timeout() -> u64 {
    sengi_tokens::Tokens::builtin()
        .px("timing.notification-timeout")
        .map_or(FALLBACK_TIMEOUT, u64::from)
}

fn serve() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let app = App::new()?;
    let handle = app.handle();
    // Nothing to show yet, so no surface yet.
    handle.hide();

    let ui: Rc<RefCell<Option<Rc<RefCell<ui::Ui>>>>> = Rc::default();
    let pending = Pending::default();
    let to_ui = {
        let ui = ui.clone();
        let pending = pending.clone();
        handle.channel(move |message| {
            pending.handled();
            if let Some(ui) = ui.borrow().as_ref() {
                ui.borrow_mut().receive(message);
            }
        })?
    };

    let (signals, signals_out) = mpsc::channel();
    let (ready, started) = mpsc::channel();
    let timeout = default_timeout();
    thread::Builder::new()
        .name("bus".into())
        .spawn(move || dbus::serve(to_ui, pending, signals_out, timeout, ready))?;
    match started.recv() {
        Ok(Ok(())) => {}
        // Not a failure: the session already has its notifications.
        Ok(Err(StartError::NameTaken)) => {
            eprintln!("sengi-notify: a notification service is already running");
            return Ok(ExitCode::SUCCESS);
        }
        Ok(Err(error)) => return Err(error.to_string().into()),
        Err(_) => return Err("the bus thread ended before it started".into()),
    }

    let config = Config::new("sengi-notify")
        .layer(Layer::Overlay)
        .anchor(Anchor::TOP | Anchor::RIGHT);
    app.run(config, move |context| {
        let window = ui::NotifyWindow::new()?;
        *ui.borrow_mut() = Some(ui::Ui::attach(&window, context.handle(), signals.clone()));
        Ok(window)
    })?;
    Ok(ExitCode::SUCCESS)
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
    let result = match invocation {
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
            .map_err(|error| format!("could not reach the service: {error}").into()),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("sengi-notify: {error}");
            ExitCode::FAILURE
        }
    }
}
