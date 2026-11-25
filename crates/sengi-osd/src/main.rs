//! sengi-osd, the level indicator for the volume and brightness keys.
//!
//! One run that finds no indicator open puts the panel on a layer-shell
//! surface that takes neither keyboard nor pointer. A thread waits on the
//! socket of `instance`: a level from a later run goes to the panel, and a
//! quiet moment ends the program.

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use sengi_layer::{Anchor, App, Config, KeyboardMode, Layer, Margins};
use sengi_osd::cli::{self, Invocation};
use sengi_osd::instance::{self, Claim, Wait};
use sengi_osd::level::Level;
use sengi_osd::ui::{self, OsdWindow};
use sengi_tokens::Tokens;

/// What the loop is told by the thread that waits on the socket.
enum Message {
    Show(Level),
    Quit,
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let level = match cli::parse(&arguments) {
        Ok(Invocation::Help) => {
            print!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Invocation::Version) => {
            println!("sengi-osd {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(Invocation::Show(level)) => level,
        Ok(Invocation::Volume) => match cli::read_volume() {
            Some(level) => level,
            None => {
                eprintln!("sengi-osd: could not read the volume");
                return ExitCode::FAILURE;
            }
        },
        Err(message) => {
            eprintln!("sengi-osd: {message}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    match show(level) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-osd: {error}");
            ExitCode::FAILURE
        }
    }
}

fn show(level: Level) -> Result<(), Box<dyn std::error::Error>> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let open = match instance::claim(runtime_dir.as_deref(), &level) {
        Claim::Delivered => return Ok(()),
        Claim::Open(open) => open,
    };
    let tokens = Tokens::builtin();
    let px = |name: &str, fallback: u32| tokens.px(name).unwrap_or(fallback);
    let (width, height) = (px("osd.width", 240), px("osd.height", 56));
    let margin = px("osd.margin", 96) as i32;
    let time = Duration::from_millis(u64::from(px("timing.osd-timeout", 1200)));

    let app = match App::new() {
        Ok(app) => app,
        Err(error) => {
            open.give_up();
            return Err(error.into());
        }
    };
    let handle = app.handle();
    let window: Rc<RefCell<slint::Weak<OsdWindow>>> = Rc::default();
    let sender = handle.clone().channel({
        let (window, handle) = (window.clone(), handle.clone());
        move |message| match message {
            Message::Show(level) => {
                if let Some(window) = window.borrow().upgrade() {
                    ui::apply(&window, &level);
                }
            }
            Message::Quit => handle.quit(),
        }
    })?;
    thread::Builder::new()
        .name("wait".into())
        .spawn(move || loop {
            match open.wait(time) {
                Wait::Level(level) => {
                    if sender.send(Message::Show(level)).is_err() {
                        open.give_up();
                        return;
                    }
                }
                Wait::Over => {
                    let _ = sender.send(Message::Quit);
                    return;
                }
            }
        })?;

    let config = Config::new("sengi-osd")
        .layer(Layer::Overlay)
        .anchor(Anchor::BOTTOM)
        .size(width, height)
        .margins(Margins {
            bottom: margin,
            ..Margins::default()
        })
        // Measured from the edge of the screen, not from the dock's zone.
        .exclusive_zone(-1)
        .keyboard(KeyboardMode::None);
    app.run(config, move |context| {
        let osd = OsdWindow::new()?;
        ui::apply(&osd, &level);
        // Nothing here is for the pointer: clicks go to what is behind.
        context.surface().set_input_region(Some(&[]));
        *window.borrow_mut() = slint::ComponentHandle::as_weak(&osd);
        Ok(osd)
    })?;
    Ok(())
}
