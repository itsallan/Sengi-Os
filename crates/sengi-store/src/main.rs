//! sengi-store: opens the store window. Not a service: it starts when
//! asked for and is gone when its window is closed.

use std::process::ExitCode;

use sengi_apps::env::Env;
use sengi_store::flatpak::Flatpak;
use sengi_store::ui::StoreWindow;

/// What the dock and the compositor know the window by. The `.desktop`
/// file has the same name.
const APP_ID: &str = "sengi-store";

const USAGE: &str = "usage: sengi-store\n\nOpens the Sengi OS store.\n";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The icons are decoded by running this program again; see
    // sengi_apps::icons::Decoder.
    if let Some(code) = sengi_apps::icons::run_decoder(&args) {
        return code;
    }
    match args.first().map(String::as_str) {
        None => {}
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("sengi-store {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("sengi-store: unknown option {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }

    let window = match StoreWindow::new() {
        Ok(window) => window,
        Err(error) => {
            eprintln!("sengi-store: cannot open a window: {error}");
            return ExitCode::FAILURE;
        }
    };
    slint::ComponentHandle::global::<sengi_store::ui::ThemeSwitch>(&window)
        .set_light(sengi_config::theme_is_light());
    // After the first component, which is what brings the backend up, and
    // before the window is shown.
    if let Err(error) = slint::set_xdg_app_id(APP_ID) {
        eprintln!("sengi-store: could not set the application id: {error}");
    }
    let data_home = Env::from_process().data_home;
    match app_run(window, data_home) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-store: {error}");
            ExitCode::FAILURE
        }
    }
}

fn app_run(window: StoreWindow, data_home: std::path::PathBuf) -> Result<(), slint::PlatformError> {
    sengi_store::app::run(window, Flatpak::default(), data_home)
}
