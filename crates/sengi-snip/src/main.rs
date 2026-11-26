//! sengi-snip, run by sengi-shot. See `--help`.

use std::cell::{Cell, RefCell};
use std::process::ExitCode;
use std::rc::Rc;

use sengi_layer::{Anchor, Config, KeyboardMode, Layer, Margins};
use sengi_snip::cli::{self, Mode};
use sengi_snip::select::{self, Rect};
use sengi_snip::ui::{CountdownWindow, SnipWindow, ThemeSwitch};
use slint::ComponentHandle;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mode = match cli::parse(&arguments) {
        Ok(mode) => mode,
        Err(message) => {
            eprintln!("sengi-snip: {message}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let result = match mode {
        Mode::Help => {
            print!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Mode::Version => {
            println!("sengi-snip {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Mode::Select => select_area(),
        Mode::Countdown(seconds) => countdown(seconds),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("sengi-snip: {error}");
            ExitCode::from(3)
        }
    }
}

/// True if a rectangle was chosen, and printed.
fn select_area() -> Result<bool, sengi_layer::Error> {
    // Above everything, with the keyboard, over the whole output the
    // compositor picks: the one with the pointer.
    let config = Config::new("sengi-snip")
        .layer(Layer::Overlay)
        .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
        .size(0, 0)
        .exclusive_zone(-1)
        .keyboard(KeyboardMode::Exclusive);
    let output: Rc<RefCell<Option<String>>> = Rc::default();
    let choice: Rc<Cell<Option<Rect>>> = Rc::default();
    sengi_layer::run(config, {
        let (output, choice) = (output.clone(), choice.clone());
        move |context| {
            let window = SnipWindow::new()?;
            sengi_config::apply_theme!(window.global::<ThemeSwitch>());
            *output.borrow_mut() = context.output().and_then(|o| o.name);
            let output = output.clone();
            context.surface().on_output_changed(move |now| {
                *output.borrow_mut() = now.name;
            });
            let handle = context.handle();
            sengi_snip::attach_select(&window, {
                let (handle, choice) = (handle.clone(), choice.clone());
                move |rect| {
                    choice.set(rect);
                    handle.quit();
                }
            });
            // Nobody can type Escape into a screen that lost the keyboard.
            context.surface().on_focus_changed(move |focused| {
                if !focused {
                    handle.quit();
                }
            });
            Ok(window)
        }
    })?;
    match choice.get() {
        Some(rect) => {
            println!(
                "{}",
                select::format_choice(output.borrow().as_deref(), rect)
            );
            Ok(true)
        }
        None => Ok(false),
    }
}

/// True if it ran out, false if it was clicked.
fn countdown(seconds: u32) -> Result<bool, sengi_layer::Error> {
    let timer: Rc<RefCell<Option<slint::Timer>>> = Rc::default();
    let ran_out = Rc::new(Cell::new(false));
    // A small surface at the top of the screen, which takes no keyboard and
    // leaves the rest of the screen to the program being photographed.
    let config = Config::new("sengi-snip-countdown")
        .layer(Layer::Overlay)
        .anchor(Anchor::TOP)
        .size(1, 1)
        .keyboard(KeyboardMode::None);
    sengi_layer::run(config, {
        let (timer, ran_out) = (timer.clone(), ran_out.clone());
        move |context| {
            let window = CountdownWindow::new()?;
            sengi_config::apply_theme!(window.global::<ThemeSwitch>());
            let handle = context.handle();
            handle.set_size(
                window.get_wanted_width().round() as u32,
                window.get_wanted_height().round() as u32,
            )?;
            handle.set_margins(Margins {
                top: window.get_top_margin().round() as i32,
                ..Margins::default()
            });
            *timer.borrow_mut() = Some(sengi_snip::attach_countdown(&window, seconds, {
                let ran_out = ran_out.clone();
                move |finished| {
                    ran_out.set(finished);
                    handle.quit();
                }
            }));
            Ok(window)
        }
    })?;
    Ok(ran_out.get())
}
