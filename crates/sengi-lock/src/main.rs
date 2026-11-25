//! sengi-lock, the Sengi OS lock screen.
//!
//! Locks the session through ext-session-lock-v1 and unlocks it when PAM
//! says the password typed is that of the user the session belongs to.
//! The pieces are in the library; this file puts them on the screen. See
//! the README for the design and for what is and is not protected.

use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use sengi_clock::Clock;
use sengi_config::{Dirs, Settings};
use sengi_layer::{App, Handle, Instances, LockError, Sender};
use sengi_lock::cli::{self, Action};
use sengi_lock::forward;
use sengi_lock::pam::{self, Outcome};
use sengi_lock::screen::{Next, Screen, Verdict, View};
use sengi_lock::supervise::{self, Ready};
use sengi_lock::user::{self, User};
use sengi_system::secret::Secret;
use slint::{Timer, TimerMode};

slint::include_modules!();

/// The PAM service the password is checked against.
const SERVICE: &str = "sengi-lock";

/// Where PAM looks for a service's configuration.
const PAM_DIRS: [&str; 2] = ["/etc/pam.d", "/usr/lib/pam.d"];

/// Set to have each step of locking timed on standard error.
const TRACE_VARIABLE: &str = "SENGI_LOCK_TRACE";

static STARTED: OnceLock<Instant> = OnceLock::new();
static USER: OnceLock<User> = OnceLock::new();

fn trace(what: &str) {
    if std::env::var_os(TRACE_VARIABLE).is_some() {
        let elapsed = STARTED
            .get()
            .map_or(0.0, |started| started.elapsed().as_secs_f64());
        eprintln!("sengi-lock: {:7.1} ms  {what}", elapsed * 1000.0);
    }
}

fn main() -> ExitCode {
    let _ = STARTED.set(Instant::now());
    // First, before a password can be anywhere in memory. The processes
    // forked below inherit it.
    sengi_system::set_program_name("sengi-lock");
    sengi_system::secret::forbid_core_dumps();
    let detach = match cli::parse(std::env::args().skip(1)) {
        Ok(Action::Lock { detach }) => detach,
        Ok(Action::Help) => {
            print!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Action::Version) => {
            println!("sengi-lock {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("sengi-lock: {message}\n\n{}", cli::USAGE);
            return ExitCode::from(64);
        }
    };

    // sengi-compositor shows the session lock only to a locker it started
    // itself, on a connection it hands over. Started there by anything
    // else, such as the idle timer or the session menu, this passes the
    // request on to the compositor, which then starts the locker, and
    // returns once the screen is locked, with or without `-f`. Where no
    // compositor answers, this locks the session itself as anywhere else.
    if std::env::var_os("WAYLAND_SOCKET").is_none() {
        let asked = forward::control_socket(
            std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
            std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        )
        .and_then(|socket| forward::ask(&socket));
        match asked {
            Some(true) => return ExitCode::SUCCESS,
            Some(false) => {
                eprintln!("sengi-lock: sengi-compositor could not lock the screen");
                return ExitCode::from(supervise::FAILED);
            }
            None => {}
        }
    }

    // A screen that nothing could unlock is worse than one that will not
    // lock, so both of these are settled before the compositor is asked.
    let Some(user) = user::current() else {
        eprintln!("sengi-lock: this user has no account entry; not locking");
        return ExitCode::from(supervise::FAILED);
    };
    if !PAM_DIRS
        .iter()
        .any(|dir| Path::new(dir).join(SERVICE).is_file())
    {
        eprintln!("sengi-lock: there is no PAM service called {SERVICE}; not locking");
        return ExitCode::from(supervise::FAILED);
    }
    let _ = USER.set(user);

    ExitCode::from(supervise::run(detach, work))
}

/// The worker process: lock, ask, unlock. Its exit code tells the
/// supervisor whether to start another.
fn work(ready: Ready) -> u8 {
    let app = match App::new() {
        Ok(app) => app,
        Err(error) => {
            eprintln!("sengi-lock: {error}");
            return supervise::NO_COMPOSITOR;
        }
    };
    trace("connected to the compositor");
    match lock(app, ready) {
        Ok(()) => supervise::UNLOCKED,
        Err(sengi_layer::Error::Lock(LockError::Refused)) => {
            // Usually a second sengi-lock while the first is still up.
            // That is no error worth a line in the journal.
            supervise::REFUSED
        }
        Err(sengi_layer::Error::Lock(LockError::Unsupported)) => {
            eprintln!("sengi-lock: {}", LockError::Unsupported);
            supervise::NO_COMPOSITOR
        }
        Err(error) => {
            eprintln!("sengi-lock: {error}");
            supervise::FAILED
        }
    }
}

/// The lock screen while it is up: its state, its windows, its timers.
struct Lock {
    handle: Handle,
    user: &'static User,
    screen: RefCell<Screen>,
    clock: RefCell<Clock>,
    windows: Instances<LockWindow>,
    /// Puts the field away when nothing has happened for a while.
    rest: Timer,
    rest_after: Cell<Duration>,
    /// Opens the prompt again after a wrong password's wait.
    hold: Timer,
    verdicts: RefCell<Option<Sender<Verdict>>>,
}

fn apply(window: &LockWindow, view: &View) {
    window.set_length(view.length as i32);
    window.set_prompt(view.prompt);
    window.set_enabled(view.enabled);
    window.set_busy(view.busy);
    window.set_rejected(view.rejected);
    window.set_placeholder(view.placeholder.as_str().into());
    window.set_note(view.note.as_str().into());
    window.set_note_is_problem(view.note_is_problem);
    window.set_note_is_warning(view.note_is_warning);
    window.set_layout(view.layout.as_str().into());
}

impl Lock {
    /// Changes the state and shows the result on every output.
    fn act(self: &Rc<Self>, change: impl FnOnce(&mut Screen)) {
        change(&mut self.screen.borrow_mut());
        self.show();
    }

    fn show(self: &Rc<Self>) {
        let (view, may_rest) = {
            let screen = self.screen.borrow();
            (screen.view(), screen.may_rest())
        };
        self.windows.for_each(|window| apply(window, &view));
        if may_rest {
            let lock = Rc::downgrade(self);
            // Starting a running timer starts its time over.
            self.rest
                .start(TimerMode::SingleShot, self.rest_after.get(), move || {
                    if let Some(lock) = lock.upgrade() {
                        if lock.screen.borrow_mut().rested() {
                            lock.show();
                        }
                    }
                });
        } else {
            self.rest.stop();
        }
    }

    fn tell_time(&self) {
        let mut clock = self.clock.borrow_mut();
        clock.follow_settings();
        let now = chrono::Utc::now();
        let ((time, period), date) = (clock.time_parts(now), clock.date(now));
        self.windows.for_each(|window| {
            window.set_time(time.as_str().into());
            window.set_period(period.as_str().into());
            window.set_date(date.as_str().into());
        });
    }

    /// Return was pressed: hand what was typed to PAM, on a thread of its
    /// own so that a slow module holds up nothing here.
    fn submit(self: &Rc<Self>) {
        let password = self.screen.borrow_mut().submitted();
        if let (Some(password), Some(verdicts)) = (password, self.verdicts.borrow().clone()) {
            let login = self.user.login.clone();
            let checker = std::thread::Builder::new()
                .name("pam".into())
                .spawn(move || {
                    let verdict = check(&login, password);
                    let _ = verdicts.send(verdict);
                });
            if checker.is_err() {
                // The password went with the closure and was wiped.
                self.judged(Verdict::Unchecked(None));
                return;
            }
        }
        self.show();
    }

    fn judged(self: &Rc<Self>, verdict: Verdict) {
        let next = self.screen.borrow_mut().judged(verdict);
        match next {
            // The one place the session is unlocked.
            Next::Unlock => self.handle.unlock(),
            Next::Hold(wait) => {
                let lock = Rc::downgrade(self);
                self.hold.start(TimerMode::SingleShot, wait, move || {
                    if let Some(lock) = lock.upgrade() {
                        lock.act(Screen::released);
                    }
                });
            }
            Next::Ask => {}
        }
        self.show();
    }
}

/// Runs on the PAM thread. The password is wiped when this returns,
/// whatever PAM did.
fn check(login: &str, password: Secret) -> Verdict {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        pam::authenticate(SERVICE, login, &password)
    }));
    drop(password);
    match outcome {
        Ok(Outcome::Granted) => Verdict::Right,
        Ok(Outcome::Denied(said)) => Verdict::Wrong(said),
        Ok(Outcome::Barred) => Verdict::Barred,
        Ok(Outcome::Unavailable(said)) => Verdict::Unchecked(said),
        Err(_) => Verdict::Unchecked(None),
    }
}

fn lock(app: App, ready: Ready) -> Result<(), sengi_layer::Error> {
    let handle = app.handle();
    let user = USER.get().expect("the user is looked up before the fork");
    let (settings, _) = Settings::load(&Dirs::from_env());
    let layouts: Vec<String> = settings
        .layouts()
        .into_iter()
        .map(|layout| layout.layout)
        .collect();

    let lock = Rc::new(Lock {
        handle: handle.clone(),
        user,
        screen: RefCell::new(Screen::new(&layouts)),
        clock: RefCell::new(Clock::new()),
        windows: Instances::new(),
        rest: Timer::default(),
        rest_after: Cell::new(Duration::from_secs(30)),
        hold: Timer::default(),
        verdicts: RefCell::new(None),
    });

    *lock.verdicts.borrow_mut() = Some(handle.channel({
        let lock = lock.clone();
        move |verdict| lock.judged(verdict)
    })?);

    // Every key that types text ends here, wherever the screen is in its
    // states, so that none of a password ever reaches the toolkit.
    handle.on_text({
        let lock = lock.clone();
        move |text| {
            lock.act(|screen| screen.typed(text));
            true
        }
    });
    handle.on_keyboard_changed({
        let lock = lock.clone();
        move |keyboard| lock.act(|screen| screen.keyboard(keyboard.caps_lock, keyboard.layout))
    });

    // The clock is drawn again when the minute turns and when the wall
    // clock is set, which waking from suspend does. Nothing else wakes
    // this process while the screen is left alone.
    let minutes = handle.channel({
        let lock = lock.clone();
        move |()| lock.tell_time()
    })?;
    if let Err(error) = sengi_clock::every_minute(move || minutes.send(()).is_ok()) {
        eprintln!("sengi-lock: the clock will not follow the time: {error}");
    }

    let build = {
        let lock = lock.clone();
        move |_: &sengi_layer::Context| {
            let window = LockWindow::new()?;
            slint::ComponentHandle::global::<ThemeSwitch>(&window)
                .set_light(sengi_config::theme_is_light());
            window.set_user(lock.user.shown.as_str().into());
            lock.rest_after.set(Duration::from_millis(
                window.get_hide_after().max(1000) as u64
            ));
            window.on_accepted({
                let lock = lock.clone();
                move || lock.submit()
            });
            window.on_cleared({
                let lock = lock.clone();
                move || lock.act(Screen::cleared)
            });
            window.on_erased({
                let lock = lock.clone();
                move || lock.act(Screen::erased)
            });
            // Not reached under sengi-layer, which hands typed text to
            // `on_text` above. Here for the keys that type nothing.
            window.on_typed({
                let lock = lock.clone();
                move |text| lock.act(|screen| screen.typed(&text))
            });
            window.on_stirred({
                let lock = lock.clone();
                move || lock.act(Screen::stirred)
            });
            lock.windows.add(&window);
            lock.tell_time();
            lock.show();
            trace("a window built");
            Ok(window)
        }
    };
    app.run_locked(build, move || {
        trace("locked, every output covered");
        ready.locked();
    })
}
