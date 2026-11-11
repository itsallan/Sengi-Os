//! sengi-settings, the Sengi OS settings application.
//!
//! Started from the launcher or by another part of the desktop that wants
//! to show one of its pages, and gone when its window is closed. This file
//! makes sure there is only one window, answers the two questions scripts
//! ask without opening one, and runs the event loop.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use slint::ComponentHandle;

use sengi_settings::app::{self, App};
use sengi_settings::bluetooth::page::BluetoothPage;
use sengi_settings::cli::{self, Action, Page};
use sengi_settings::datetime::DateTimePage;
use sengi_settings::display::page::DisplayPage;
use sengi_settings::instance::{self, Claim};
use sengi_settings::keyboard::KeyboardPage;
use sengi_settings::network::page::NetworkPage;
use sengi_settings::power::PowerPage;
use sengi_settings::services::{self, Services};
use sengi_settings::settings::Settings;
use sengi_settings::sound::page::SoundPage;
use sengi_settings::store::Dirs;
use sengi_settings::ui::SettingsWindow;
use sengi_settings::{derive, secret};

/// Set to have the program say on standard error how long starting took.
const TRACE_VARIABLE: &str = "SENGI_SETTINGS_TRACE";

/// What the dock and the compositor know the window by. The `.desktop`
/// file has the same name.
const APP_ID: &str = "sengi-settings";

fn load_settings(dirs: &Dirs) -> Settings {
    let (settings, warnings) = Settings::load(dirs);
    for warning in warnings {
        eprintln!("sengi-settings: {warning}");
    }
    settings
}

fn main() -> ExitCode {
    let started = Instant::now();
    // First, before a passphrase can be anywhere in memory.
    secret::forbid_core_dumps();
    let action = match cli::parse(std::env::args().skip(1)) {
        Ok(action) => action,
        Err(message) => {
            eprintln!("sengi-settings: {message}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let dirs = Dirs::from_env();
    #[allow(unused_mut)]
    let mut services = services::real();
    let page = match action {
        Action::Help => {
            print!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Action::Version => {
            println!("sengi-settings {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Action::Get { section, key } => {
            return match load_settings(&dirs).get(&section, &key) {
                Some(value) => {
                    println!("{}", value.plain());
                    ExitCode::SUCCESS
                }
                None => {
                    eprintln!("sengi-settings: no setting called {section}.{key}");
                    ExitCode::from(2)
                }
            };
        }
        Action::Screen(on) => {
            return match sengi_settings::screen::switch(on) {
                Ok(()) => ExitCode::SUCCESS,
                Err(problem) => {
                    eprintln!("sengi-settings: {problem}");
                    ExitCode::FAILURE
                }
            };
        }
        Action::Apply => {
            let applied = derive::apply(&dirs, &load_settings(&dirs));
            let problems = applied.problems();
            for problem in &problems {
                eprintln!("sengi-settings: {problem}");
            }
            return if problems.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Action::Open(page) => page,
        #[cfg(debug_assertions)]
        Action::Demo(name) => match services::demo(&name) {
            Some((page, demo)) => {
                services = demo;
                Some(page)
            }
            None => {
                eprintln!(
                    "sengi-settings: no demo called {name}. There are: {}",
                    services::DEMOS.join(", ")
                );
                return ExitCode::from(2);
            }
        },
    };

    // Before anything slow, so a second invocation finds this one.
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    let instance = match instance::claim(runtime_dir.as_deref(), page) {
        Claim::Granted(instance) => instance,
        Claim::Forwarded => return ExitCode::SUCCESS,
    };

    let window = match SettingsWindow::new() {
        Ok(window) => window,
        Err(error) => {
            eprintln!("sengi-settings: cannot open a window: {error}");
            return ExitCode::FAILURE;
        }
    };
    // After the first component, which is what brings the backend up, and
    // before the window is shown.
    if let Err(error) = slint::set_xdg_app_id(APP_ID) {
        eprintln!("sengi-settings: could not set the application id: {error}");
    }
    let app = App::new(window, dirs.clone(), load_settings(&dirs));
    register_pages(&app, services);
    if let Some(page) = page {
        app.show(page);
    }
    // No page named, or one this build does not have.
    if let (None, Some(first)) = (app.current(), app.first_page()) {
        app.show(first);
    }
    app.window.invoke_focus_sidebar();

    // A later invocation's page. The request waits in the instance; this
    // only says to go and look.
    instance.on_request(|| {
        app::post(|app| app.window.invoke_request_pending());
    });
    let instance = std::rc::Rc::new(instance);
    app.window.on_request_pending({
        let (instance, app) = (instance.clone(), std::rc::Rc::downgrade(&app));
        move || {
            let (Some(app), Some(request)) = (app.upgrade(), instance.take()) else {
                return;
            };
            if let Some(page) = request.0 {
                app.show(page);
            }
        }
    });

    if std::env::var_os(TRACE_VARIABLE).is_some() {
        // The software renderer has no hook for "a frame was drawn". A
        // timer that is due at once fires after the first turn of the
        // event loop, which is when the first frame has been handed over.
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            eprintln!(
                "sengi-settings: window up after {} ms",
                started.elapsed().as_millis()
            );
        });
    }

    loop {
        if let Err(error) = app.window.run() {
            eprintln!("sengi-settings: {error}");
            app.shutdown();
            return ExitCode::FAILURE;
        }
        // The window has been closed. A request that was acknowledged
        // while it was closing is still owed a window.
        match instance.retire() {
            Some(request) => {
                if let Some(page) = request.0 {
                    app.show(page);
                }
            }
            None => break,
        }
    }
    app.shutdown();
    ExitCode::SUCCESS
}

/// The pages this build has.
fn register_pages(app: &std::rc::Rc<App>, services: Services) {
    app.register(Page::Network, NetworkPage::attach(app, services.network));
    app.register(
        Page::Bluetooth,
        BluetoothPage::attach(app, services.bluetooth),
    );
    app.register(Page::Sound, SoundPage::attach(app, services.sound));
    app.register(Page::Display, DisplayPage::attach(app, services.display));
    app.register(
        Page::Power,
        PowerPage::attach(app, services.bus.clone(), services.class.clone()),
    );
    app.register(
        Page::DateTime,
        DateTimePage::attach(app, services.bus.clone()),
    );
    app.register(Page::Keyboard, KeyboardPage::attach(app));
}
