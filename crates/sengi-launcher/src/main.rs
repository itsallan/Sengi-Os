//! sengi-launcher, the Sengi OS application search and menu.
//!
//! The logic is in the library; this file parses the command line, makes
//! sure only one panel is open, and puts the panel on a layer-shell surface
//! through sengi-layer.

use std::cell::Cell;
use std::process::ExitCode;
use std::rc::Rc;

use sengi_launcher::cli::{self, Action, Invocation, Mode};
use sengi_launcher::env::Env;
use sengi_launcher::instance::{self, Claim, Instance};
use sengi_launcher::run::{self, Source};
use sengi_launcher::session::Session;
use sengi_launcher::ui::{self, LauncherWindow, Outcome};
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

    match open(invocation, &env, instance, session) {
        Ok(outcome) => run::complete(outcome, source, &env, std::io::stdout()),
        Err(error) => {
            eprintln!("sengi-launcher: {error}");
            run::FAILED
        }
    }
}

fn pixels(length: f32) -> u32 {
    length.round().max(1.0) as u32
}

/// Show the panel and run until a result is picked or it is dismissed.
fn open(
    invocation: &Invocation,
    env: &Env,
    instance: Instance,
    session: Session,
) -> Result<Outcome, sengi_layer::Error> {
    let app = App::new()?;
    let handle = app.handle();

    // A later invocation asks us to close through the instance socket.
    let replaced = Rc::new(Cell::new(false));
    let close = handle.channel({
        let (handle, replaced) = (handle.clone(), replaced.clone());
        move |()| {
            replaced.set(true);
            handle.quit();
        }
    })?;
    instance.on_close_request(move || {
        let _ = close.send(());
    });

    // Centred, above everything, and typing goes nowhere else.
    let config = Config::new("sengi-launcher")
        .layer(Layer::Overlay)
        .anchor(Anchor::NONE)
        .keyboard(KeyboardMode::Exclusive);

    let outcome = Rc::new(Cell::new(Outcome::Dismissed));
    let prompt = invocation.prompt().to_owned();
    let compact = invocation.compact();
    let mut session = Some(session);
    let result = app.run(config, {
        let outcome = outcome.clone();
        move |context| {
            let session = session.take().ok_or("the launcher has a single surface")?;
            let window = LauncherWindow::new()?;
            window.set_compact(compact);
            let handle = context.handle();
            ui::attach(&window, session, &prompt, {
                let (handle, outcome) = (handle.clone(), outcome.clone());
                move |result| {
                    outcome.set(result);
                    handle.quit();
                }
            });
            // Tall enough for a full list and see-through below a short one,
            // so the search field does not move as the results change.
            handle.set_size(
                pixels(window.get_panel_width()),
                pixels(window.get_full_height()),
            )?;
            Ok(window)
        }
    });

    // If we were asked to close, the socket may already be someone else's.
    if !replaced.get() {
        instance::release(env.runtime_dir.as_deref());
    }
    result?;
    Ok(outcome.get())
}
