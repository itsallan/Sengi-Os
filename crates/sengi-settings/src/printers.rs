//! The Printers page: which printers there are, which is the default,
//! what waits to print, and adding and removing printers.
//!
//! CUPS does the work and `sengi-print` drives it. Looking, choosing a
//! default, printing a test page and cancelling a job are done as the
//! user. Adding, removing and searching the network need root and go
//! through the helper under pkexec, with the password asked here in a
//! dialog (see `sengi-update`'s `elevate`). Nothing runs in the
//! background: CUPS is asked when the page is shown and when the person
//! presses Refresh, and the CUPS daemon, started by its socket for the
//! question, goes away by itself afterwards.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;

use sengi_print::cups::{self, Outcome, Problem, Snapshot, Tools};
use sengi_print::parse::{self, Found, Job, Printer, State as PrinterState};
use sengi_print::valid;
use sengi_update::client::{Control, Pass, Reply};

use slint::ComponentHandle;

use crate::about::root_in_memory;
use crate::app::{self, App, Controller};
use crate::secret::Secret;
use crate::ui::{DialogSpec, Item, PrintersData};

/// A list longer than this is cut: more printers than this is a mistake,
/// and the window is not made for it.
const MAX_ROWS: usize = 40;

/// The words for a printer's queue name: `Brother_HL-L2350DW` is shown as
/// "Brother HL-L2350DW".
pub fn display_name(name: &str) -> String {
    sengi_text::line(&name.replace('_', " "), 80)
}

/// Where a printer is connected, in a few words.
pub fn place_words(uri: &str) -> String {
    let scheme = uri.split_once("://").map_or("", |(scheme, _)| scheme);
    match scheme {
        "dnssd" => "On the network".to_string(),
        "usb" => "USB".to_string(),
        "ipp" | "ipps" | "socket" | "lpd" | "http" | "https" => {
            let rest = uri.split_once("://").map_or("", |(_, rest)| rest);
            let host = rest.split(['/', '?']).next().unwrap_or("");
            let host = host.rsplit_once(':').map_or(host, |(host, _)| host);
            if host.is_empty() {
                "On the network".to_string()
            } else {
                sengi_text::line(host, 60)
            }
        }
        _ => String::new(),
    }
}

/// What a row says under the printer's name.
pub fn printer_detail(printer: &Printer) -> String {
    let mut parts = vec![match &printer.state {
        PrinterState::Idle => "Idle".to_string(),
        PrinterState::Printing(_) => "Printing".to_string(),
        PrinterState::Offline(reason) if reason.is_empty() => "Offline".to_string(),
        PrinterState::Offline(reason) => format!("Offline, {}", lower_first(reason)),
        PrinterState::Stopped(reason) if reason.is_empty() || reason == "Paused" => {
            "Paused".to_string()
        }
        PrinterState::Stopped(reason) => format!("Paused, {}", lower_first(reason)),
    }];
    if let Some(uri) = &printer.uri {
        let place = place_words(uri);
        if !place.is_empty() {
            parts.push(place);
        }
    }
    parts.join(" \u{b7} ")
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if chars.clone().next().is_some_and(|c| c.is_lowercase()) => {
            first.to_lowercase().chain(chars).collect()
        }
        _ => text.to_string(),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The line under the top row's title.
pub fn summary(snapshot: &Snapshot) -> String {
    let n = snapshot.printers.len();
    if n == 0 {
        return "No printers added yet".to_string();
    }
    let count = plural(n, "printer", "printers");
    match &snapshot.default {
        Some(default) => format!("{count} \u{b7} {} is the default", display_name(default)),
        None => count,
    }
}

/// The printer the buttons are for: the one the person picked if it is
/// still there, else the default one, else the first.
pub fn chosen<'a>(snapshot: &'a Snapshot, picked: Option<&str>) -> Option<&'a Printer> {
    picked
        .and_then(|name| snapshot.printers.iter().find(|p| p.name == name))
        .or_else(|| {
            snapshot
                .default
                .as_deref()
                .and_then(|name| snapshot.printers.iter().find(|p| p.name == name))
        })
        .or_else(|| snapshot.printers.first())
}

pub fn printer_rows(snapshot: &Snapshot, picked: Option<&str>) -> Vec<Item> {
    let selected = chosen(snapshot, picked).map(|p| p.name.as_str());
    snapshot
        .printers
        .iter()
        .take(MAX_ROWS)
        .map(|printer| {
            let mut row = app::item(
                &printer.name,
                &display_name(&printer.name),
                &printer_detail(printer),
            );
            row.icon = "printer".into();
            row.action = "Manage".into();
            if snapshot.default.as_deref() == Some(printer.name.as_str()) {
                row.status = "Default".into();
            }
            row.checked = selected == Some(printer.name.as_str());
            row
        })
        .collect()
}

pub fn job_rows(jobs: &[Job]) -> Vec<Item> {
    jobs.iter()
        .take(MAX_ROWS)
        .map(|job| {
            let number = job.id.rsplit_once('-').map_or("", |(_, n)| n);
            let mut detail = vec![job.user.clone(), parse::size_words(job.bytes)];
            if !job.when.is_empty() {
                detail.push(job.when.clone());
            }
            let mut row = app::item(
                &job.id,
                &format!("Job {number} on {}", display_name(&job.printer)),
                &detail.join(" \u{b7} "),
            );
            row.icon = "file".into();
            row.remove = "Cancel".into();
            row
        })
        .collect()
}

pub fn found_rows(found: &[Found]) -> Vec<Item> {
    found
        .iter()
        .take(MAX_ROWS)
        .enumerate()
        .map(|(index, found)| {
            let mut row = app::item(&index.to_string(), &found.title, &place_words(&found.uri));
            row.icon = "printer".into();
            row.action = "Add".into();
            row
        })
        .collect()
}

/// The queue name for a printer called `title`, one that is not taken.
pub fn queue_name(title: &str, snapshot: &Snapshot) -> String {
    let taken: Vec<String> = snapshot.printers.iter().map(|p| p.name.clone()).collect();
    valid::unique_name(&valid::name_from_display(title), &taken)
}

/// The title to name a printer by when it was added by its address.
pub fn title_for_address(uri: &str) -> String {
    let host = place_words(uri);
    if host.is_empty() || host == "On the network" || host == "USB" {
        "Printer".to_string()
    } else if host.chars().all(|c| c.is_ascii_digit() || c == '.') {
        format!("Printer {host}")
    } else {
        host
    }
}

/// What a failed change tells the person.
pub fn failure_note<T>(what: &str, outcome: &Outcome<T>) -> Option<(String, bool)> {
    match outcome {
        Outcome::Done(_) | Outcome::Stopped => None,
        Outcome::Failed(words) => Some((format!("{what} {words}"), true)),
        Outcome::WrongPassword => Some((
            "That password was not accepted. Nothing was changed.".to_string(),
            true,
        )),
        Outcome::NoPassword => Some((
            "Nothing was changed, because no password was given.".to_string(),
            false,
        )),
    }
}

#[derive(Default)]
struct State {
    snapshot: Snapshot,
    loaded: bool,
    loading: bool,
    /// What is being changed, in words, while it is.
    busy: Option<String>,
    problem: Option<Problem>,
    note: Option<(String, bool)>,
    picked: Option<String>,
    adding: bool,
    searching: bool,
    searched: bool,
    found: Vec<Found>,
    address_problem: String,
    /// The root file system is in memory: nothing can be installed.
    live: bool,
}

pub struct PrintersPage {
    state: RefCell<State>,
    tools: Tools,
    /// Where answers to the password question go.
    replies: RefCell<Option<Sender<Reply>>>,
    stop: RefCell<Option<Arc<AtomicBool>>>,
}

/// What a privileged job brings back.
enum Done {
    Found(Vec<Found>),
    Added(String),
    Removed,
    Installed,
}

enum Task {
    Discover,
    Add { name: String, uri: String },
    Remove { name: String },
    Install,
}

impl Task {
    /// How a note begins when the job did not work.
    fn failure(&self) -> &'static str {
        match self {
            Task::Discover => "The search did not finish.",
            Task::Add { .. } => "The printer was not added.",
            Task::Remove { .. } => "The printer was not removed.",
            Task::Install => "Printing support was not installed.",
        }
    }
}

/// What the install offer says it costs: the numbers of `cups` and
/// `cups-client` and what they pull in, measured on Debian 13.
pub const INSTALL_DETAIL: &str = "Adds CUPS, the print system: about 87 MB on disk and 25 MB \
    to download. It needs an internet connection and your password, and nothing of it runs \
    until something prints.";
pub const INSTALL_LIVE: &str = "Printing can be installed after Sengi OS is installed on this \
    computer. This live system runs from memory and has no room for it.";

impl PrintersPage {
    pub fn attach(app: &Rc<App>) -> Rc<PrintersPage> {
        Self::attach_with(app, Tools::for_user())
    }

    pub fn attach_with(app: &Rc<App>, tools: Tools) -> Rc<PrintersPage> {
        let page = Rc::new(PrintersPage {
            state: RefCell::default(),
            tools,
            replies: RefCell::default(),
            stop: RefCell::default(),
        });
        let data = app.window.global::<PrintersData>();
        let on = |handler: fn(&Rc<PrintersPage>, &Rc<App>)| {
            let (page, app) = (Rc::downgrade(&page), Rc::downgrade(app));
            move || {
                if let (Some(page), Some(app)) = (page.upgrade(), app.upgrade()) {
                    handler(&page, &app);
                }
            }
        };
        let on_text = |handler: fn(&Rc<PrintersPage>, &Rc<App>, &str)| {
            let (page, app) = (Rc::downgrade(&page), Rc::downgrade(app));
            move |text: slint::SharedString| {
                if let (Some(page), Some(app)) = (page.upgrade(), app.upgrade()) {
                    handler(&page, &app, text.as_str());
                }
            }
        };
        data.on_refresh(on(|page, app| page.refresh(app)));
        data.on_install_support(on(|page, app| page.ask_install(app)));
        data.on_select(on_text(|page, app, name| page.select(app, name)));
        data.on_make_default(on(|page, app| page.make_default(app)));
        data.on_test_page(on(|page, app| page.test_page(app)));
        data.on_remove(on(|page, app| page.ask_remove(app)));
        data.on_cancel_job(on_text(|page, app, id| page.cancel_job(app, id)));
        data.on_open_add(on(|page, app| page.open_add(app)));
        data.on_close_add(on(|page, app| page.close_add(app)));
        data.on_search(on(|page, app| page.search(app)));
        data.on_add_found(on_text(|page, app, index| page.add_found(app, index)));
        data.on_add_address(on_text(|page, app, text| page.add_address(app, text)));
        page.state.borrow_mut().live = live_now();
        page.render(app);
        page
    }

    fn render(&self, app: &Rc<App>) {
        let state = self.state.borrow();
        let data = app.window.global::<PrintersData>();
        let snapshot = &state.snapshot;
        let missing = matches!(state.problem, Some(Problem::NotInstalled));
        data.set_available(!missing);
        data.set_can_install(missing);
        data.set_install_blocked(state.live);
        data.set_install_detail(
            if state.live {
                INSTALL_LIVE
            } else {
                INSTALL_DETAIL
            }
            .into(),
        );
        let (note, warning) = match (&state.note, &state.problem) {
            (Some(note), _) => note.clone(),
            // The card that offers the install says it is not there.
            (None, Some(Problem::NotInstalled)) => (String::new(), false),
            (None, Some(problem)) => (problem.message(), true),
            (None, None) => (String::new(), false),
        };
        data.set_note(note.into());
        data.set_note_warning(warning);
        data.set_loading(state.loading && !state.loaded);
        data.set_busy(state.busy.is_some());
        data.set_busy_text(state.busy.clone().unwrap_or_default().into());
        data.set_summary(if state.loaded {
            summary(snapshot).into()
        } else {
            "".into()
        });
        data.set_printers(app::model(printer_rows(snapshot, state.picked.as_deref())));
        data.set_jobs(app::model(job_rows(&snapshot.jobs)));
        let picked = chosen(snapshot, state.picked.as_deref());
        data.set_has_chosen(picked.is_some());
        data.set_chosen_title(
            picked
                .map_or(String::new(), |p| display_name(&p.name))
                .into(),
        );
        data.set_chosen_detail(picked.map_or(String::new(), printer_detail).into());
        data.set_chosen_default(
            picked.is_some_and(|p| snapshot.default.as_deref() == Some(p.name.as_str())),
        );
        data.set_adding(state.adding);
        data.set_searching(state.searching);
        data.set_searched(state.searched);
        data.set_found(app::model(found_rows(&state.found)));
        data.set_address_problem(state.address_problem.clone().into());
    }

    /// Refresh, pressed: what was said last is old news.
    fn refresh(self: &Rc<Self>, app: &Rc<App>) {
        self.state.borrow_mut().note = None;
        self.reload(app);
    }

    fn reload(self: &Rc<Self>, app: &Rc<App>) {
        {
            let mut state = self.state.borrow_mut();
            if state.loading {
                return;
            }
            state.loading = true;
        }
        self.render(app);
        let tools = self.tools.clone();
        spawn("settings-printers", move || {
            let answer = cups::snapshot(&tools, &AtomicBool::new(false));
            app::post(move |app| {
                if let Some(page) = app.page::<PrintersPage>() {
                    page.loaded(app, answer);
                }
            });
        });
    }

    fn loaded(&self, app: &Rc<App>, answer: Result<Snapshot, Problem>) {
        {
            let mut state = self.state.borrow_mut();
            state.loading = false;
            match answer {
                Ok(snapshot) => {
                    state.snapshot = snapshot;
                    state.loaded = true;
                    state.problem = None;
                }
                Err(problem) => {
                    state.problem = Some(problem);
                    state.snapshot = Snapshot::default();
                    state.loaded = true;
                }
            }
        }
        self.render(app);
    }

    fn select(&self, app: &Rc<App>, name: &str) {
        if !self
            .state
            .borrow()
            .snapshot
            .printers
            .iter()
            .any(|p| p.name == name)
        {
            return;
        }
        self.state.borrow_mut().picked = Some(name.to_string());
        self.render(app);
    }

    /// The name of the printer the buttons are for.
    fn current(&self) -> Option<String> {
        let state = self.state.borrow();
        chosen(&state.snapshot, state.picked.as_deref()).map(|p| p.name.clone())
    }

    fn make_default(self: &Rc<Self>, app: &Rc<App>) {
        let Some(name) = self.current() else { return };
        let title = display_name(&name);
        self.user_job(
            app,
            format!("Making {title} the default\u{2026}"),
            move |tools| {
                cups::set_default(tools, &name)
                    .map(|()| (format!("{title} is now your default printer."), false))
            },
        );
    }

    fn test_page(self: &Rc<Self>, app: &Rc<App>) {
        let Some(name) = self.current() else { return };
        let title = display_name(&name);
        self.user_job(
            app,
            format!("Sending a test page to {title}\u{2026}"),
            move |tools| {
                cups::test_page(tools, &name)
                    .map(|_| (format!("A test page was sent to {title}."), false))
            },
        );
    }

    fn cancel_job(self: &Rc<Self>, app: &Rc<App>, id: &str) {
        if !self.state.borrow().snapshot.jobs.iter().any(|j| j.id == id) {
            return;
        }
        let id = id.to_string();
        self.user_job(
            app,
            "Cancelling the job\u{2026}".to_string(),
            move |tools| {
                cups::cancel_job(tools, &id).map(|()| ("The job was cancelled.".to_string(), false))
            },
        );
    }

    /// Something done as the user: a line to show while it runs, then what
    /// it says about itself, and the list is read again.
    fn user_job(
        self: &Rc<Self>,
        app: &Rc<App>,
        busy: String,
        work: impl FnOnce(&Tools) -> Result<(String, bool), Problem> + Send + 'static,
    ) {
        {
            let mut state = self.state.borrow_mut();
            if state.busy.is_some() {
                return;
            }
            state.busy = Some(busy);
            state.note = None;
        }
        self.render(app);
        let tools = self.tools.clone();
        spawn("settings-printers-job", move || {
            let result = work(&tools);
            app::post(move |app| {
                if let Some(page) = app.page::<PrintersPage>() {
                    {
                        let mut state = page.state.borrow_mut();
                        state.busy = None;
                        state.note = Some(match result {
                            Ok(done) => done,
                            Err(problem) => (problem.message(), true),
                        });
                    }
                    page.render(app);
                    page.reload(app);
                }
            });
        });
    }

    fn open_add(&self, app: &Rc<App>) {
        {
            let mut state = self.state.borrow_mut();
            state.adding = true;
            state.searched = false;
            state.found.clear();
            state.address_problem.clear();
        }
        self.render(app);
    }

    fn close_add(&self, app: &Rc<App>) {
        self.stop_search();
        {
            let mut state = self.state.borrow_mut();
            state.adding = false;
            state.searching = false;
            state.found.clear();
            state.address_problem.clear();
        }
        self.render(app);
    }

    fn stop_search(&self) {
        if let Some(stop) = self.stop.borrow().as_ref() {
            stop.store(true, Ordering::SeqCst);
        }
    }

    fn search(self: &Rc<Self>, app: &Rc<App>) {
        {
            let mut state = self.state.borrow_mut();
            if state.searching || state.busy.is_some() {
                return;
            }
            state.searching = true;
            state.searched = false;
            state.found.clear();
            state.address_problem.clear();
            state.note = None;
        }
        self.render(app);
        self.run_privileged(app, Task::Discover);
    }

    fn add_found(self: &Rc<Self>, app: &Rc<App>, index: &str) {
        let picked = {
            let state = self.state.borrow();
            index
                .parse::<usize>()
                .ok()
                .and_then(|i| state.found.get(i).cloned())
                .map(|found| (queue_name(&found.title, &state.snapshot), found))
        };
        let Some((name, found)) = picked else { return };
        self.begin_add(app, name, found.uri, found.title);
    }

    fn add_address(self: &Rc<Self>, app: &Rc<App>, text: &str) {
        match valid::address_to_uri(text) {
            Err(error) => {
                self.state.borrow_mut().address_problem = error.message().to_string();
                self.render(app);
            }
            Ok(uri) => {
                let title = title_for_address(&uri);
                let name = queue_name(&title, &self.state.borrow().snapshot);
                self.begin_add(app, name, uri, title);
            }
        }
    }

    fn begin_add(self: &Rc<Self>, app: &Rc<App>, name: String, uri: String, title: String) {
        {
            let mut state = self.state.borrow_mut();
            if state.busy.is_some() || state.searching {
                return;
            }
            state.busy = Some(format!("Adding {title}\u{2026}"));
            state.address_problem.clear();
            state.note = None;
        }
        self.render(app);
        self.run_privileged(app, Task::Add { name, uri });
    }

    fn ask_install(self: &Rc<Self>, app: &Rc<App>) {
        if self.state.borrow().live {
            return;
        }
        let page = Rc::downgrade(self);
        app.ask(
            DialogSpec {
                title: "Install printing support?".into(),
                body: "This downloads about 25 MB and adds CUPS, the print system, which \
                       takes about 87 MB on disk. It asks for your password, and the install \
                       cannot be stopped once it has begun."
                    .into(),
                confirm: "Install".into(),
                cancel: "Cancel".into(),
                ..DialogSpec::default()
            },
            move |app| {
                let Some(page) = page.upgrade() else { return };
                {
                    let mut state = page.state.borrow_mut();
                    if state.busy.is_some() || state.live {
                        return;
                    }
                    state.busy = Some(
                        "Installing printing support. This can take a few minutes\u{2026}"
                            .to_string(),
                    );
                    state.note = None;
                }
                page.render(app);
                page.run_privileged(app, Task::Install);
            },
        );
    }

    fn ask_remove(self: &Rc<Self>, app: &Rc<App>) {
        let Some(name) = self.current() else { return };
        let title = display_name(&name);
        let page = Rc::downgrade(self);
        app.ask(
            DialogSpec {
                title: format!("Remove {title}?").into(),
                body: "The printer is taken off this computer. It can be added again. \
                       Jobs that are waiting for it are cancelled."
                    .into(),
                confirm: "Remove".into(),
                cancel: "Cancel".into(),
                danger: true,
                ..DialogSpec::default()
            },
            move |app| {
                let Some(page) = page.upgrade() else { return };
                {
                    let mut state = page.state.borrow_mut();
                    if state.busy.is_some() {
                        return;
                    }
                    state.busy = Some(format!("Removing {title}\u{2026}"));
                    state.note = None;
                }
                page.render(app);
                page.run_privileged(app, Task::Remove { name });
            },
        );
    }

    /// Runs `job` through the helper on a thread of its own. The password
    /// is asked for in a dialog when pkexec asks, and the answer goes
    /// back to it.
    fn run_privileged(self: &Rc<Self>, _app: &Rc<App>, job: Task) {
        let (reply_tx, reply_rx) = mpsc::channel::<Reply>();
        let stop = Arc::new(AtomicBool::new(false));
        *self.replies.borrow_mut() = Some(reply_tx);
        *self.stop.borrow_mut() = Some(stop.clone());
        let tools = self.tools.clone();
        spawn("settings-printers-root", move || {
            let control = Control {
                stop,
                replies: reply_rx,
            };
            let mut ask = || {
                app::post(|app| {
                    if let Some(page) = app.page::<PrintersPage>() {
                        page.ask_password(app);
                    }
                });
            };
            let outcome = match &job {
                Task::Discover => cups::discover(&tools, &control, &mut ask).map(Done::Found),
                Task::Add { name, uri } => cups::add(&tools, name, uri, &control, &mut ask)
                    .map(|()| Done::Added(name.clone())),
                Task::Remove { name } => {
                    cups::remove(&tools, name, &control, &mut ask).map(|()| Done::Removed)
                }
                Task::Install => {
                    cups::install_support(&tools, &control, &mut ask).map(|()| Done::Installed)
                }
            };
            let (searching, what) = (matches!(job, Task::Discover), job.failure());
            app::post(move |app| {
                if let Some(page) = app.page::<PrintersPage>() {
                    page.privileged_done(app, outcome, searching, what);
                }
            });
        });
    }

    fn privileged_done(
        self: &Rc<Self>,
        app: &Rc<App>,
        outcome: Outcome<Done>,
        searching: bool,
        what: &'static str,
    ) {
        *self.replies.borrow_mut() = None;
        *self.stop.borrow_mut() = None;
        // A password question that was left open is closed with its job.
        if app.dialog_open() {
            app.close_dialog();
        }
        let mut reload = false;
        {
            let mut state = self.state.borrow_mut();
            state.busy = None;
            if searching {
                state.searching = false;
                state.searched = !matches!(outcome, Outcome::Stopped);
            }
            state.note = failure_note(what, &outcome);
            match outcome {
                Outcome::Done(Done::Found(found)) => state.found = found,
                Outcome::Done(Done::Added(name)) => {
                    state.note = Some((format!("{} was added.", display_name(&name)), false));
                    state.picked = Some(name);
                    state.adding = false;
                    state.found.clear();
                    reload = true;
                }
                Outcome::Done(Done::Removed) => {
                    state.note = Some(("The printer was removed.".to_string(), false));
                    state.picked = None;
                    reload = true;
                }
                Outcome::Done(Done::Installed) => {
                    state.note = Some(("Printing support was installed.".to_string(), false));
                    reload = true;
                }
                _ => {}
            }
        }
        self.render(app);
        if reload {
            self.reload(app);
        }
    }

    fn ask_password(self: &Rc<Self>, app: &Rc<App>) {
        let (confirm, cancel) = (Rc::downgrade(self), Rc::downgrade(self));
        app.open_dialog(
            DialogSpec {
                title: "Change the printers".into(),
                body: "Enter your password to search for and change printers. It is asked \
                       for each time and is not kept."
                    .into(),
                input: true,
                masked: true,
                placeholder: "Password".into(),
                confirm: "Continue".into(),
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
}

fn live_now() -> bool {
    std::fs::read_to_string("/proc/mounts").is_ok_and(|mounts| root_in_memory(&mounts))
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
    if let Err(error) = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(work)
    {
        eprintln!("sengi-settings: could not start a thread ({name}): {error}");
    }
}

impl Controller for PrintersPage {
    fn show(&self, app: &Rc<App>) {
        // The page is looked at each time it comes into view, and
        // not again until then or until Refresh.
        if let Some(page) = app.page::<PrintersPage>() {
            page.state.borrow_mut().live = live_now();
            page.reload(app);
        }
    }

    fn hide(&self, _app: &Rc<App>) {
        self.stop_search();
    }

    fn shutdown(&self) {
        self.stop_search();
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printer(name: &str, state: PrinterState, uri: Option<&str>) -> Printer {
        Printer {
            name: name.to_string(),
            state,
            uri: uri.map(str::to_string),
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            printers: vec![
                printer(
                    "Office",
                    PrinterState::Idle,
                    Some("ipp://192.168.1.50/ipp/print"),
                ),
                printer(
                    "Garage_Laser",
                    PrinterState::Offline("Unable to connect to printer".to_string()),
                    Some("dnssd://Garage%20Laser._ipp._tcp.local/?uuid=1"),
                ),
            ],
            default: Some("Office".to_string()),
            jobs: vec![Job {
                id: "Office-12".to_string(),
                printer: "Office".to_string(),
                user: "allan".to_string(),
                bytes: 2048,
                when: "Thu 26 Nov 2026 10:00:00 AM CET".to_string(),
            }],
        }
    }

    #[test]
    fn rows_show_the_name_state_place_and_default() {
        let rows = printer_rows(&snapshot(), None);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "Office");
        assert_eq!(rows[0].detail, "Idle \u{b7} 192.168.1.50");
        assert_eq!(rows[0].status, "Default");
        assert!(rows[0].checked);
        assert_eq!(rows[1].title, "Garage Laser");
        assert_eq!(
            rows[1].detail,
            "Offline, unable to connect to printer \u{b7} On the network"
        );
        assert_eq!(rows[1].status, "");
        assert!(!rows[1].checked);
        assert_eq!(rows[1].id, "Garage_Laser");
    }

    #[test]
    fn the_person_picks_one_and_the_pick_is_remembered_while_it_exists() {
        let s = snapshot();
        let rows = printer_rows(&s, Some("Garage_Laser"));
        assert!(!rows[0].checked && rows[1].checked);
        // A pick that is gone falls back to the default.
        assert_eq!(chosen(&s, Some("Gone")).unwrap().name, "Office");
        let no_default = Snapshot { default: None, ..s };
        assert_eq!(chosen(&no_default, None).unwrap().name, "Office");
        assert!(chosen(&Snapshot::default(), None).is_none());
    }

    #[test]
    fn the_summary_counts_and_names_the_default() {
        assert_eq!(summary(&Snapshot::default()), "No printers added yet");
        assert_eq!(
            summary(&snapshot()),
            "2 printers \u{b7} Office is the default"
        );
        let one = Snapshot {
            printers: vec![printer("A_B", PrinterState::Idle, None)],
            default: None,
            jobs: vec![],
        };
        assert_eq!(summary(&one), "1 printer");
    }

    #[test]
    fn states_have_their_words() {
        let p = |state| printer("X", state, None);
        assert_eq!(printer_detail(&p(PrinterState::Printing(None))), "Printing");
        assert_eq!(
            printer_detail(&p(PrinterState::Offline(String::new()))),
            "Offline"
        );
        assert_eq!(
            printer_detail(&p(PrinterState::Stopped("Paused".to_string()))),
            "Paused"
        );
        assert_eq!(
            printer_detail(&p(PrinterState::Stopped("Out of paper".to_string()))),
            "Paused, out of paper"
        );
    }

    #[test]
    fn places_are_a_host_not_a_whole_address() {
        assert_eq!(
            place_words("ipp://printer.local:631/ipp/print"),
            "printer.local"
        );
        assert_eq!(place_words("socket://10.0.0.9:9100"), "10.0.0.9");
        assert_eq!(place_words("dnssd://x._ipp._tcp.local/"), "On the network");
        assert_eq!(place_words("usb://HP/LaserJet"), "USB");
        assert_eq!(place_words("weird"), "");
    }

    #[test]
    fn the_queue_is_a_list_of_jobs_with_a_way_to_cancel() {
        let rows = job_rows(&snapshot().jobs);
        assert_eq!(rows[0].id, "Office-12");
        assert_eq!(rows[0].title, "Job 12 on Office");
        assert_eq!(
            rows[0].detail,
            "allan \u{b7} 2 KB \u{b7} Thu 26 Nov 2026 10:00:00 AM CET"
        );
        assert_eq!(rows[0].remove, "Cancel");
        assert_eq!(rows[0].action, "");
    }

    #[test]
    fn found_printers_are_added_by_their_place_in_the_list() {
        let rows = found_rows(&[
            Found {
                title: "Brother HL".to_string(),
                uri: "dnssd://Brother%20HL._ipp._tcp.local/".to_string(),
            },
            Found {
                title: "Office".to_string(),
                uri: "ipp://office.local/ipp/print".to_string(),
            },
        ]);
        assert_eq!(rows[0].id, "0");
        assert_eq!(rows[1].id, "1");
        assert_eq!(rows[1].detail, "office.local");
        assert_eq!(rows[0].action, "Add");
    }

    #[test]
    fn a_new_queue_gets_a_name_that_is_valid_and_not_taken() {
        let s = snapshot();
        assert_eq!(
            queue_name("Brother HL-L2350DW series", &s),
            "Brother_HL-L2350DW_series"
        );
        assert_eq!(queue_name("office", &s), "office_2");
        assert!(valid::printer_name(&queue_name("\u{202e}$(rm -rf)", &s)));
        assert_eq!(
            title_for_address("ipp://192.168.1.50/ipp/print"),
            "Printer 192.168.1.50"
        );
        assert_eq!(
            title_for_address("ipp://brw3c2af4.local/ipp/print"),
            "brw3c2af4.local"
        );
        assert!(valid::printer_name(&queue_name(
            &title_for_address("ipp://192.168.1.50/ipp/print"),
            &s
        )));
    }

    #[test]
    fn every_way_a_change_fails_has_its_words() {
        assert_eq!(failure_note("x", &Outcome::Done(())), None);
        assert_eq!(failure_note("x", &Outcome::<()>::Stopped), None);
        for outcome in [
            Outcome::<()>::Failed("lpadmin: boom".to_string()),
            Outcome::WrongPassword,
            Outcome::NoPassword,
        ] {
            let (words, _) = failure_note("The printer was not added.", &outcome).unwrap();
            assert!(!words.is_empty());
        }
        let (words, warning) =
            failure_note("Not added.", &Outcome::<()>::Failed("boom".into())).unwrap();
        assert_eq!(words, "Not added. boom");
        assert!(warning);
        assert!(!failure_note("x", &Outcome::<()>::NoPassword).unwrap().1);
    }

    #[test]
    fn sentences_that_start_with_a_name_keep_their_capital() {
        assert_eq!(lower_first("Out of paper"), "out of paper");
        assert_eq!(lower_first("HP error"), "HP error");
        assert_eq!(lower_first(""), "");
    }
}
