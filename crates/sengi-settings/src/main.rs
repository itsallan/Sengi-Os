//! sengi-settings, the Sengi OS settings application.
//!
//! Started from the launcher or by another part of the desktop that wants
//! to show one of its pages, and gone when its window is closed. This file
//! makes sure there is only one window, answers the two questions scripts
//! ask without opening one, and runs the event loop.

use std::io::Read;
use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use sengi_settings::about::AboutPage;
use sengi_settings::app::{self, App};
use sengi_settings::bluetooth::page::BluetoothPage;
use sengi_settings::cli::{self, Action, Page};
use sengi_settings::datetime::DateTimePage;
use sengi_settings::display::page::DisplayPage;
use sengi_settings::instance::{self, Claim};
use sengi_settings::keyboard::KeyboardPage;
use sengi_settings::network::page::NetworkPage;
use sengi_settings::notifications::NotificationsPage;
use sengi_settings::pointer::PointerPage;
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

/// The signals that mean "end now", which Settings ends tidily on.
const ENDING: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

/// How long the window has to close after such a signal before the
/// process leaves without it.
const CLOSING: Duration = Duration::from_secs(3);

/// The end of a pipe the signal handler writes to. A thread waits at the
/// other end.
static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

/// Runs in whatever thread the signal lands on, between any two of its
/// instructions: it does the one thing that is safe there, a write.
extern "C" fn on_signal(signal: libc::c_int) {
    // SAFETY: errno is this thread's own, and write is async-signal-safe.
    // The pipe does not block; if it is full, a signal is already on its
    // way to being handled.
    unsafe {
        let errno = *libc::__errno_location();
        let byte = signal as u8;
        libc::write(
            SIGNAL_PIPE.load(Ordering::Relaxed),
            (&raw const byte).cast(),
            1,
        );
        *libc::__errno_location() = errno;
    }
}

/// End tidily on SIGTERM, SIGINT and SIGHUP: a display change still on
/// trial is undone and the last setting is applied before the process
/// goes. Without this, `pkill sengi-settings` in the fifteen seconds of a
/// trial would leave a mode nobody agreed to.
fn end_on_signals() {
    let mut ends = [0 as libc::c_int; 2];
    // SAFETY: pipe2 fills in the two descriptors it is given room for.
    if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        eprintln!("sengi-settings: no pipe for signals; they will end it at once");
        return;
    }
    // SAFETY: the descriptors are the pipe's, just made and owned here.
    // Only the writing end is made non-blocking; the reader sleeps.
    let mut reader = unsafe {
        let flags = libc::fcntl(ends[1], libc::F_GETFL);
        libc::fcntl(ends[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
        std::fs::File::from_raw_fd(ends[0])
    };
    let spawned = std::thread::Builder::new()
        .name("settings-signals".to_string())
        .spawn(move || {
            let mut signal = [0u8; 1];
            loop {
                match reader.read(&mut signal) {
                    Ok(1) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    _ => return,
                }
            }
            // Here, not on the UI thread: a window that has hung is one
            // reason to be sent a signal.
            app::terminate();
            let _ = slint::invoke_from_event_loop(|| {
                let _ = slint::quit_event_loop();
            });
            // The UI thread tells the pages and returns from main. If it
            // does not, what mattered is already done.
            std::thread::sleep(CLOSING);
            // SAFETY: _exit only ends the process.
            unsafe { libc::_exit(128 + libc::c_int::from(signal[0])) };
        });
    if spawned.is_err() {
        eprintln!("sengi-settings: no thread for signals; they will end it at once");
        return;
    }
    SIGNAL_PIPE.store(ends[1], Ordering::Relaxed);
    for signal in ENDING {
        // SAFETY: the action is zeroed and then given a handler that
        // only writes to a pipe, an empty mask, and SA_RESTART so that
        // reads elsewhere are not cut short by it.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

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
    sengi_system::set_program_name("sengi-settings");
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
    end_on_signals();
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
            // Told to end: nobody is owed a window now.
            Some(_) if app::terminating() => break,
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
    app.register(Page::Mouse, PointerPage::attach(app));
    app.register(Page::Notifications, NotificationsPage::attach(app));
    app.register(Page::About, AboutPage::attach(app));
}
