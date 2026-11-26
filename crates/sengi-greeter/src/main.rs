//! sengi-greeter, the Sengi OS login screen.
//!
//! A greetd client: it shows who can log in, takes the password, and has
//! greetd check it and start the session. The pieces are in the library
//! and in sengi-lock; this file puts them on the screen. See the README
//! for the design.

use std::cell::{Cell, RefCell};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use sengi_clock::Clock;
use sengi_config::{Dirs, Settings};
use sengi_greeter::cli::{self, Action};
use sengi_greeter::login::{self, Command, Event, Patience};
use sengi_greeter::power;
use sengi_greeter::screen::{Login, Power, Press, View};
use sengi_greeter::users;
use sengi_layer::{Anchor, App, Config, Handle, Instances, KeyboardMode, Layer, Outputs, Sender};
use sengi_lock::screen::Next;
use slint::{Timer, TimerMode};

slint::include_modules!();

/// Where greetd says its socket is.
const SOCKET_VARIABLE: &str = "GREETD_SOCK";

fn main() -> ExitCode {
    // First, before a password can be anywhere in memory.
    sengi_system::set_program_name("sengi-greeter");
    sengi_system::secret::forbid_core_dumps();
    let session = match cli::parse(std::env::args().skip(1)) {
        Ok(Action::Greet { session }) => session,
        Ok(Action::Help) => {
            print!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Action::Version) => {
            println!("sengi-greeter {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("sengi-greeter: {message}\n\n{}", cli::USAGE);
            return ExitCode::from(64);
        }
    };

    // Before any thread is started: see `users`.
    let users = users::logins();

    let Some(socket) = std::env::var_os(SOCKET_VARIABLE) else {
        eprintln!("sengi-greeter: {SOCKET_VARIABLE} is not set; greetd starts this program");
        return ExitCode::FAILURE;
    };
    // A login screen that cannot log anyone in should not come up at all.
    let first = match UnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("sengi-greeter: could not reach greetd: {error}");
            return ExitCode::FAILURE;
        }
    };
    let app = match App::new() {
        Ok(app) => app,
        Err(error) => {
            eprintln!("sengi-greeter: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut first = Some(first);
    let connect = move || match first.take() {
        Some(stream) => Ok(stream),
        None => UnixStream::connect(&socket),
    };
    match greet(app, users, connect, session) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("sengi-greeter: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The login screen while it is up: its state, its windows, its timers.
struct Greeter {
    handle: Handle,
    login: RefCell<Login>,
    clock: RefCell<Clock>,
    windows: Instances<GreeterWindow>,
    /// Puts the field away when nothing has happened for a while.
    rest: Timer,
    rest_after: Cell<Duration>,
    /// Opens the prompt again after a wrong password's wait.
    hold: Timer,
    /// To the thread that speaks to greetd.
    commands: mpsc::Sender<Command>,
    /// Lets go of Restart or Shut down when the second press has not come.
    confirm: Timer,
    confirm_within: Cell<Duration>,
    /// Called from the thread that ran a restart or shut down that failed.
    stayed: RefCell<Option<Sender<()>>>,
    /// greetd has granted a session.
    granted: Cell<bool>,
}

fn apply(window: &GreeterWindow, view: &View) {
    let field = &view.field;
    window.set_user(view.user.as_str().into());
    window.set_caption(view.caption.as_str().into());
    window.set_naming(view.naming);
    window.set_choice(view.choice);
    window.set_power(view.power);
    window.set_restart_armed(view.armed == Some(Power::Restart));
    window.set_shut_down_armed(view.armed == Some(Power::ShutDown));
    window.set_plain(view.plain.as_str().into());
    window.set_length(field.length as i32);
    window.set_prompt(field.prompt);
    window.set_enabled(field.enabled);
    window.set_busy(field.busy);
    window.set_rejected(field.rejected);
    window.set_placeholder(field.placeholder.as_str().into());
    window.set_note(field.note.as_str().into());
    window.set_note_is_problem(field.note_is_problem);
    window.set_note_is_warning(field.note_is_warning);
    window.set_layout(field.layout.as_str().into());
}

impl Greeter {
    /// Changes the state, passes on what the change has for greetd, and
    /// shows the result on every output.
    fn act(self: &Rc<Self>, change: impl FnOnce(&mut Login) -> Option<Command>) {
        let command = change(&mut self.login.borrow_mut());
        self.send(command);
        self.show();
    }

    fn send(&self, command: Option<Command>) {
        if let Some(command) = command {
            // If the thread is gone it has said so, and the loop is ending.
            let _ = self.commands.send(command);
        }
    }

    fn show(self: &Rc<Self>) {
        let (view, may_rest) = {
            let login = self.login.borrow();
            (login.view(), login.may_rest())
        };
        self.windows.for_each(|window| apply(window, &view));
        if may_rest {
            let greeter = Rc::downgrade(self);
            // Starting a running timer starts its time over.
            self.rest
                .start(TimerMode::SingleShot, self.rest_after.get(), move || {
                    if let Some(greeter) = greeter.upgrade() {
                        let (changed, command) = greeter.login.borrow_mut().rested();
                        greeter.send(command);
                        if changed {
                            // A key that wakes the screen is for the
                            // field, not for a button Tab was left on.
                            greeter.focus_field();
                            greeter.show();
                        }
                    }
                });
        } else {
            self.rest.stop();
        }
    }

    /// Puts the keyboard focus back on the field, on every output. Typing
    /// reaches the field wherever the focus is; Return does not, and a
    /// Return that lands on Restart is one press towards restarting.
    fn focus_field(&self) {
        self.windows.for_each(|window| window.invoke_focus_field());
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

    /// The thread that speaks to greetd has something to say.
    fn heard(self: &Rc<Self>, event: Event) {
        match event {
            Event::Asked {
                question,
                visible,
                said,
            } => self.login.borrow_mut().asked(&question, visible, said),
            Event::Judged(verdict) => {
                let next = self.login.borrow_mut().judged(verdict);
                // Whatever the verdict, the next key is for the field.
                self.focus_field();
                match next {
                    // The one place a login ends well. greetd starts the
                    // session when this process, and with it the
                    // compositor it runs in, has exited.
                    Next::Unlock => {
                        self.granted.set(true);
                        self.handle.quit();
                    }
                    Next::Hold(wait) => {
                        let greeter = Rc::downgrade(self);
                        self.hold.start(TimerMode::SingleShot, wait, move || {
                            if let Some(greeter) = greeter.upgrade() {
                                greeter.act(|login| {
                                    login.released();
                                    None
                                });
                            }
                        });
                    }
                    Next::Ask => {}
                }
            }
            Event::Gone => {
                eprintln!("sengi-greeter: greetd has gone away");
                self.handle.quit();
            }
        }
        self.show();
    }

    /// Restart or Shut down was pressed: once arms it, twice does it.
    fn press(self: &Rc<Self>, power: Power) {
        let press = self.login.borrow_mut().pressed(power);
        match press {
            Press::Armed => {
                let greeter = Rc::downgrade(self);
                self.confirm.start(
                    TimerMode::SingleShot,
                    self.confirm_within.get(),
                    move || {
                        if let Some(greeter) = greeter.upgrade() {
                            if greeter.login.borrow_mut().disarmed() {
                                greeter.show();
                            }
                        }
                    },
                );
            }
            Press::Go => {
                self.confirm.stop();
                let stayed = self.stayed.borrow().clone();
                let started = stayed.and_then(|stayed| {
                    power::carry_out(power::command(power), move || {
                        let _ = stayed.send(());
                    })
                    .ok()
                });
                if started.is_none() {
                    self.stayed();
                }
            }
            Press::Nothing => {}
        }
        self.show();
    }

    /// The machine did not restart or shut down after all.
    fn stayed(self: &Rc<Self>) {
        self.login.borrow_mut().stayed();
        // In this order. The button that was pressed was switched off
        // while the machine was thought to be going down, and a button
        // that is switched off does not hear that it has lost the focus:
        // it would go on drawing the ring. So it is switched on again
        // first.
        self.show();
        self.focus_field();
    }
}

/// Runs the login screen. True if greetd granted a session.
fn greet(
    app: App,
    users: Vec<sengi_lock::user::User>,
    connect: impl FnMut() -> std::io::Result<UnixStream> + Send + 'static,
    session: String,
) -> Result<bool, Box<dyn std::error::Error>> {
    let handle = app.handle();
    let (settings, _) = Settings::load(&Dirs::from_env());
    let layouts: Vec<String> = settings
        .layouts()
        .into_iter()
        .map(|layout| layout.layout)
        .collect();

    let (commands, received) = mpsc::channel();
    let greeter = Rc::new(Greeter {
        handle: handle.clone(),
        login: RefCell::new(Login::new(users, &layouts)),
        clock: RefCell::new(Clock::new()),
        windows: Instances::new(),
        rest: Timer::default(),
        rest_after: Cell::new(Duration::from_secs(30)),
        hold: Timer::default(),
        confirm: Timer::default(),
        confirm_within: Cell::new(Duration::from_secs(5)),
        commands,
        stayed: RefCell::new(None),
        granted: Cell::new(false),
    });

    let events = handle.channel({
        let greeter = greeter.clone();
        move |event| greeter.heard(event)
    })?;
    std::thread::Builder::new()
        .name("greetd".into())
        .spawn(move || {
            login::serve(
                connect,
                received,
                |event| events.send(event).is_ok(),
                &[session],
                Patience::default(),
            )
        })?;

    *greeter.stayed.borrow_mut() = Some(handle.channel({
        let greeter = greeter.clone();
        move |()| greeter.stayed()
    })?);

    // Every key that types text ends here, so that none of a password
    // ever reaches the toolkit. Typing is always for the field, even
    // after Tab has put the focus on a button.
    handle.on_text({
        let greeter = greeter.clone();
        move |text| {
            greeter.act(|login| {
                login.typed(text);
                None
            });
            greeter.focus_field();
            true
        }
    });
    handle.on_keyboard_changed({
        let greeter = greeter.clone();
        move |keyboard| {
            greeter.act(|login| {
                login.keyboard(keyboard.caps_lock, keyboard.layout);
                None
            })
        }
    });

    // The clock is drawn again when the minute turns and when the wall
    // clock is set. Nothing else wakes this process while it is left
    // alone.
    let minutes = handle.channel({
        let greeter = greeter.clone();
        move |()| greeter.tell_time()
    })?;
    if let Err(error) = sengi_clock::every_minute(move || minutes.send(()).is_ok()) {
        eprintln!("sengi-greeter: the clock will not follow the time: {error}");
    }

    let config = Config::new("sengi-greeter")
        .layer(Layer::Overlay)
        .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
        .size(0, 0)
        .exclusive_zone(-1)
        .keyboard(KeyboardMode::Exclusive)
        .outputs(Outputs::All);

    let build = {
        let greeter = greeter.clone();
        move |_: &sengi_layer::Context| {
            let window = GreeterWindow::new()?;
            sengi_config::apply_theme!(slint::ComponentHandle::global::<ThemeSwitch>(&window));
            greeter.rest_after.set(Duration::from_millis(
                window.get_hide_after().max(1000) as u64
            ));
            window.on_accepted({
                let greeter = greeter.clone();
                move || greeter.act(Login::submitted)
            });
            window.on_cleared({
                let greeter = greeter.clone();
                move || greeter.act(Login::cleared)
            });
            window.on_erased({
                let greeter = greeter.clone();
                move || {
                    greeter.act(|login| {
                        login.erased();
                        None
                    })
                }
            });
            // Not reached for text under sengi-layer, which hands that to
            // `on_text` above. Here for the keys that type nothing.
            window.on_typed({
                let greeter = greeter.clone();
                move |text| {
                    greeter.act(|login| {
                        login.typed(&text);
                        None
                    })
                }
            });
            window.on_stepped({
                let greeter = greeter.clone();
                move |step| greeter.act(|login| login.stepped(step as isize))
            });
            window.on_restart({
                let greeter = greeter.clone();
                move || greeter.press(Power::Restart)
            });
            window.on_shut_down({
                let greeter = greeter.clone();
                move || greeter.press(Power::ShutDown)
            });
            greeter.confirm_within.set(Duration::from_millis(
                window.get_confirm_within().max(1000) as u64,
            ));
            window.on_turned_away({
                let greeter = greeter.clone();
                move || {
                    if greeter.login.borrow_mut().disarmed() {
                        greeter.show();
                    }
                }
            });
            window.on_stirred({
                let greeter = greeter.clone();
                move || {
                    greeter.act(|login| {
                        login.stirred();
                        None
                    })
                }
            });
            greeter.windows.add(&window);
            greeter.tell_time();
            greeter.show();
            Ok(window)
        }
    };
    app.run(config, build)?;
    Ok(greeter.granted.get())
}
