//! sengi-calendar: the month popup under the clock. The bar runs it when
//! the clock is clicked. It draws once, wakes for input only, and exits when
//! dismissed.

use std::process::ExitCode;

use sengi_calendar::cal::WeekStart;
use sengi_calendar::ui::{CalendarWindow, ThemeSwitch};
use sengi_calendar::view::{self, View};
use sengi_clock::Clock;
use sengi_config::store::Dirs;
use sengi_config::Settings;
use sengi_layer::{Anchor, Config, KeyboardMode, Layer};
use slint::ComponentHandle;

const USAGE: &str = "\
Usage: sengi-calendar

Opens the calendar popup under the clock. Escape, a click outside it or
the keyboard going elsewhere closes it. Left and Right, or Page Up and
Page Down, change the month; Home or T goes back to today.

  -h, --help     show this help
  -V, --version  show the version
";

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("sengi-calendar {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("sengi-calendar: unexpected argument {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-calendar: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let today = Clock::new()
        .ymd(chrono::Utc::now())
        .ok_or("the clock gave no date")?;
    let (settings, _) = Settings::load(&Dirs::from_env());
    let start = WeekStart::parse(&settings.text("appearance", "week-start"));
    let mut view = Some(View::new(today, start));

    // Above everything, with the keyboard, and over the whole output so a
    // click outside the popup reaches it and closes it.
    let config = Config::new("sengi-calendar")
        .layer(Layer::Overlay)
        .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
        .size(0, 0)
        .exclusive_zone(-1)
        .keyboard(KeyboardMode::Exclusive);
    Ok(sengi_layer::run(config, move |context| {
        let view = view.take().ok_or("the popup has a single surface")?;
        let window = CalendarWindow::new()?;
        sengi_config::apply_theme!(window.global::<ThemeSwitch>());
        let handle = context.handle();
        view::attach(&window, view, {
            let handle = handle.clone();
            move || handle.quit()
        });
        // A popup nobody can type into has been left behind.
        context.surface().on_focus_changed(move |focused| {
            if !focused {
                handle.quit();
            }
        });
        Ok(window)
    })?)
}
