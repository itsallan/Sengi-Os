//! sengi-view: opens the Image Viewer on the picture given.

use std::process::ExitCode;

use sengi_view::app;
use sengi_view::ui::ViewWindow;

/// What the dock and the compositor know the window by. The `.desktop`
/// file has the same name.
const APP_ID: &str = "sengi-view";

const USAGE: &str = "usage: sengi-view [FILE]\n\nOpens the Sengi OS image viewer on the PNG or JPEG FILE (a path or a file://\naddress). Left and Right go through the pictures in the same folder.\n";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("sengi-view {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) if other.starts_with('-') && other != "-" => {
            eprintln!("sengi-view: unknown option {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
        Some(_) => {}
    }
    if args.len() > 1 {
        eprintln!("sengi-view: only the first file is opened, one window per file");
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let path = match app::start_for(args.first().map(String::as_str), &cwd) {
        Ok(path) => path,
        Err(why) => {
            eprintln!("sengi-view: {why}");
            return ExitCode::FAILURE;
        }
    };

    let window = match ViewWindow::new() {
        Ok(window) => window,
        Err(error) => {
            eprintln!("sengi-view: cannot open a window: {error}");
            return ExitCode::FAILURE;
        }
    };
    slint::ComponentHandle::global::<sengi_view::ui::ThemeSwitch>(&window)
        .set_light(sengi_config::theme_is_light());
    // After the first component, which is what brings the backend up, and
    // before the window is shown.
    if let Err(error) = slint::set_xdg_app_id(APP_ID) {
        eprintln!("sengi-view: could not set the application id: {error}");
    }
    match app::run(window, path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-view: {error}");
            ExitCode::FAILURE
        }
    }
}
