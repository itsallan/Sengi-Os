//! The Updates page: what could be updated, and one button for all of it.
//!
//! The work is `sengi-update`'s: looking is done as the user and needs no
//! password; installing system packages runs the helper through pkexec
//! and asks for the password here, in a dialog; apps are updated by
//! Flatpak as the user. This page keeps the state and the words. Nothing
//! runs while the window is closed, and nothing runs in the background:
//! when Settings starts it looks for updates if the last look is more
//! than a day old, and that is all. The sidebar's badge is the count of
//! the last look.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;

use sengi_update::apt::Upgrade;
use sengi_update::cache::{self, Snapshot};
use sengi_update::client::{self, Control, Note, Outcome, Pass, Reply};
use sengi_update::flatpak::AppUpdate;
use sengi_update::guard::{self, Paths};
use sengi_update::helper;
use sengi_update::{Event, Phase, Progress, Tools};

use slint::ComponentHandle;

use crate::about::root_in_memory;
use crate::app::{self, App, Controller};
use crate::secret::Secret;
use crate::ui::{DialogSpec, UpdateRow, UpdatesData};

/// What the live system says: it has no disk to update.
pub const LIVE_NOTE: &str = "Updates are available after Sengi OS is installed. This live system runs from memory and has nothing on a disk to update.";

/// A list longer than this says how many more there are.
const MAX_ROWS: usize = 60;

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// When the last look was, in words.
pub fn age_words(now: u64, then: u64) -> String {
    if then == 0 {
        return "Not checked yet".to_string();
    }
    let seconds = now.saturating_sub(then);
    match seconds {
        0..=59 => "Checked just now".to_string(),
        60..=3599 => format!(
            "Checked {} ago",
            plural((seconds / 60) as usize, "minute", "minutes")
        ),
        3600..=86_399 => format!(
            "Checked {} ago",
            plural((seconds / 3600) as usize, "hour", "hours")
        ),
        86_400..=172_799 => "Checked yesterday".to_string(),
        _ => format!(
            "Checked {} ago",
            plural((seconds / 86_400) as usize, "day", "days")
        ),
    }
}

/// The big words of the top row.
pub fn headline(snapshot: &Snapshot) -> String {
    match snapshot.total() {
        0 if snapshot.checked == 0 => "Check for updates".to_string(),
        0 => "Up to date".to_string(),
        n => format!("{} available", plural(n, "update", "updates")),
    }
}

/// The small words under them: what the updates are, and how long ago
/// they were looked for.
pub fn subline(snapshot: &Snapshot, now: u64) -> String {
    let mut parts = Vec::new();
    if !snapshot.system.is_empty() {
        let mut system = plural(snapshot.system.len(), "system package", "system packages");
        if snapshot.security() > 0 {
            system.push_str(&format!(" ({} security)", snapshot.security()));
        }
        parts.push(system);
    }
    if !snapshot.apps.is_empty() {
        parts.push(plural(snapshot.apps.len(), "app", "apps"));
    }
    if let Some(bytes) = snapshot
        .download
        .filter(|b| *b > 0 && !snapshot.system.is_empty())
    {
        parts.push(format!(
            "{} to download",
            sengi_update::apt::format_size(bytes)
        ));
    }
    let age = age_words(now, snapshot.checked);
    if parts.is_empty() {
        age
    } else {
        format!("{} \u{b7} {age}", parts.join(", "))
    }
}

/// The rows of the system list: security updates first, then by name.
pub fn system_rows(upgrades: &[Upgrade]) -> Vec<UpdateRow> {
    let mut sorted: Vec<&Upgrade> = upgrades.iter().collect();
    sorted.sort_by(|a, b| {
        b.security
            .cmp(&a.security)
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut rows: Vec<UpdateRow> = sorted
        .iter()
        .take(MAX_ROWS)
        .map(|u| UpdateRow {
            title: u.name.clone().into(),
            detail: match &u.from {
                Some(from) => format!("{from} to {}", u.to),
                None => format!("New, version {}", u.to),
            }
            .into(),
            tag: if u.security { "Security" } else { "" }.into(),
        })
        .collect();
    if sorted.len() > MAX_ROWS {
        rows.push(UpdateRow {
            title: format!("and {} more", sorted.len() - MAX_ROWS).into(),
            ..UpdateRow::default()
        });
    }
    rows
}

pub fn app_rows(apps: &[AppUpdate]) -> Vec<UpdateRow> {
    let mut rows: Vec<UpdateRow> = apps
        .iter()
        .take(MAX_ROWS)
        .map(|a| {
            let mut detail = Vec::new();
            if !a.version.is_empty() {
                detail.push(a.version.clone());
            }
            if !a.size.is_empty() {
                detail.push(a.size.clone());
            }
            UpdateRow {
                title: a.name.clone().into(),
                detail: detail.join(" \u{b7} ").into(),
                tag: Default::default(),
            }
        })
        .collect();
    if apps.len() > MAX_ROWS {
        rows.push(UpdateRow {
            title: format!("and {} more", apps.len() - MAX_ROWS).into(),
            ..UpdateRow::default()
        });
    }
    rows
}

/// The line under the bar: only what the tools said.
pub fn progress_line(progress: &Progress) -> String {
    let mut parts = Vec::new();
    if let Some((n, m)) = progress.step {
        parts.push(format!("Step {n} of {m}"));
    }
    if let Some(percent) = progress.percent {
        parts.push(format!("{}%", percent.round() as i32));
    }
    if let Some(size) = &progress.size {
        parts.push(size.clone());
    }
    if !progress.detail.is_empty() {
        parts.push(progress.detail.clone());
    }
    parts.join(" \u{b7} ")
}

/// What an update that did not simply finish tells the person.
pub fn outcome_note(outcome: &Outcome) -> Option<(String, bool)> {
    match outcome {
        Outcome::Done { .. } => None,
        Outcome::Cancelled => Some((
            "The update was cancelled. Packages that were already installed stay installed; \
             nothing was left half done."
                .to_string(),
            false,
        )),
        Outcome::Refused(words) => Some((words.clone(), true)),
        Outcome::Failed(words) => Some((format!("The update did not finish. {words}"), true)),
        Outcome::WrongPassword => Some((
            "That password was not accepted. Nothing was installed.".to_string(),
            true,
        )),
        Outcome::NoPassword => Some((
            "The update was not started, because no password was given.".to_string(),
            false,
        )),
    }
}

#[derive(Default)]
struct State {
    snapshot: Snapshot,
    live: bool,
    checking: bool,
    working: bool,
    restart: bool,
    note: Option<(String, bool)>,
    phase: Option<Phase>,
    progress: Option<Progress>,
    /// How far a check has got, for the line under its headline.
    looking: String,
}

pub struct UpdatesPage {
    state: RefCell<State>,
    /// Where answers to the password question go.
    replies: RefCell<Option<Sender<Reply>>>,
    stop: RefCell<Option<Arc<AtomicBool>>>,
    stopping: Cell<bool>,
}

fn snapshot_path() -> Option<std::path::PathBuf> {
    cache::dir().map(|dir| dir.join("updates"))
}

fn live_now() -> bool {
    std::fs::read_to_string("/proc/mounts").is_ok_and(|mounts| root_in_memory(&mounts))
}

impl UpdatesPage {
    pub fn attach(app: &Rc<App>) -> Rc<UpdatesPage> {
        let page = Rc::new(UpdatesPage {
            state: RefCell::new(State {
                snapshot: snapshot_path()
                    .map(|path| Snapshot::load(&path))
                    .unwrap_or_default(),
                live: live_now(),
                restart: guard::reboot_required(&Paths::system().reboot_required),
                ..State::default()
            }),
            replies: RefCell::default(),
            stop: RefCell::default(),
            stopping: Cell::new(false),
        });
        let data = app.window.global::<UpdatesData>();
        let (weak_page, weak_app) = (Rc::downgrade(&page), Rc::downgrade(app));
        data.on_check({
            let (page, app) = (weak_page.clone(), weak_app.clone());
            move || {
                if let (Some(page), Some(app)) = (page.upgrade(), app.upgrade()) {
                    page.start_check(&app, false);
                }
            }
        });
        data.on_update_all({
            let (page, app) = (weak_page.clone(), weak_app.clone());
            move || {
                if let (Some(page), Some(app)) = (page.upgrade(), app.upgrade()) {
                    page.start_update(&app);
                }
            }
        });
        data.on_cancel({
            let page = weak_page.clone();
            move || {
                if let Some(page) = page.upgrade() {
                    page.cancel();
                }
            }
        });
        data.on_restart({
            let (page, app) = (weak_page, weak_app);
            move || {
                if let (Some(page), Some(app)) = (page.upgrade(), app.upgrade()) {
                    page.ask_restart(&app);
                }
            }
        });
        page.render(app);
        // Once a day, when Settings is opened, and only then.
        let due = {
            let state = page.state.borrow();
            !state.live && state.snapshot.stale(sengi_update::now())
        };
        if due {
            page.start_check(app, true);
        }
        page
    }

    fn render(&self, app: &Rc<App>) {
        let state = self.state.borrow();
        let now = sengi_update::now();
        let data = app.window.global::<UpdatesData>();
        let snapshot = &state.snapshot;
        data.set_live(state.live);
        data.set_checking(state.checking);
        data.set_working(state.working);
        if state.live {
            // The sentence is in the note, which wraps; a title is cut.
            data.set_headline("Not on the live system".into());
            data.set_subline("".into());
        } else if state.checking {
            data.set_headline("Looking for updates".into());
            data.set_subline(state.looking.clone().into());
        } else {
            data.set_headline(headline(snapshot).into());
            data.set_subline(subline(snapshot, now).into());
        }
        let (note, warning) = if state.live {
            (LIVE_NOTE.to_string(), false)
        } else {
            state.note.clone().unwrap_or_default()
        };
        data.set_note(note.into());
        data.set_note_warning(warning);
        let shown = !state.live;
        data.set_system(app::model(if shown {
            system_rows(&snapshot.system)
        } else {
            Vec::new()
        }));
        data.set_system_title(format!("System packages ({})", snapshot.system.len()).into());
        data.set_apps(app::model(if shown {
            app_rows(&snapshot.apps)
        } else {
            Vec::new()
        }));
        data.set_apps_title(format!("Apps ({})", snapshot.apps.len()).into());
        data.set_can_update(shown && snapshot.total() > 0);
        data.set_restart_needed(shown && state.restart);
        data.set_badge(if shown {
            snapshot.total().min(999) as i32
        } else {
            0
        });
        data.set_phase(state.phase.map_or("", Phase::label).into());
        match &state.progress {
            Some(progress) => {
                data.set_percent(progress.percent.map_or(-1, |p| p.round() as i32));
                data.set_progress_line(progress_line(progress).into());
            }
            None => {
                data.set_percent(-1);
                data.set_progress_line("".into());
            }
        }
        data.set_can_cancel(
            state.working && !self.stopping.get() && state.phase.is_none_or(Phase::can_cancel),
        );
    }

    fn start_check(self: &Rc<Self>, app: &Rc<App>, quiet: bool) {
        {
            let mut state = self.state.borrow_mut();
            if state.live || state.checking || state.working {
                return;
            }
            state.checking = true;
            state.looking = String::new();
            if !quiet {
                state.note = None;
            }
        }
        self.render(app);
        let previous = self.state.borrow().snapshot.clone();
        let spawned = std::thread::Builder::new()
            .name("settings-updates".to_string())
            .spawn(move || {
                let stop = AtomicBool::new(false);
                let state = cache::dir().map(|dir| helper::private_state(&dir));
                let checked = client::check_all(
                    &Tools::for_user(),
                    &Paths::system(),
                    state.as_deref(),
                    &previous,
                    &stop,
                    &mut |event| {
                        if let Event::Progress(progress) = event {
                            app::post(move |app| {
                                if let Some(page) = app.page::<UpdatesPage>() {
                                    page.state.borrow_mut().looking = progress_line(&progress);
                                    page.render(app);
                                }
                            });
                        }
                    },
                );
                if checked.refused.is_none() && !checked.cancelled {
                    if let Some(path) = snapshot_path() {
                        if let Err(error) = checked.snapshot.save(&path) {
                            eprintln!("sengi-settings: could not keep the update list: {error}");
                        }
                    }
                }
                app::post(move |app| {
                    if let Some(page) = app.page::<UpdatesPage>() {
                        page.checked(app, checked, quiet);
                    }
                });
            });
        if let Err(error) = spawned {
            eprintln!("sengi-settings: could not start a thread to look for updates: {error}");
            self.state.borrow_mut().checking = false;
            self.render(app);
        }
    }

    fn checked(&self, app: &Rc<App>, checked: client::Checked, quiet: bool) {
        {
            let mut state = self.state.borrow_mut();
            state.checking = false;
            match &checked.refused {
                Some(guard::Refusal::Live) => state.live = true,
                Some(other) => state.note = Some((other.message(), true)),
                None => {
                    state.snapshot = checked.snapshot;
                    state.restart = guard::reboot_required(&Paths::system().reboot_required);
                    if !quiet && !checked.problems.is_empty() {
                        state.note = Some((checked.problems.join(" "), true));
                    }
                }
            }
        }
        self.render(app);
    }

    fn start_update(self: &Rc<Self>, app: &Rc<App>) {
        let (system, apps) = {
            let mut state = self.state.borrow_mut();
            if state.live || state.checking || state.working || state.snapshot.total() == 0 {
                return;
            }
            state.working = true;
            state.note = None;
            state.phase = None;
            state.progress = None;
            (
                !state.snapshot.system.is_empty(),
                !state.snapshot.apps.is_empty(),
            )
        };
        let (reply_tx, reply_rx) = mpsc::channel::<Reply>();
        let stop = Arc::new(AtomicBool::new(false));
        *self.replies.borrow_mut() = Some(reply_tx);
        *self.stop.borrow_mut() = Some(stop.clone());
        self.stopping.set(false);
        self.render(app);
        let spawned = std::thread::Builder::new()
            .name("settings-update".to_string())
            .spawn(move || {
                let control = Control {
                    stop,
                    replies: reply_rx,
                };
                let outcome = client::update_all(
                    &Tools::for_user(),
                    &Paths::system(),
                    system,
                    apps,
                    &control,
                    &mut |note| {
                        app::post(move |app| {
                            if let Some(page) = app.page::<UpdatesPage>() {
                                page.noted(app, note);
                            }
                        });
                    },
                );
                app::post(move |app| {
                    if let Some(page) = app.page::<UpdatesPage>() {
                        page.finished(app, outcome, system, apps);
                    }
                });
            });
        if let Err(error) = spawned {
            eprintln!("sengi-settings: could not start a thread to update: {error}");
            self.state.borrow_mut().working = false;
            self.render(app);
        }
    }

    fn noted(self: &Rc<Self>, app: &Rc<App>, note: Note) {
        match note {
            Note::NeedPassword => self.ask_password(app),
            Note::Event(Event::Phase(phase)) => {
                let mut state = self.state.borrow_mut();
                state.phase = Some(phase);
                state.progress = None;
                drop(state);
                self.render(app);
            }
            Note::Event(Event::Progress(progress)) => {
                let mut state = self.state.borrow_mut();
                state.phase = Some(progress.phase);
                state.progress = Some(progress);
                drop(state);
                self.render(app);
            }
            Note::Event(_) => {}
        }
    }

    fn ask_password(self: &Rc<Self>, app: &Rc<App>) {
        let (confirm, cancel) = (Rc::downgrade(self), Rc::downgrade(self));
        app.open_dialog(
            DialogSpec {
                title: "Update the system".into(),
                body: "Enter your password to install system updates. It is asked for \
                       each time and is not kept."
                    .into(),
                input: true,
                masked: true,
                placeholder: "Password".into(),
                confirm: "Update".into(),
                cancel: "Cancel".into(),
                minimum: 1,
                ..DialogSpec::default()
            },
            move |app, typed| {
                // From here on the password is wiped when dropped.
                let secret = Secret::new(typed);
                let pass = Pass::new(secret.expose().as_bytes().to_vec());
                app.close_dialog();
                if let Some(page) = confirm.upgrade() {
                    page.answer(Reply::Password(pass));
                }
            },
            move |_app| {
                if let Some(page) = cancel.upgrade() {
                    page.answer(Reply::Cancel);
                }
            },
        );
    }

    fn answer(&self, reply: Reply) {
        if let Some(tx) = self.replies.borrow().as_ref() {
            let _ = tx.send(reply);
        }
    }

    fn cancel(&self) {
        let can = {
            let state = self.state.borrow();
            state.working && state.phase.is_none_or(Phase::can_cancel)
        };
        if can {
            if let Some(stop) = self.stop.borrow().as_ref() {
                stop.store(true, Ordering::SeqCst);
                self.stopping.set(true);
            }
        }
    }

    fn finished(&self, app: &Rc<App>, outcome: Outcome, system: bool, apps: bool) {
        *self.replies.borrow_mut() = None;
        *self.stop.borrow_mut() = None;
        {
            let mut state = self.state.borrow_mut();
            state.working = false;
            state.phase = None;
            state.progress = None;
            state.note = outcome_note(&outcome);
            if let Outcome::Done {
                reboot_required, ..
            } = &outcome
            {
                if system {
                    state.snapshot.system.clear();
                    state.snapshot.download = None;
                }
                if apps {
                    state.snapshot.apps.clear();
                }
                state.snapshot.checked = sengi_update::now();
                state.restart = *reboot_required;
                if let Some(path) = snapshot_path() {
                    let _ = state.snapshot.save(&path);
                }
            }
        }
        self.render(app);
    }

    fn ask_restart(&self, app: &Rc<App>) {
        app.ask(
            DialogSpec {
                title: "Restart now?".into(),
                body: "Programs that are open will be closed. Save your work first.".into(),
                confirm: "Restart".into(),
                cancel: "Cancel".into(),
                danger: true,
                ..DialogSpec::default()
            },
            |_app| {
                // logind, which asks polkit whether this session may.
                let spawned = std::thread::Builder::new()
                    .name("settings-restart".to_string())
                    .spawn(|| {
                        let done = std::process::Command::new("systemctl")
                            .arg("reboot")
                            .stdin(std::process::Stdio::null())
                            .status();
                        if !done.is_ok_and(|status| status.success()) {
                            app::post(|app| {
                                app.tell(
                                    "Could not restart",
                                    "Sengi OS was not allowed to restart the computer. \
                                     Use the power menu in the bar instead.",
                                );
                            });
                        }
                    });
                if let Err(error) = spawned {
                    eprintln!("sengi-settings: could not start a thread to restart: {error}");
                }
            },
        );
    }
}

impl Controller for UpdatesPage {
    fn show(&self, app: &Rc<App>) {
        {
            let mut state = self.state.borrow_mut();
            state.live = live_now();
            if !state.working {
                state.restart = guard::reboot_required(&Paths::system().reboot_required);
            }
        }
        self.render(app);
    }

    fn shutdown(&self) {
        // A download that nobody is watching is not carried on. An
        // install that has begun is, by the helper.
        if let Some(stop) = self.stop.borrow().as_ref() {
            stop.store(true, Ordering::SeqCst);
        }
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upgrade(name: &str, from: Option<&str>, security: bool) -> Upgrade {
        Upgrade {
            name: name.to_string(),
            from: from.map(str::to_string),
            to: "2".to_string(),
            security,
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            checked: 1000,
            system: vec![
                upgrade("tzdata", Some("1"), false),
                upgrade("openssl", Some("1"), true),
                upgrade("libnew", None, false),
            ],
            apps: vec![AppUpdate {
                id: "org.gimp.GIMP".to_string(),
                name: "GIMP".to_string(),
                version: "2.10".to_string(),
                size: "112 MB".to_string(),
            }],
            download: Some(36_200_000),
            ..Snapshot::default()
        }
    }

    #[test]
    fn the_top_row_says_how_many_and_how_long_ago() {
        let s = snapshot();
        assert_eq!(headline(&s), "4 updates available");
        assert_eq!(
            subline(&s, 1000 + 300),
            "3 system packages (1 security), 1 app, 36 MB to download \u{b7} Checked 5 minutes ago"
        );
        let none = Snapshot {
            checked: 1000,
            ..Snapshot::default()
        };
        assert_eq!(headline(&none), "Up to date");
        assert_eq!(subline(&none, 1030), "Checked just now");
        assert_eq!(headline(&Snapshot::default()), "Check for updates");
        assert_eq!(subline(&Snapshot::default(), 5), "Not checked yet");
        let one = Snapshot {
            checked: 1,
            apps: s.apps.clone(),
            ..Snapshot::default()
        };
        assert_eq!(headline(&one), "1 update available");
        assert!(subline(&one, 2).starts_with("1 app "));
    }

    #[test]
    fn ages_are_in_the_biggest_unit_that_fits() {
        assert_eq!(age_words(100, 0), "Not checked yet");
        assert_eq!(age_words(100, 70), "Checked just now");
        assert_eq!(age_words(100 + 60, 100), "Checked 1 minute ago");
        assert_eq!(age_words(100 + 7200, 100), "Checked 2 hours ago");
        assert_eq!(age_words(100 + 86_400, 100), "Checked yesterday");
        assert_eq!(age_words(100 + 3 * 86_400, 100), "Checked 3 days ago");
        // A clock that went backwards is not a negative age.
        assert_eq!(age_words(50, 100), "Checked just now");
    }

    #[test]
    fn security_updates_come_first_and_new_packages_say_so() {
        let rows = system_rows(&snapshot().system);
        assert_eq!(rows[0].title, "openssl");
        assert_eq!(rows[0].tag, "Security");
        assert_eq!(rows[0].detail, "1 to 2");
        assert_eq!(rows[1].title, "libnew");
        assert_eq!(rows[1].detail, "New, version 2");
        assert_eq!(rows[1].tag, "");
        assert_eq!(rows[2].title, "tzdata");
    }

    #[test]
    fn a_long_list_says_how_many_more() {
        let many: Vec<Upgrade> = (0..75)
            .map(|n| upgrade(&format!("pkg{n:02}"), Some("1"), false))
            .collect();
        let rows = system_rows(&many);
        assert_eq!(rows.len(), MAX_ROWS + 1);
        assert_eq!(rows[MAX_ROWS].title, "and 15 more");
    }

    #[test]
    fn apps_show_version_and_size() {
        let rows = app_rows(&snapshot().apps);
        assert_eq!(rows[0].title, "GIMP");
        assert_eq!(rows[0].detail, "2.10 \u{b7} 112 MB");
    }

    #[test]
    fn the_progress_line_has_only_what_the_tools_said() {
        let full = Progress {
            phase: Phase::Download,
            percent: Some(44.6),
            step: Some((2, 7)),
            size: Some("12 MB of 36 MB".to_string()),
            detail: "Retrieving file 2 of 7".to_string(),
        };
        assert_eq!(
            progress_line(&full),
            "Step 2 of 7 \u{b7} 45% \u{b7} 12 MB of 36 MB \u{b7} Retrieving file 2 of 7"
        );
        let bare = Progress {
            phase: Phase::Flatpak,
            percent: None,
            step: None,
            size: None,
            detail: String::new(),
        };
        assert_eq!(progress_line(&bare), "");
    }

    #[test]
    fn every_way_an_update_ends_has_its_words() {
        assert_eq!(
            outcome_note(&Outcome::Done {
                upgraded: 1,
                reboot_required: false
            }),
            None
        );
        for outcome in [
            Outcome::Cancelled,
            Outcome::Refused("The battery is at 9%.".to_string()),
            Outcome::Failed("E: disk full".to_string()),
            Outcome::WrongPassword,
            Outcome::NoPassword,
        ] {
            let (words, _) = outcome_note(&outcome).expect("words");
            assert!(!words.is_empty());
        }
        let (words, warning) = outcome_note(&Outcome::Failed("E: disk full".to_string())).unwrap();
        assert!(words.contains("E: disk full") && warning);
        // Cancelling is not a fault.
        assert!(!outcome_note(&Outcome::Cancelled).unwrap().1);
    }

    #[test]
    fn the_live_system_says_updates_come_with_an_install() {
        assert!(LIVE_NOTE.starts_with("Updates are available after Sengi OS is installed"));
    }

    #[test]
    fn the_live_check_agrees_with_the_one_the_helper_makes() {
        for mounts in [
            "overlay / overlay rw 0 0\n",
            "tmpfs / tmpfs rw 0 0\n",
            "/dev/sda2 / ext4 rw 0 0\n",
            "",
        ] {
            assert_eq!(root_in_memory(mounts), guard::is_live(mounts), "{mounts:?}");
        }
    }
}
