//! sengi-calc: opens the Calculator.

use std::process::ExitCode;

use sengi_calc::app;
use sengi_calc::ui::{CalcWindow, ThemeSwitch};
use slint::ComponentHandle;

/// What the dock and the compositor know the window by. The `.desktop`
/// file has the same name.
const APP_ID: &str = "sengi-calc";

const USAGE: &str = "usage: sengi-calc\n\nOpens the Sengi OS calculator. Type with the keyboard: digits, + - * / ( ) %,\nReturn or = for the answer, Backspace, Escape to clear, Ctrl+C to copy.\n";

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("sengi-calc {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("sengi-calc: unexpected argument {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    let window = match CalcWindow::new() {
        Ok(window) => window,
        Err(error) => {
            eprintln!("sengi-calc: cannot open a window: {error}");
            return ExitCode::FAILURE;
        }
    };
    sengi_config::apply_theme!(window.global::<ThemeSwitch>());
    // After the first component, which is what brings the backend up, and
    // before the window is shown.
    if let Err(error) = slint::set_xdg_app_id(APP_ID) {
        eprintln!("sengi-calc: could not set the application id: {error}");
    }
    match app::run(window) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-calc: {error}");
            ExitCode::FAILURE
        }
    }
}
