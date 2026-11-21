//! sengi-quick, the Sengi OS quick settings panel.
//!
//! Started when a status item in the bar is clicked, gone when dismissed.
//! The logic is in the library; this file makes sure only one panel is
//! open, starts the service workers, and puts the panel on a layer-shell
//! surface through sengi-layer.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use sengi_layer::{Anchor, App, Config, KeyboardMode, Layer};
use sengi_quick::cli::{self, Action};
use sengi_quick::commands;
use sengi_quick::instance::{self, Claim, Instance};
use sengi_quick::panel::Panel;
use sengi_quick::ui::{self, Exit, QuickWindow};
use sengi_system::services::{Event, Services, Workers};

/// Set to have the panel say on standard error how long each step of
/// starting up took.
const TRACE_VARIABLE: &str = "SENGI_QUICK_TRACE";

/// How long closing may wait for the last volume or brightness change to
/// be applied. One `wpctl` run or one call to logind takes a few
/// milliseconds; this is for when something is stuck.
const LAST_WORDS: Duration = Duration::from_millis(500);

/// Something to do to the window once it is built. Nothing, except in a
/// demo, which presses a few things to set its scene.
type Staging = Box<dyn FnOnce(&QuickWindow)>;

fn main() -> ExitCode {
    let started = Instant::now();
    // First, before a passphrase can be anywhere in memory.
    sengi_system::set_program_name("sengi-quick");
    sengi_system::secret::forbid_core_dumps();
    let action = match cli::parse(std::env::args().skip(1)) {
        Ok(action) => action,
        Err(message) => {
            eprintln!("sengi-quick: {message}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let (services, staging): (Services, Staging) = match action {
        Action::Help => {
            print!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Action::Version => {
            println!("sengi-quick {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Action::Toggle => (sengi_system::real_services(), Box::new(|_| {})),
        #[cfg(debug_assertions)]
        Action::Demo(name) => match sengi_quick::demo::scenario(&name) {
            Some(demo) => {
                let script = demo.script;
                (demo.services, Box::new(move |window| play(window, script)))
            }
            None => {
                eprintln!(
                    "sengi-quick: no demo called {name}. There are: {}",
                    sengi_quick::demo::SCENARIOS.join(", ")
                );
                return ExitCode::from(2);
            }
        },
    };

    // Before anything slow, so a second click closes the panel at once.
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    let instance = match instance::claim(runtime_dir.as_deref()) {
        Claim::Granted(instance) => instance,
        Claim::Toggled => return ExitCode::SUCCESS,
    };

    let exit = match open(&instance, services, staging, started) {
        Ok(exit) => exit,
        Err(error) => {
            eprintln!("sengi-quick: {error}");
            return ExitCode::FAILURE;
        }
    };
    // The socket goes before the next program starts, so that program can
    // open a panel of its own.
    drop(instance);
    match exit {
        Exit::Dismissed => {}
        Exit::Lock => spawn(commands::LOCK),
        Exit::Session => spawn(commands::SESSION),
    }
    ExitCode::SUCCESS
}

/// Runs a shell command without waiting for it. The extra shell exits at
/// once, so the command is adopted by the session and outlives the panel.
fn spawn(command: &str) {
    let result = Command::new("sh")
        .arg("-c")
        .arg(format!("{command} >/dev/null 2>&1 &"))
        .stdin(Stdio::null())
        .status();
    if let Err(error) = result {
        eprintln!("sengi-quick: could not run {command}: {error}");
    }
}

fn kind(event: &Event) -> &'static str {
    match event {
        Event::Wifi(_) => "Wi-Fi",
        Event::Joined { .. } => "join result",
        Event::Bluetooth(_) => "Bluetooth",
        Event::Volume { .. } => "volume",
        Event::Hardware { .. } => "hardware",
        Event::Dnd(_) => "do not disturb",
        _ => "something new",
    }
}

/// Show the panel and run until it is dismissed.
fn open(
    instance: &Instance,
    services: Services,
    staging: Staging,
    started: Instant,
) -> Result<Exit, sengi_layer::Error> {
    let tracing = std::env::var_os(TRACE_VARIABLE).is_some();
    let trace = move |what: &str| {
        if tracing {
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            eprintln!("sengi-quick: {elapsed:7.1} ms  {what}");
        }
    };

    // These are a few small files and a look along PATH: quick enough to
    // do before the window exists, so that tiles for hardware the machine
    // lacks are never drawn.
    let hardware = services.machine.read();
    let panel = Panel::new(hardware, services.dnd.available());
    trace("hardware read");

    let app = App::new()?;
    let handle = app.handle();
    trace("connected to the compositor");

    // A later invocation asks us to close through the instance socket.
    let close = handle.channel({
        let handle = handle.clone();
        move |()| handle.quit()
    })?;
    instance.on_close_request(move || {
        let _ = close.send(());
    });

    // The workers start now and answer whenever they can; the panel is
    // drawn without waiting for any of them.
    type Apply = Box<dyn Fn(Event)>;
    let apply: Rc<RefCell<Option<Apply>>> = Rc::default();
    let events = handle.channel({
        let apply = apply.clone();
        move |event: Event| {
            trace(kind(&event));
            if let Some(apply) = apply.borrow().as_ref() {
                apply(event);
            }
        }
    })?;
    let workers = Workers::start(services, move |event| events.send(event).is_ok());

    // Above everything, with the keyboard, and covering the whole output
    // (bar included) so that a click anywhere outside the panel reaches us
    // and closes it. The component draws the panel in the corner and leaves
    // the rest transparent.
    let config = Config::new("sengi-quick")
        .layer(Layer::Overlay)
        .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
        .size(0, 0)
        .exclusive_zone(-1)
        .keyboard(KeyboardMode::Exclusive);

    let exit = Rc::new(Cell::new(Exit::Dismissed));
    let workers = Rc::new(workers);
    let mut parts = Some((panel, workers.clone(), staging));
    app.run(config, {
        let exit = exit.clone();
        move |context| {
            let (panel, workers, staging) = parts.take().ok_or("the panel has a single surface")?;
            let window = QuickWindow::new()?;
            let handle = context.handle();
            // The first reason to close is the one that counts: closing
            // after a click on Lock also loses the keyboard.
            let finish = Rc::new({
                let (exit, closing) = (exit.clone(), Cell::new(false));
                move |reason| {
                    if !closing.replace(true) {
                        exit.set(reason);
                        handle.quit();
                    }
                }
            });
            let events = ui::attach(&window, panel, move |command| workers.send(command), {
                let finish = finish.clone();
                move |reason| finish(reason)
            });
            *apply.borrow_mut() = Some(Box::new(events));

            // A panel nobody can type into, because the keyboard went to
            // something else, has been left behind.
            context.surface().on_focus_changed(move |focused| {
                if !focused {
                    finish(Exit::Dismissed);
                }
            });

            staging(&window);
            trace("panel built");
            Ok(window)
        }
    })?;
    // The panel is off the screen. Before the process goes, let the audio
    // and brightness workers finish what the sliders last asked for.
    workers.finish(LAST_WORDS);
    Ok(exit.get())
}

/// Carry out a demo's script, a step at a time, once the panel is up and
/// the made-up services have answered.
#[cfg(debug_assertions)]
fn play(window: &QuickWindow, script: Vec<sengi_quick::demo::Action>) {
    use sengi_quick::demo;
    use slint::ComponentHandle;
    const PAUSE: Duration = Duration::from_millis(300);

    let weak = window.as_weak();
    for (index, action) in script.into_iter().enumerate() {
        let weak = weak.clone();
        slint::Timer::single_shot(PAUSE * (index as u32 + 1), move || {
            let Some(window) = weak.upgrade() else { return };
            match action {
                demo::Action::Choose(row) => {
                    use slint::Model;
                    if let Some(item) = window.get_networks().row_data(row) {
                        window.invoke_network_chosen(item.id);
                    }
                }
                demo::Action::Submit(passphrase) => {
                    window.invoke_passphrase_submitted(passphrase.into())
                }
            }
        });
    }
}
