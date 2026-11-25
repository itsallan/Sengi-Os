//! sengi-launcher, the Sengi OS application search and menu.
//!
//! The logic is in the library; this file parses the command line, makes
//! sure only one panel is open, and puts the panel on a layer-shell surface
//! through sengi-layer.

use std::cell::{Cell, RefCell};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Mutex;

use sengi_apps::decoder::Pixels;
use sengi_apps::env::Env;
use sengi_launcher::cli::{self, Action, Invocation, Mode};
use sengi_launcher::icons;
use sengi_launcher::instance::{self, Claim, Instance};
use sengi_launcher::pinned::DockPins;
use sengi_launcher::run::{self, Source};
use sengi_launcher::session::Session;
use sengi_launcher::ui::{self, LauncherWindow, Outcome, Pins};
use sengi_layer::{Anchor, App, Config, KeyboardMode, Layer};

fn main() -> ExitCode {
    let invocation = match cli::parse(std::env::args().skip(1)) {
        Ok(invocation) => invocation,
        Err(message) => {
            eprintln!("sengi-launcher: {message}\n\n{}", cli::USAGE);
            return ExitCode::from(run::FAILED);
        }
    };
    match &invocation.action {
        Action::Help => {
            print!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        Action::Version => {
            println!("sengi-launcher {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Action::List(query) => ExitCode::from(list(&invocation, query)),
        Action::Show => ExitCode::from(show(&invocation)),
    }
}

fn load(invocation: &Invocation, env: &Env) -> Option<(Source, Session)> {
    match run::load(invocation, env, std::io::stdin().lock()) {
        Ok(loaded) => Some(loaded),
        Err(error) => {
            eprintln!("sengi-launcher: reading the menu: {error}");
            None
        }
    }
}

fn list(invocation: &Invocation, query: &str) -> u8 {
    let env = Env::from_process();
    let Some((source, mut session)) = load(invocation, &env) else {
        return run::FAILED;
    };
    print!("{}", run::list(&source, &mut session, query));
    run::CHOSEN
}

fn show(invocation: &Invocation) -> u8 {
    let env = Env::from_process();
    // Before anything slow, so a second press of the key closes the panel
    // at once.
    let instance = match instance::claim(env.runtime_dir.as_deref(), invocation.mode) {
        Claim::Granted(instance) => instance,
        Claim::Toggled => {
            return match invocation.mode {
                Mode::Apps => run::CHOSEN,
                Mode::Menu => run::DISMISSED,
            };
        }
    };
    let Some((source, session)) = load(invocation, &env) else {
        return run::FAILED;
    };
    // Loading can be slow when a menu's lines come from a slow command. If
    // the key was pressed again meanwhile, the panel is already unwanted.
    if instance.close_requested() {
        return run::DISMISSED;
    }

    let check = run::checker(&source, &env);
    // Only the application search can change the dock.
    let pins: Option<Rc<dyn Pins>> = match &source {
        Source::Apps(apps) => Some(Rc::new(DockPins::from_process(apps.entries()))),
        Source::Menu(_) => None,
    };
    match open(invocation, &instance, session, check, pins) {
        Ok(outcome) => run::complete(outcome, source, &env, std::io::stdout()),
        Err(error) => {
            eprintln!("sengi-launcher: {error}");
            run::FAILED
        }
    }
}

/// Show the panel and run until a result is picked or it is dismissed.
fn open(
    invocation: &Invocation,
    instance: &Instance,
    session: Session,
    check: Box<dyn Fn(usize) -> Result<(), String>>,
    pins: Option<Rc<dyn Pins>>,
) -> Result<Outcome, sengi_layer::Error> {
    let app = App::new()?;
    let handle = app.handle();

    // A later invocation asks us to close through the instance socket.
    let close = handle.channel({
        let handle = handle.clone();
        move |()| handle.quit()
    })?;
    instance.on_close_request(move || {
        let _ = close.send(());
    });

    // Above everything, typing goes nowhere else, and the surface covers the
    // whole output (bar and dock included) so that a click anywhere outside
    // the panel reaches us and closes it. The component draws the panel in
    // the middle and leaves the rest transparent.
    let config = Config::new("sengi-launcher")
        .layer(Layer::Overlay)
        .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
        .size(0, 0)
        .exclusive_zone(-1)
        .keyboard(KeyboardMode::Exclusive);

    let outcome = Rc::new(Cell::new(Outcome::Dismissed));
    let invocation = invocation.clone();
    // Icons are decoded in a thread of their own and come back here, to the
    // panel once it exists.
    let sink: Rc<RefCell<Option<ui::IconSink>>> = Rc::default();
    let to_ui = handle.channel({
        let sink = sink.clone();
        move |(index, pixels): (usize, Option<Pixels>)| {
            if let Some(sink) = sink.borrow().as_ref() {
                sink(index, pixels);
            }
        }
    })?;
    let requests = ui::icon_requests(&session);
    let mut parts = Some((session, check, to_ui, requests, pins));
    app.run(config, {
        let outcome = outcome.clone();
        move |context| {
            let (session, check, to_ui, requests, pins) =
                parts.take().ok_or("the launcher has a single surface")?;
            let window = LauncherWindow::new()?;
            slint::ComponentHandle::global::<ui::ThemeSwitch>(&window)
                .set_light(sengi_config::theme_is_light());
            let handle = context.handle();
            let icon_sink = ui::attach_with_pins(
                &window,
                session,
                &invocation,
                check,
                {
                    let (handle, outcome) = (handle.clone(), outcome.clone());
                    move |result| {
                        outcome.set(result);
                        handle.quit();
                    }
                },
                pins,
            );
            *sink.borrow_mut() = Some(icon_sink);
            if !requests.is_empty() {
                let to_ui = Mutex::new(to_ui);
                icons::start(
                    requests,
                    ui::icon_pixels(&window),
                    icons::decoder(),
                    move |index, pixels| {
                        let _ = to_ui.lock().unwrap().send((index, pixels));
                    },
                );
            }
            Ok(window)
        }
    })?;
    Ok(outcome.get())
}
