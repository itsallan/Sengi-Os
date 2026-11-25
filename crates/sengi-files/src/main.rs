//! sengi-files: opens the Files window, on the folder given or on home.

use std::process::ExitCode;

use sengi_apps::env::Env;
use sengi_files::app;
use sengi_files::ui::FilesWindow;

/// What the dock and the compositor know the window by. The `.desktop`
/// file has the same name.
const APP_ID: &str = "sengi-files";

const USAGE: &str = "usage: sengi-files [FOLDER]\n\nOpens the Sengi OS file manager on FOLDER, or on your home folder.\n";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut folder = None;
    match args.first().map(String::as_str) {
        None => {}
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("sengi-files {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) if other.starts_with('-') && other != "-" => {
            eprintln!("sengi-files: unknown option {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
        Some(path) => folder = Some(path.to_string()),
    }

    let env = Env::from_process();
    let cwd = std::env::current_dir().unwrap_or_else(|_| env.home.clone());
    let start = app::start_for(&env, folder.as_deref(), &cwd);

    let window = match FilesWindow::new() {
        Ok(window) => window,
        Err(error) => {
            eprintln!("sengi-files: cannot open a window: {error}");
            return ExitCode::FAILURE;
        }
    };
    slint::ComponentHandle::global::<sengi_files::ui::ThemeSwitch>(&window)
        .set_light(sengi_config::theme_is_light());
    // After the first component, which is what brings the backend up, and
    // before the window is shown.
    if let Err(error) = slint::set_xdg_app_id(APP_ID) {
        eprintln!("sengi-files: could not set the application id: {error}");
    }
    match app::run(window, start) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-files: {error}");
            ExitCode::FAILURE
        }
    }
}
