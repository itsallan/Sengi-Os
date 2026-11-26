//! sengi-edit: opens the Text Editor on the file given, or on an empty
//! page.

use std::process::ExitCode;

use sengi_apps::env::Env;
use sengi_edit::app;
use sengi_edit::ui::EditWindow;

/// What the dock and the compositor know the window by. The `.desktop`
/// file has the same name.
const APP_ID: &str = "sengi-edit";

const USAGE: &str = "usage: sengi-edit [FILE]\n\nOpens the Sengi OS text editor on FILE (a path or a file:// address), or on an\nempty page. FILE is made when you save it if it does not exist.\n";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("sengi-edit {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) if other.starts_with('-') && other != "-" => {
            eprintln!("sengi-edit: unknown option {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
        Some(_) => {}
    }
    if args.len() > 1 {
        eprintln!("sengi-edit: only the first file is opened, one window per file");
    }

    let env = Env::from_process();
    let cwd = std::env::current_dir().unwrap_or_else(|_| env.home.clone());
    let start = match app::start_for(&env.home, args.first().map(String::as_str), &cwd) {
        Ok(start) => start,
        Err(why) => {
            eprintln!("sengi-edit: {why}");
            return ExitCode::FAILURE;
        }
    };

    let window = match EditWindow::new() {
        Ok(window) => window,
        Err(error) => {
            eprintln!("sengi-edit: cannot open a window: {error}");
            return ExitCode::FAILURE;
        }
    };
    sengi_config::apply_theme!(
        slint::ComponentHandle::global::<sengi_edit::ui::ThemeSwitch>(&window)
    );
    // After the first component, which is what brings the backend up, and
    // before the window is shown.
    if let Err(error) = slint::set_xdg_app_id(APP_ID) {
        eprintln!("sengi-edit: could not set the application id: {error}");
    }
    match app::run(window, start) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-edit: {error}");
            ExitCode::FAILURE
        }
    }
}
