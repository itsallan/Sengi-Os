//! sengi-installer: asks where and for whom to install Sengi OS, shows
//! what that will erase, and follows the installation.
//!
//! The window runs as the live session's user. The one thing that needs
//! root, writing the disk, is done by sengi-install, started through
//! sudo with the plan and the password on its standard input. The live
//! user may use sudo without a password, which is what makes that work
//! and is acceptable only there: whoever sits at a live session owns the
//! machine already. The installed system has no such rule and no
//! installer.

use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, VecModel};

use sengi_config::settings::Layout;
use sengi_config::{Dirs, Value};
use sengi_install::disks::{self, Disk};
use sengi_install::progress::Event;
use sengi_installer::model::{self, Form, Outcome, Progress, StepState, Typed};
use sengi_installer::ui::{
    ChooserState, DialogSpec, DialogState, InfoRow, InstallerWindow, Item, SengiStepState, StepRow,
    Wizard,
};
use sengi_lock::entry::PasswordEntry;
use sengi_settings::app::{narrow, Choice, MAX_CHOICES};
use sengi_settings::keyboard::Database;
use sengi_settings::settings::Settings;
use sengi_settings::{datetime, derive};
use sengi_system::secret;

const USAGE: &str = "\
Usage: sengi-installer

Open the Sengi OS installer. It runs in the live session, from the USB
stick, and puts Sengi OS on a disk of this computer.

  -h, --help     show this help
  -V, --version  show the version
";

/// What the dock and the compositor know the window by. The `.desktop`
/// file has the same name.
const APP_ID: &str = "sengi-installer";

/// How the program that writes the disk is started: as root, without
/// being asked for a password (see the top of this file), and in a
/// scope of its own under the system's slice. Left where it starts, it
/// would be part of this window's app scope, which oomd may end when
/// memory is short; an installation is not something to end halfway.
const BACKEND: [&str; 9] = [
    "-n",
    "systemd-run",
    "--scope",
    "--quiet",
    "--collect",
    "--slice=system.slice",
    "--property=ManagedOOMPreference=omit",
    "/usr/bin/sengi-install",
    "run",
];

/// The most that is kept of what the installing program says when it
/// says it to standard error.
const KEPT_STDERR: usize = 4096;

/// What a chooser on the Region page is choosing.
enum Choosing {
    Layout,
    /// A variant of this layout.
    Variant(String),
    Zone,
}

struct Chooser {
    choosing: Choosing,
    choices: Vec<Choice>,
}

/// A stand-in for the machine and the installing program, for looking
/// at the window without a disk to erase. Debug builds only.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(debug_assertions), allow(dead_code))]
enum Demo {
    Off,
    Works,
    Fails,
}

struct App {
    window: InstallerWindow,
    demo: Demo,
    page: Cell<&'static str>,
    disks: RefCell<Vec<Disk>>,
    chosen: RefCell<Option<Disk>>,
    form: RefCell<Form>,
    password: RefCell<PasswordEntry>,
    again: RefCell<PasswordEntry>,
    layout: RefCell<Layout>,
    zone: RefCell<String>,
    database: RefCell<Option<Database>>,
    chooser: RefCell<Option<Chooser>>,
    progress: RefCell<Progress>,
    installing: Cell<bool>,
    /// The names the system has taken.
    passwd: String,
    group: String,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Run `task` on the UI thread with the application, from any thread.
fn post(task: impl FnOnce(&Rc<App>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        let app = APP.with(|app| app.borrow().clone());
        if let Some(app) = app {
            task(&app);
        }
    });
}

fn rows<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

fn facts(pairs: Vec<(&str, String)>) -> ModelRc<InfoRow> {
    rows(
        pairs
            .into_iter()
            .map(|(title, value)| InfoRow {
                title: title.into(),
                value: value.into(),
            })
            .collect(),
    )
}

fn uefi() -> bool {
    Path::new("/sys/firmware/efi").is_dir()
}

/// Disks to show when there is no machine to ask.
fn demo_disks() -> Vec<Disk> {
    let part = |name: &str, size: u64, fstype: &str, label: &str| disks::Part {
        name: name.to_string(),
        size,
        fstype: fstype.to_string(),
        label: label.to_string(),
    };
    let disk = |name: &str, size: u64, model: &str| Disk {
        name: name.to_string(),
        path: format!("/dev/{name}"),
        size,
        model: model.to_string(),
        removable: false,
        read_only: false,
        parts: Vec::new(),
        fstype: String::new(),
        label: String::new(),
        mounted: Vec::new(),
        swap: false,
        held: false,
        medium: false,
    };
    let mut main = disk("sda", 256_060_514_304, "SanDisk SD8SN8U256G");
    main.parts = vec![
        part("sda1", 104_857_600, "vfat", "SYSTEM"),
        part("sda2", 200_000_000_000, "ntfs", "Windows"),
        part("sda3", 55_000_000_000, "ext4", ""),
    ];
    let mut stick = disk("sdb", 30_752_000_000, "Ultra Fit");
    stick.removable = true;
    stick.medium = true;
    stick.fstype = "iso9660".to_string();
    stick.label = "SENGIOS".to_string();
    let card = disk("mmcblk0", 3_900_000_000, "SD04G");
    vec![main, stick, card]
}

/// What stands in for sengi-install in a demo: it says what the real
/// one says, slowly enough to be looked at.
fn demo_script(fails: bool) -> String {
    let end = if fails {
        "echo 'begin boot-loader'; sleep 1; echo 'failed boot-loader grub-install ended with status 1: cannot find a device for /boot/efi'; exit 1"
    } else {
        "for s in boot-loader finish; do echo \"begin $s\"; sleep 0.7; echo \"end $s\"; done; echo 'done 135'"
    };
    format!(
        "read -r password; cat >/dev/null; echo 'log /var/log/sengi-install.log'; \
         for s in check partition format; do echo \"begin $s\"; sleep 0.7; echo \"end $s\"; done; \
         echo 'begin copy'; for p in 8 21 42; do echo \"percent copy $p\"; sleep 0.5; done; sleep ${{SENGI_DEMO_PAUSE:-1}}; \
         for p in 63 87 100; do echo \"percent copy $p\"; sleep 0.5; done; echo 'end copy'; \
         for s in configure account boot-image; do echo \"begin $s\"; sleep 0.7; echo \"end $s\"; done; {end}"
    )
}

impl App {
    fn wizard(&self) -> Wizard<'_> {
        self.window.global::<Wizard>()
    }

    /// Go to `page` and set the bar under it to match.
    fn show(&self, page: &'static str) {
        self.page.set(page);
        let wizard = self.wizard();
        self.window.invoke_release();
        wizard.set_place(model::place(page).into());
        wizard.set_back_label(
            if model::page_before(page).is_some() {
                "Back"
            } else {
                ""
            }
            .into(),
        );
        wizard.set_next_erases(page == "review");
        wizard.set_next_label(
            if page == "review" {
                "Erase disk and install\u{2026}"
            } else {
                "Continue"
            }
            .into(),
        );
        match page {
            "disk" => self.scan(),
            "you" => self.render_form(),
            "review" => self.render_review(),
            _ => {}
        }
        self.set_can_next();
        wizard.set_page(page.into());
        self.window.invoke_settle();
    }

    fn set_can_next(&self) {
        let can = match self.page.get() {
            "disk" => self.chosen.borrow().is_some(),
            "you" => self.form_ready(),
            "progress" => self.progress.borrow().outcome != Outcome::Running,
            _ => true,
        };
        self.wizard().set_can_next(can);
    }

    // Region.

    fn layout_name(&self) -> String {
        let layout = self.layout.borrow();
        match self.database.borrow().as_ref() {
            Some(database) => database.describe(&layout),
            None => layout.render(),
        }
    }

    fn render_region(&self) {
        let wizard = self.wizard();
        wizard.set_keyboard(self.layout_name().into());
        wizard.set_zone(self.zone.borrow().replace('_', " ").into());
    }

    fn open_chooser(
        &self,
        title: &str,
        searchable: bool,
        choosing: Choosing,
        choices: Vec<Choice>,
    ) {
        let state = self.window.global::<ChooserState>();
        state.set_title(title.into());
        state.set_searchable(searchable);
        *self.chooser.borrow_mut() = Some(Chooser { choosing, choices });
        self.fill_chooser("", true);
        state.set_generation(state.get_generation().wrapping_add(1));
        state.set_open(true);
    }

    /// Show the choices that match what was typed. A long list is shown
    /// a stretch at a time, as Settings shows it.
    fn fill_chooser(&self, typed: &str, opening: bool) {
        let state = self.window.global::<ChooserState>();
        let chooser = self.chooser.borrow();
        let Some(open) = chooser.as_ref() else { return };
        let matching = narrow(&open.choices, typed);
        let total = matching.len();
        let at = matching.iter().position(|choice| choice.current);
        let first = match at {
            Some(at) if opening && total > MAX_CHOICES => {
                at.saturating_sub(MAX_CHOICES / 2).min(total - MAX_CHOICES)
            }
            _ => 0,
        };
        let items: Vec<Item> = matching
            .iter()
            .skip(first)
            .take(MAX_CHOICES)
            .map(|choice| Item {
                id: choice.id.as_str().into(),
                title: choice.title.as_str().into(),
                detail: choice.detail.as_str().into(),
                action: "Choose".into(),
                checked: choice.current,
                enabled: true,
                ..Item::default()
            })
            .collect();
        state.set_empty(if total == 0 { "Nothing matches" } else { "" }.into());
        state.set_current(match at {
            Some(at) if at >= first && at < first + MAX_CHOICES => (at - first) as i32,
            _ => 0,
        });
        state.set_items(rows(items));
    }

    fn close_chooser(&self) {
        *self.chooser.borrow_mut() = None;
        self.window.global::<ChooserState>().set_open(false);
        self.window.invoke_focus_next();
    }

    fn choose_keyboard(&self) {
        let database = self.database.borrow();
        let Some(database) = database.as_ref() else {
            return;
        };
        let now = self.layout.borrow().layout.clone();
        let choices = database
            .layouts
            .iter()
            .map(|layout| Choice {
                id: layout.name.clone(),
                title: layout.description.clone(),
                detail: layout.name.clone(),
                current: layout.name == now,
            })
            .collect();
        self.open_chooser("Keyboard layout", true, Choosing::Layout, choices);
    }

    fn choose_zone(&self) {
        let now = self.zone.borrow().clone();
        let choices = datetime::zones()
            .into_iter()
            .map(|zone| Choice {
                current: zone.name == now,
                id: zone.name,
                title: zone.city,
                detail: zone.region,
            })
            .collect();
        self.open_chooser("Time zone", true, Choosing::Zone, choices);
    }

    fn chosen(self: &Rc<Self>, id: String) {
        let Some(chooser) = self.chooser.borrow_mut().take() else {
            return;
        };
        match chooser.choosing {
            Choosing::Zone => {
                *self.zone.borrow_mut() = id;
                self.close_chooser();
            }
            Choosing::Layout => {
                let variants: Vec<Choice> = self
                    .database
                    .borrow()
                    .as_ref()
                    .map(|database| {
                        database
                            .variants_of(&id)
                            .into_iter()
                            .map(|variant| Choice {
                                id: variant.name.clone(),
                                title: variant.description.clone(),
                                detail: variant.name.clone(),
                                current: false,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if variants.is_empty() {
                    self.set_layout(id, String::new());
                    self.close_chooser();
                } else {
                    let mut choices = vec![Choice {
                        id: String::new(),
                        title: "The usual one".to_string(),
                        detail: "No variant".to_string(),
                        current: true,
                    }];
                    choices.extend(variants);
                    self.open_chooser("Variant", true, Choosing::Variant(id), choices);
                }
            }
            Choosing::Variant(layout) => {
                self.set_layout(layout, id);
                self.close_chooser();
            }
        }
        self.render_region();
    }

    /// Take the layout for the installed system, and switch the running
    /// session to it now: the password is typed on a later page, and it
    /// has to be typed with the keyboard it will be typed with again.
    fn set_layout(&self, layout: String, variant: String) {
        let Some(layout) = Layout::parse(&Layout { layout, variant }.render()) else {
            return;
        };
        *self.layout.borrow_mut() = layout.clone();
        if self.demo != Demo::Off {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("installer-keyboard".to_string())
            .spawn(move || {
                let dirs = Dirs::from_env();
                let (mut settings, _) = Settings::load(&dirs);
                let value = Value::List(vec![layout.render()]);
                match settings.set("keyboard", "layouts", value) {
                    Ok(()) => {
                        for problem in derive::apply(&dirs, &settings).problems() {
                            eprintln!("sengi-installer: {problem}");
                        }
                        derive::reconfigure();
                    }
                    Err(problem) => eprintln!("sengi-installer: {problem}"),
                }
            });
        if let Err(error) = spawned {
            eprintln!("sengi-installer: could not switch the keyboard layout: {error}");
        }
    }

    // Disk.

    /// Look at the disks, again or for the first time. A disk that was
    /// chosen stays chosen only if it is still there and still the same.
    fn scan(&self) {
        let found = match self.demo {
            Demo::Off => disks::scan_system(false),
            _ => demo_disks(),
        };
        let kept = self.chosen.borrow().as_ref().and_then(|chosen| {
            found
                .iter()
                .find(|disk| *disk == chosen && disks::refusal(disk).is_none())
                .cloned()
        });
        *self.chosen.borrow_mut() = kept;
        *self.disks.borrow_mut() = found;
        self.render_disks();
    }

    fn render_disks(&self) {
        let wizard = self.wizard();
        let disks = self.disks.borrow();
        let chosen = self.chosen.borrow();
        let items: Vec<Item> = disks
            .iter()
            .map(|disk| {
                let offered = disks::refusal(disk).is_none();
                let is_chosen = chosen
                    .as_ref()
                    .is_some_and(|chosen| chosen.path == disk.path);
                Item {
                    id: disk.path.as_str().into(),
                    title: model::disk_title(disk).into(),
                    detail: model::disk_detail(disk).into(),
                    action: if offered && !is_chosen { "Choose" } else { "" }.into(),
                    checked: is_chosen,
                    enabled: offered,
                    ..Item::default()
                }
            })
            .collect();
        wizard.set_disk_note(model::disk_note(&disks).into());
        wizard.set_disks(rows(items));
        // The keyboard starts on the disk that was chosen, or on the
        // first that can be. Nothing is chosen for anyone.
        let at = disks.iter().position(|disk| match chosen.as_ref() {
            Some(chosen) => chosen.path == disk.path,
            None => disks::refusal(disk).is_none(),
        });
        if let Some(at) = at {
            wizard.set_disk_current(at as i32);
        }
        wizard.set_disk_chosen(chosen.is_some());
        wizard.set_erase_warning(
            chosen
                .as_ref()
                .map(model::erase_warning)
                .unwrap_or_default()
                .into(),
        );
        drop(chosen);
        self.set_can_next();
    }

    fn pick_disk(&self, path: &str) {
        let picked = self
            .disks
            .borrow()
            .iter()
            .find(|disk| disk.path == path && disks::refusal(disk).is_none())
            .cloned();
        if picked.is_some() {
            *self.chosen.borrow_mut() = picked;
            self.render_disks();
        }
    }

    // You.

    fn typed(&self) -> Typed {
        let (password, again) = (self.password.borrow(), self.again.borrow());
        Typed {
            password_chars: password.chars(),
            again_chars: again.chars(),
            same: password.same_as(&again),
        }
    }

    fn form_ready(&self) -> bool {
        self.form
            .borrow()
            .ready(self.typed(), &self.passwd, &self.group)
    }

    fn render_form(&self) {
        let wizard = self.wizard();
        let typed = self.typed();
        let form = self.form.borrow();
        let problems = form.problems(typed, &self.passwd, &self.group);
        // Only what the program changed is written back: writing a
        // field's own text into it would move its caret.
        if wizard.get_user_name() != form.user_name.as_str() {
            wizard.set_user_name(form.user_name.as_str().into());
        }
        wizard.set_full_name_problem(problems.full_name.into());
        wizard.set_user_name_problem(problems.user_name.into());
        wizard.set_host_name_problem(problems.host_name.into());
        wizard.set_password_problem(problems.password.into());
        wizard.set_password_length(typed.password_chars as i32);
        wizard.set_again_length(typed.again_chars as i32);
        drop(form);
        self.set_can_next();
    }

    fn entry(&self, field: &str) -> &RefCell<PasswordEntry> {
        if field == "again" {
            &self.again
        } else {
            &self.password
        }
    }

    // Review.

    fn render_review(&self) {
        let chosen = self.chosen.borrow();
        let Some(disk) = chosen.as_ref() else { return };
        let wizard = self.wizard();
        wizard.set_review(facts(model::review(
            &self.form.borrow(),
            disk,
            &self.layout_name(),
            &self.zone.borrow(),
            uefi(),
        )));
        wizard.set_erase_warning(model::erase_warning(disk).into());
    }

    /// The question before the disk is erased. Its dialog starts with
    /// the keyboard on Cancel.
    fn ask_to_erase(&self) {
        let chosen = self.chosen.borrow();
        let Some(disk) = chosen.as_ref() else { return };
        let state = self.window.global::<DialogState>();
        state.set_spec(DialogSpec {
            title: format!("Erase {}?", model::disk_title(disk)).into(),
            body: format!(
                "Everything on {} will be erased. Now on it: {}. There is no undo.",
                disk.path,
                sengi_text::line(&disks::contents_words(disk), 120)
            )
            .into(),
            confirm: "Erase and install".into(),
            cancel: "Cancel".into(),
            danger: true,
            ..DialogSpec::default()
        });
        state.set_open(true);
    }

    fn close_dialog(&self) {
        self.window.global::<DialogState>().set_open(false);
        self.window.invoke_settle();
    }

    // The installation.

    fn start(self: &Rc<Self>) {
        let chosen = self.chosen.borrow().clone();
        let (Some(disk), false) = (chosen, self.installing.get()) else {
            return;
        };
        if !self.form_ready() {
            return;
        }
        let plan = self
            .form
            .borrow()
            .plan(&disk, &self.layout.borrow(), &self.zone.borrow())
            .render();
        let Some(password) = self.password.borrow_mut().take() else {
            return;
        };
        self.again.borrow_mut().clear();
        self.installing.set(true);
        *self.progress.borrow_mut() = Progress::default();

        let wizard = self.wizard();
        self.page.set("progress");
        wizard.set_place("".into());
        wizard.set_back_label("".into());
        wizard.set_next_label("".into());
        wizard.set_next_erases(false);
        wizard.set_page("progress".into());
        self.render_progress();

        let mut command = match self.demo {
            Demo::Off => {
                let mut command = Command::new("sudo");
                command.args(BACKEND);
                command
            }
            demo => {
                let mut command = Command::new("sh");
                command.args(["-c", &demo_script(demo == Demo::Fails)]);
                command
            }
        };
        let spawned = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                self.ended("it could not be started", &error.to_string());
                return;
            }
        };
        // The password first, on a line of its own, then the plan. Both
        // fit in the pipe, so this does not wait for the reader.
        if let Some(mut stdin) = child.stdin.take() {
            let written = stdin
                .write_all(password.expose().as_bytes())
                .and_then(|()| stdin.write_all(b"\n"))
                .and_then(|()| stdin.write_all(plan.as_bytes()));
            if let Err(error) = written {
                eprintln!("sengi-installer: could not hand the plan over: {error}");
            }
        }
        drop(password);

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let spawned = std::thread::Builder::new()
            .name("installer-progress".to_string())
            .spawn(move || {
                let errors = std::thread::spawn(move || {
                    let mut said = Vec::new();
                    if let Some(stderr) = stderr {
                        let _ = stderr.take(KEPT_STDERR as u64).read_to_end(&mut said);
                    }
                    String::from_utf8_lossy(&said).into_owned()
                });
                if let Some(stdout) = stdout {
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        if let Some(event) = Event::parse(&line) {
                            post(move |app| app.event(event));
                        }
                    }
                }
                let status = match child.wait() {
                    Ok(status) => status.to_string(),
                    Err(error) => error.to_string(),
                };
                let said = errors.join().unwrap_or_default();
                let last = said
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or_default()
                    .to_string();
                post(move |app| app.ended(&status, &last));
            });
        if let Err(error) = spawned {
            self.ended("its output could not be read", &error.to_string());
        }
    }

    fn event(&self, event: Event) {
        self.progress.borrow_mut().apply(event);
        self.render_progress();
    }

    /// The installing program has gone.
    fn ended(&self, status: &str, last_words: &str) {
        self.progress.borrow_mut().ended(status, last_words);
        self.installing.set(false);
        self.render_progress();
    }

    fn render_progress(&self) {
        let wizard = self.wizard();
        let progress = self.progress.borrow();
        let steps: Vec<StepRow> = progress
            .steps
            .iter()
            .map(|step| StepRow {
                text: step.title.into(),
                state: match step.state {
                    StepState::Waiting => SengiStepState::Waiting,
                    StepState::Running => SengiStepState::Running,
                    StepState::Done => SengiStepState::Done,
                    StepState::Failed => SengiStepState::Failed,
                },
                percent: step.percent.map_or(-1, i32::from),
            })
            .collect();
        wizard.set_steps(rows(steps));
        wizard.set_warnings(progress.warnings.join(" ").into());
        // While the program runs, the window waits for it. When it has
        // said how it ended and has gone, the buttons come back.
        if self.installing.get() {
            return;
        }
        let disk = self
            .chosen
            .borrow()
            .as_ref()
            .map(model::disk_title)
            .unwrap_or_default();
        match &progress.outcome {
            Outcome::Running => {}
            Outcome::Done(seconds) => {
                wizard.set_outcome("done".into());
                wizard.set_outcome_words(
                    format!(
                        "Sengi OS is on {disk}. It took {}. Restart the computer, and take the USB stick out as soon as the screen goes dark: left in, the computer may start from the stick again. Then log in with the name and password you chose.",
                        model::duration_words(*seconds)
                    )
                    .into(),
                );
                wizard.set_outcome_facts(facts(Vec::new()));
                wizard.set_next_label("Restart".into());
                wizard.set_back_label("Not now".into());
            }
            Outcome::Failed(_, why) => {
                wizard.set_outcome("failed".into());
                let state = if progress.disk_was_touched() {
                    format!("{disk} was being written when this happened and no longer holds what it did. This live session is as it was; the installer can be run again.")
                } else {
                    "Nothing on the computer was changed.".to_string()
                };
                wizard.set_outcome_words(format!("{why}. {state}").into());
                let mut rows = Vec::new();
                if !progress.log.is_empty() {
                    rows.push(("Full record", progress.log.clone()));
                }
                wizard.set_outcome_facts(facts(rows));
                wizard.set_next_label("Close".into());
                wizard.set_back_label("".into());
            }
        }
        drop(progress);
        self.set_can_next();
        self.window.invoke_focus_next();
    }

    fn next(self: &Rc<Self>) {
        match self.page.get() {
            "review" => self.ask_to_erase(),
            "progress" => {
                let done = matches!(self.progress.borrow().outcome, Outcome::Done(_));
                if done && self.demo == Demo::Off {
                    if let Err(error) = Command::new("systemctl").arg("reboot").status() {
                        eprintln!("sengi-installer: could not restart: {error}");
                    }
                }
                if !self.installing.get() {
                    let _ = slint::quit_event_loop();
                }
            }
            page => {
                let ready = match page {
                    "disk" => self.chosen.borrow().is_some(),
                    "you" => self.form_ready(),
                    _ => true,
                };
                if let (true, Some(after)) = (ready, model::page_after(page)) {
                    self.show(after);
                }
            }
        }
    }

    fn back(&self) {
        match self.page.get() {
            // "Not now", once it is installed.
            "progress" if !self.installing.get() => {
                let _ = slint::quit_event_loop();
            }
            page => {
                if let Some(before) = model::page_before(page) {
                    self.show(before);
                }
            }
        }
    }

    fn wire(self: &Rc<Self>) {
        let wizard = self.wizard();
        let weak = Rc::downgrade(self);
        macro_rules! on {
            ($setter:ident, |$app:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = weak.clone();
                wizard.$setter(move |$($arg),*| {
                    if let Some($app) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        on!(on_next, |app| app.next());
        on!(on_back, |app| app.back());
        on!(on_choose_keyboard, |app| app.choose_keyboard());
        on!(on_choose_zone, |app| app.choose_zone());
        on!(on_disk_picked, |app, id| app.pick_disk(&id));
        on!(on_look_again, |app| app.scan());
        on!(on_full_name_edited, |app, text| {
            app.form.borrow_mut().set_full_name(&text);
            app.render_form();
        });
        on!(on_user_name_edited, |app, text| {
            app.form.borrow_mut().set_user_name(&text);
            app.render_form();
        });
        on!(on_host_name_edited, |app, text| {
            app.form.borrow_mut().host_name = text.to_string();
            app.render_form();
        });
        on!(on_password_typed, |app, field, text| {
            app.entry(&field).borrow_mut().push(&text);
            app.render_form();
        });
        on!(on_password_erased, |app, field| {
            app.entry(&field).borrow_mut().erase();
            app.render_form();
        });
        on!(on_password_cleared, |app, field| {
            app.entry(&field).borrow_mut().clear();
            app.render_form();
        });

        let chooser = self.window.global::<ChooserState>();
        let app = weak.clone();
        chooser.on_chosen(move |id| {
            if let Some(app) = app.upgrade() {
                app.chosen(id.to_string());
            }
        });
        let app = weak.clone();
        chooser.on_cancelled(move || {
            if let Some(app) = app.upgrade() {
                app.close_chooser();
            }
        });
        let app = weak.clone();
        chooser.on_filtered(move |typed| {
            if let Some(app) = app.upgrade() {
                app.fill_chooser(&typed, false);
            }
        });

        let dialog = self.window.global::<DialogState>();
        let app = weak.clone();
        dialog.on_confirmed(move |_| {
            if let Some(app) = app.upgrade() {
                app.window.global::<DialogState>().set_open(false);
                app.start();
            }
        });
        let app = weak.clone();
        dialog.on_cancelled(move || {
            if let Some(app) = app.upgrade() {
                app.close_dialog();
            }
        });

        // A window that could be closed in the middle would leave the
        // installation running with nobody to tell how it went.
        let app = weak;
        self.window
            .window()
            .on_close_requested(move || match app.upgrade() {
                Some(app) if app.installing.get() => slint::CloseRequestResponse::KeepWindowShown,
                _ => slint::CloseRequestResponse::HideWindow,
            });
    }
}

fn main() -> ExitCode {
    sengi_system::set_program_name("sengi-installer");
    // First, before a password can be anywhere in memory.
    secret::forbid_core_dumps();
    #[allow(unused_mut)]
    let mut demo = Demo::Off;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("sengi-installer {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            #[cfg(debug_assertions)]
            "--demo" => demo = Demo::Works,
            #[cfg(debug_assertions)]
            "--demo-fail" => demo = Demo::Fails,
            other => {
                eprintln!("sengi-installer: unknown option {other}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    let window = match InstallerWindow::new() {
        Ok(window) => window,
        Err(error) => {
            eprintln!("sengi-installer: cannot open a window: {error}");
            return ExitCode::FAILURE;
        }
    };
    // After the first component, which is what brings the backend up, and
    // before the window is shown.
    if let Err(error) = slint::set_xdg_app_id(APP_ID) {
        eprintln!("sengi-installer: could not set the application id: {error}");
    }

    let dirs = Dirs::from_env();
    let (settings, _) = Settings::load(&dirs);
    let layout = settings
        .layouts()
        .into_iter()
        .next()
        .or_else(|| Layout::parse("us"))
        .expect("us is a layout");
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    let app = Rc::new(App {
        window,
        demo,
        page: Cell::new("welcome"),
        disks: RefCell::new(Vec::new()),
        chosen: RefCell::new(None),
        form: RefCell::new(Form {
            host_name: model::DEFAULT_HOST_NAME.to_string(),
            ..Form::default()
        }),
        password: RefCell::new(PasswordEntry::new()),
        again: RefCell::new(PasswordEntry::new()),
        layout: RefCell::new(layout),
        zone: RefCell::new("UTC".to_string()),
        database: RefCell::new(None),
        chooser: RefCell::new(None),
        progress: RefCell::new(Progress::default()),
        installing: Cell::new(false),
        passwd: read("/etc/passwd"),
        group: read("/etc/group"),
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    app.wire();
    let wizard = app.wizard();
    wizard.set_version(env!("CARGO_PKG_VERSION").into());
    wizard.set_host_name(model::DEFAULT_HOST_NAME.into());
    app.render_region();
    app.show("welcome");

    // The names of the layouts are a few hundred kilobytes to read and
    // sort, which is not for the UI thread and not needed for the first
    // page.
    let spawned = std::thread::Builder::new()
        .name("installer-xkb".to_string())
        .spawn(|| {
            let database = Database::load();
            post(move |app| {
                *app.database.borrow_mut() = Some(database);
                app.render_region();
            });
        });
    if let Err(error) = spawned {
        eprintln!("sengi-installer: could not read the keyboard layouts: {error}");
    }

    let result = app.window.run();
    APP.with(|slot| *slot.borrow_mut() = None);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-installer: {error}");
            ExitCode::FAILURE
        }
    }
}
