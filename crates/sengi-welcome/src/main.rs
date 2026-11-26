//! sengi-welcome: the first-run welcome of Sengi OS.

use std::process::ExitCode;
use std::rc::Rc;

use sengi_apps::env::Env;
use sengi_config::{Dirs, Settings};
use sengi_welcome::app::{self, Start};
use sengi_welcome::marker;
use sengi_welcome::system::{self, Process};
use sengi_welcome::ui::{ThemeSwitch, WelcomeWindow};

/// What the dock and the compositor know the window by. The `.desktop`
/// file has the same name.
const APP_ID: &str = "sengi-welcome";

const USAGE: &str = "usage: sengi-welcome [--again | --first-run]\n\nOpens the Sengi OS welcome: a few steps to make a new installation yours.\n\n  --again      open it whether or not it has been seen before\n  --first-run  for the session: open it only on an installed system, and only\n               if it has not been seen; otherwise do nothing\n";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = Env::from_process();
    match args.first().map(String::as_str) {
        None | Some("--again") => {}
        Some("--first-run") => {
            let live = marker::running_live();
            if !marker::should_show(live, marker::is_done(&env.state_home)) {
                return ExitCode::SUCCESS;
            }
        }
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("sengi-welcome {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("sengi-welcome: unknown option {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    if args.len() > 1 {
        eprintln!("sengi-welcome: unexpected argument {}\n\n{USAGE}", args[1]);
        return ExitCode::from(2);
    }

    let window = match WelcomeWindow::new() {
        Ok(window) => window,
        Err(error) => {
            eprintln!("sengi-welcome: cannot open a window: {error}");
            return ExitCode::FAILURE;
        }
    };
    sengi_config::apply_theme!(slint::ComponentHandle::global::<ThemeSwitch>(&window));
    // After the first component, which is what brings the backend up, and
    // before the window is shown.
    if let Err(error) = slint::set_xdg_app_id(APP_ID) {
        eprintln!("sengi-welcome: could not set the application id: {error}");
    }
    let (settings, _) = Settings::load(&Dirs::from_env());
    let start = Start {
        state_home: env.state_home,
        net_dir: system::NET_DIR.into(),
        xkb_list: system::XKB_LIST.into(),
        wallpaper_dir: system::WALLPAPER_DIR.into(),
        wallpaper: settings.text("appearance", "wallpaper"),
        layouts: settings.layouts(),
        runner: Rc::new(Process::new()),
    };
    match app::run(window, start) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-welcome: {error}");
            ExitCode::FAILURE
        }
    }
}
