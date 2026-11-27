//! sengi-welcome: the first-run welcome of Sengi OS, and the question of its
//! live image.

use std::process::ExitCode;
use std::rc::Rc;

use sengi_apps::env::Env;
use sengi_config::{Dirs, Settings};
use sengi_welcome::app::{self, Start};
use sengi_welcome::system::{self, Process};
use sengi_welcome::ui::{LiveWindow, ThemeSwitch, WelcomeWindow};
use sengi_welcome::{live, marker};

/// What the dock and the compositor know the window by. The `.desktop`
/// file has the same name.
const APP_ID: &str = "sengi-welcome";

const USAGE: &str = "usage: sengi-welcome [--again | --first-run | --live]\n\nOpens the Sengi OS welcome: a few steps to make a new installation yours.\n\n  --again      open it whether or not it has been seen before\n  --first-run  for the session: open it only on an installed system, and only\n               if it has not been seen; otherwise do nothing\n  --live       for the session: on the live image, ask once per start whether\n               to install Sengi OS or try it first; otherwise do nothing\n";

/// After the first component, which is what brings the backend up, and
/// before the window is shown.
fn set_app_id() {
    if let Err(error) = slint::set_xdg_app_id(APP_ID) {
        eprintln!("sengi-welcome: could not set the application id: {error}");
    }
}

fn no_window(error: slint::PlatformError) -> ExitCode {
    eprintln!("sengi-welcome: cannot open a window: {error}");
    ExitCode::FAILURE
}

fn ended(result: Result<(), slint::PlatformError>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-welcome: {error}");
            ExitCode::FAILURE
        }
    }
}

fn ask_live(env: &Env) -> ExitCode {
    let boot = std::fs::read_to_string(marker::BOOT_ID).unwrap_or_default();
    let asked = marker::was_asked(&env.state_home, &boot);
    if !marker::should_ask(marker::running_live(), asked) {
        return ExitCode::SUCCESS;
    }
    // The toolkit asks the desktop portal for a colour scheme before its
    // first window, which starts the portal's services, and they stay. The
    // question takes its colours from Settings, so it is given no bus to ask
    // on; the installer is handed the session's. Set before any thread is.
    let bus = std::env::var_os(system::BUS);
    std::env::set_var(system::BUS, "disabled:");
    let window = match LiveWindow::new() {
        Ok(window) => window,
        Err(error) => return no_window(error),
    };
    sengi_config::apply_theme!(slint::ComponentHandle::global::<ThemeSwitch>(&window));
    set_app_id();
    let runner = Rc::new(Process::handing_on(bus));
    ended(live::run(window, &env.state_home, &boot, runner))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = Env::from_process();
    if args.len() > 1 {
        eprintln!("sengi-welcome: unexpected argument {}\n\n{USAGE}", args[1]);
        return ExitCode::from(2);
    }
    match args.first().map(String::as_str) {
        None | Some("--again") => {}
        Some("--first-run") => {
            let live = marker::running_live();
            if !marker::should_show(live, marker::is_done(&env.state_home)) {
                return ExitCode::SUCCESS;
            }
        }
        Some("--live") => return ask_live(&env),
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

    let window = match WelcomeWindow::new() {
        Ok(window) => window,
        Err(error) => return no_window(error),
    };
    sengi_config::apply_theme!(slint::ComponentHandle::global::<ThemeSwitch>(&window));
    set_app_id();
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
    ended(app::run(window, start))
}
