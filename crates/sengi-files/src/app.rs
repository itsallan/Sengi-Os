//! Files' window and what it does. The window is touched only from the
//! event loop. A folder is read, and a paste or the emptying of the Trash
//! done, on a thread of its own that posts what it found back here, so a
//! folder of ten thousand files never holds the window up.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, VecModel};

use sengi_apps::Env;
use sengi_system::trash;

use crate::fsops::{self, Entry};
use crate::jobs::{self, Report};
use crate::open;
use crate::session::{places, Place, Session};
use crate::ui::{Crumb, DialogSpec, FileRow, Files, FilesWindow};

/// What to do when a load is done.
#[derive(Default, Clone)]
struct Then {
    /// Names to select.
    select: Vec<String>,
    /// Start renaming the one selected.
    rename: bool,
    /// Scroll to the top: it is a new place.
    top: bool,
}

enum Asking {
    EmptyTrash,
}

struct State {
    session: Session,
    env: Env,
    places: Vec<(String, Place)>,
    /// The first row the list shows and how many it has room for.
    view: (usize, usize),
    notice: String,
    notice_error: bool,
    renaming: Option<usize>,
    busy: bool,
    free: Option<u64>,
    cancel: Arc<AtomicBool>,
    then: Then,
    asking: Option<Asking>,
    top_tick: i32,
    reveal_tick: i32,
    /// Tests have no event loop: work is done at once, not on a thread.
    sync: bool,
}

pub struct App {
    window: slint::Weak<FilesWindow>,
    state: RefCell<State>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Runs `then` on the event loop, with the app.
fn post(then: impl FnOnce(&Rc<App>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        let app = APP.with(|app| app.borrow().clone());
        if let Some(app) = app {
            then(&app);
        }
    });
}

fn tilde(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// "2026-11-25T10:20:30" as "2026-11-25 10:20".
fn deleted_text(date: &str) -> String {
    match date.get(..16) {
        Some(short) => format!("Deleted {}", short.replacen('T', " ", 1)),
        None => String::new(),
    }
}

fn read_error(place: &Path, error: &std::io::Error) -> String {
    let name = place.file_name().map_or_else(
        || place.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    match error.kind() {
        std::io::ErrorKind::NotFound => format!("\"{name}\" is not there any more."),
        std::io::ErrorKind::PermissionDenied => {
            format!("You do not have permission to open \"{name}\".")
        }
        _ => format!("Cannot open \"{name}\": {}.", trash::plain(error)),
    }
}

fn plural(count: usize) -> String {
    if count == 1 {
        "1 item".to_string()
    } else {
        format!("{count} items")
    }
}

impl State {
    fn row(&self, at: usize) -> FileRow {
        let entry = &self.session.entries[at];
        let cut = self.session.clipboard.as_ref().is_some_and(|clipboard| {
            clipboard.cut
                && self
                    .session
                    .path_of(at)
                    .is_some_and(|path| clipboard.items.contains(&path))
        });
        let (size, detail) = if self.session.in_trash() {
            (
                entry
                    .original
                    .as_ref()
                    .and_then(|path| path.parent())
                    .map(|parent| tilde(&self.session.home, parent))
                    .unwrap_or_default(),
                deleted_text(&entry.deleted),
            )
        } else {
            (
                if entry.folder {
                    String::new()
                } else {
                    fsops::size_text(entry.size)
                },
                fsops::date_text(entry.modified),
            )
        };
        FileRow {
            index: at as i32,
            name: entry.name.as_str().into(),
            folder: entry.folder,
            selected: self.session.selected.contains(&at),
            cut,
            size: size.into(),
            detail: detail.into(),
        }
    }

    fn status(&self) -> String {
        let session = &self.session;
        if self.busy {
            return "Working\u{2026}".to_string();
        }
        if let Some(read) = session.loading {
            return if read == 0 {
                "Reading folder\u{2026}".to_string()
            } else {
                format!("Reading folder\u{2026} {read} items")
            };
        }
        if session.problem.is_some() {
            return String::new();
        }
        let mut text = session.count_text();
        if session.skipped > 0 {
            text.push_str(&format!(
                ", {} with unreadable names not shown",
                plural(session.skipped)
            ));
        }
        if let Some(free) = self.free.filter(|_| !session.in_trash()) {
            text.push_str(&format!(" \u{b7} {} free", fsops::size_text(free)));
        }
        text
    }

    fn hint(&self) -> String {
        let session = &self.session;
        if let Some(problem) = &session.problem {
            problem.clone()
        } else if session.loading.is_some() || !session.entries.is_empty() {
            String::new()
        } else if session.in_trash() {
            "The Trash is empty.".to_string()
        } else {
            "This folder is empty.".to_string()
        }
    }
}

impl App {
    fn render(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let s = self.state.borrow();
        let files = window.global::<Files>();
        let total = s.session.entries.len();
        let first = s.view.0.min(total);
        let end = (first + s.view.1).min(total);
        let rows: Vec<FileRow> = (first..end).map(|at| s.row(at)).collect();
        files.set_rows(ModelRc::new(VecModel::from(rows)));
        files.set_first(first as i32);
        files.set_total(total as i32);
        files.set_cursor(s.session.cursor as i32);
        files.set_places(ModelRc::new(VecModel::from(
            s.places
                .iter()
                .map(|(name, _)| name.as_str().into())
                .collect::<Vec<slint::SharedString>>(),
        )));
        files.set_place(
            s.places
                .iter()
                .position(|(_, place)| *place == s.session.place)
                .map_or(-1, |at| at as i32),
        );
        files.set_crumbs(ModelRc::new(VecModel::from(
            s.session
                .crumbs()
                .into_iter()
                .map(|crumb| Crumb {
                    label: crumb.label.into(),
                })
                .collect::<Vec<_>>(),
        )));
        files.set_in_trash(s.session.in_trash());
        files.set_can_back(s.session.can_back());
        files.set_can_up(s.session.parent_place().is_some());
        files.set_can_paste(s.session.clipboard.is_some() && !s.session.in_trash());
        files.set_has_selection(!s.session.selected.is_empty());
        files.set_busy(s.busy);
        files.set_show_hidden(s.session.show_hidden);
        files.set_status(s.status().into());
        files.set_notice(s.notice.as_str().into());
        files.set_notice_is_error(s.notice_error);
        files.set_hint(s.hint().into());
        files.set_renaming(s.renaming.map_or(-1, |at| at as i32));
        files.set_scroll_top(s.top_tick);
        files.set_reveal_tick(s.reveal_tick);
        files.set_reveal_index(s.session.cursor as i32);
    }

    fn say(&self, text: impl Into<String>, error: bool) {
        let mut s = self.state.borrow_mut();
        s.notice = text.into();
        s.notice_error = error;
    }

    fn clear_notice(&self) {
        self.say("", false);
    }

    fn say_report(&self, report: &Report, text: String) {
        self.say(text, report.failed());
    }

    /// Whether another change may start. If not, says why.
    fn idle(&self) -> bool {
        if self.state.borrow().busy {
            self.say("Wait for the work in progress to finish.", true);
            self.render();
            return false;
        }
        true
    }
}

/// Reads the place the session is at, on a thread, and shows what comes.
fn load(app: &Rc<App>, then: Then) {
    let (generation, place, hidden, cancel, trash_dir) = {
        let mut s = app.state.borrow_mut();
        // Whatever is still being read is not wanted any more.
        s.cancel.store(true, Ordering::Relaxed);
        s.cancel = Arc::new(AtomicBool::new(false));
        s.then = then;
        let generation = s.session.begin_load();
        (
            generation,
            s.session.place.clone(),
            s.session.show_hidden,
            s.cancel.clone(),
            s.session.trash.clone(),
        )
    };
    app.render();
    run_job(
        app,
        move || read_place(&place, hidden, &cancel, &trash_dir, generation),
        move |app, result| loaded(app, generation, result),
    );
}

/// Does `work`, on a thread of its own, and then `done` on the event loop
/// with what it made. Tests have no event loop and do both at once.
fn run_job<T: Send + 'static>(
    app: &Rc<App>,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&Rc<App>, T) + Send + 'static,
) {
    if app.state.borrow().sync {
        done(app, work());
    } else {
        std::thread::spawn(move || {
            let value = work();
            post(move |app| done(app, value));
        });
    }
}

type Read = Result<(Vec<Entry>, usize), String>;

fn read_place(
    place: &Place,
    hidden: bool,
    cancel: &AtomicBool,
    trash_dir: &Path,
    generation: u64,
) -> Read {
    match place {
        Place::Folder(dir) => fsops::read(dir, hidden, cancel, &mut |read| {
            post(move |app| {
                let mut s = app.state.borrow_mut();
                if s.session.generation == generation {
                    s.session.loading = Some(read);
                    drop(s);
                    app.render();
                }
            });
        })
        .map(|listing| (listing.entries, listing.skipped))
        .map_err(|error| read_error(dir, &error)),
        Place::Trash => trash::list(trash_dir)
            .map(|items| {
                let mut entries: Vec<Entry> = items
                    .into_iter()
                    .map(|item| Entry {
                        name: item.name,
                        folder: item.folder,
                        link: false,
                        size: 0,
                        modified: 0,
                        original: item.original,
                        deleted: item.date,
                    })
                    .collect();
                fsops::sort(&mut entries);
                (entries, 0)
            })
            .map_err(|error| read_error(trash_dir, &error)),
    }
}

fn loaded(app: &Rc<App>, generation: u64, result: Result<(Vec<Entry>, usize), String>) {
    {
        let mut s = app.state.borrow_mut();
        let then = s.then.clone();
        if !s.session.finish_load(generation, result, &then.select) {
            return;
        }
        s.free = match s.session.place.clone() {
            Place::Folder(dir) => fsops::free_space(&dir),
            Place::Trash => None,
        };
        if then.top {
            s.top_tick += 1;
        } else if !then.select.is_empty() {
            s.reveal_tick += 1;
        }
        if then.rename {
            s.renaming = s.session.only_selected();
        }
    }
    let renaming = app.state.borrow().renaming;
    if let Some(at) = renaming {
        let name = app.state.borrow().session.entries[at].name.clone();
        set_rename_text(app, &name);
    }
    app.render();
}

fn set_rename_text(app: &Rc<App>, name: &str) {
    if let Some(window) = app.window.upgrade() {
        window.global::<Files>().set_rename_text(name.into());
    }
}

fn navigate(app: &Rc<App>, place: Place) {
    {
        let mut s = app.state.borrow_mut();
        s.session.go(place);
        s.renaming = None;
        s.view.0 = 0;
    }
    app.clear_notice();
    load(
        app,
        Then {
            top: true,
            ..Then::default()
        },
    );
}

/// Loads the place again, keeping what is selected.
fn refresh(app: &Rc<App>) {
    let select = {
        let s = app.state.borrow();
        s.session
            .selected_entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    };
    load(
        app,
        Then {
            select,
            ..Then::default()
        },
    );
}

fn reload_selecting(app: &Rc<App>, names: Vec<String>, rename: bool) {
    load(
        app,
        Then {
            select: names,
            rename,
            top: false,
        },
    );
}

fn open_at(app: &Rc<App>, at: usize) {
    let (path, folder, in_trash, env) = {
        let s = app.state.borrow();
        let Some(entry) = s.session.entries.get(at) else {
            return;
        };
        (
            s.session.path_of(at),
            entry.folder,
            s.session.in_trash(),
            s.env.clone(),
        )
    };
    if app.state.borrow().renaming.is_some() {
        return;
    }
    if in_trash {
        app.say("Restore it from the Trash to open it.", false);
        app.render();
        return;
    }
    let Some(path) = path else { return };
    if folder {
        navigate(app, Place::Folder(path));
        return;
    }
    match open::open(&env, &path) {
        Ok(()) => {
            let name = path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            app.say(format!("Opening \"{name}\"\u{2026}"), false);
        }
        Err(error) => app.say(error, true),
    }
    app.render();
}

fn new_folder(app: &Rc<App>) {
    new_item(app, "a folder", fsops::create_folder);
}

fn new_file(app: &Rc<App>) {
    new_item(app, "a file", fsops::create_file);
}

fn new_item(app: &Rc<App>, what: &str, create: fn(&std::path::Path) -> std::io::Result<String>) {
    if !app.idle() {
        return;
    }
    let made = {
        let s = app.state.borrow();
        s.session.can_change_here().and_then(|dir| {
            create(&dir).map_err(|e| format!("Cannot make {what}: {}.", trash::plain(&e)))
        })
    };
    match made {
        Ok(name) => {
            app.clear_notice();
            reload_selecting(app, vec![name], true);
        }
        Err(error) => {
            app.say(error, true);
            app.render();
        }
    }
}

fn start_rename(app: &Rc<App>) {
    let result = {
        let s = app.state.borrow();
        if s.session.in_trash() {
            Err("Restore it from the Trash before renaming it.".to_string())
        } else {
            match s.session.only_selected() {
                None => Err("Select one item to rename.".to_string()),
                Some(at) => s.session.can_rename(at).map(|()| at),
            }
        }
    };
    match result {
        Ok(at) => {
            let name = {
                let mut s = app.state.borrow_mut();
                s.renaming = Some(at);
                s.reveal_tick += 1;
                s.session.entries[at].name.clone()
            };
            app.clear_notice();
            set_rename_text(app, &name);
        }
        Err(error) => app.say(error, true),
    }
    app.render();
}

fn rename_accepted(app: &Rc<App>, text: &str) -> bool {
    let (dir, old) = {
        let s = app.state.borrow();
        let (Some(at), Some(dir)) = (s.renaming, s.session.dir()) else {
            return true;
        };
        (dir.to_path_buf(), s.session.entries[at].name.clone())
    };
    match fsops::rename(&dir, &old, text) {
        Ok(new) => {
            app.state.borrow_mut().renaming = None;
            app.clear_notice();
            if new == old {
                app.render();
            } else {
                reload_selecting(app, vec![new], false);
            }
            true
        }
        Err(error) => {
            app.say(error.to_string(), true);
            app.render();
            false
        }
    }
}

fn trash_selected(app: &Rc<App>) {
    if !app.idle() {
        return;
    }
    let planned = {
        let s = app.state.borrow();
        if s.session.in_trash() {
            Err(
                "Items in the Trash are deleted with Empty Trash, or put back with Restore."
                    .to_string(),
            )
        } else {
            s.session
                .changeable_selection()
                .map(|paths| (paths, s.session.trash.clone()))
        }
    };
    match planned {
        Ok((paths, bin)) => {
            let report = jobs::trash_items(&bin, &paths);
            let text = report.say_with("Moved", " to the Trash");
            app.say_report(&report, text);
            reload_selecting(app, Vec::new(), false);
        }
        Err(error) => {
            app.say(error, true);
            app.render();
        }
    }
}

fn copy_or_cut(app: &Rc<App>, cut: bool) {
    let result = {
        let mut s = app.state.borrow_mut();
        if cut {
            s.session.cut()
        } else {
            s.session.copy()
        }
    };
    match result {
        Ok(count) => app.say(
            format!("{} {}.", if cut { "Cut" } else { "Copied" }, plural(count)),
            false,
        ),
        Err(error) => app.say(error, true),
    }
    app.render();
}

fn paste(app: &Rc<App>) {
    if !app.idle() {
        return;
    }
    let plan = app.state.borrow().session.paste_plan();
    let (clipboard, dest) = match plan {
        Ok(plan) => plan,
        Err(error) => {
            app.say(error, true);
            app.render();
            return;
        }
    };
    app.state.borrow_mut().busy = true;
    app.clear_notice();
    app.render();
    let cut = clipboard.cut;
    run_job(
        app,
        move || jobs::paste(&clipboard.items, cut, &dest),
        move |app, report| job_done(app, &report, if cut { "Moved" } else { "Pasted" }, cut),
    );
}

fn job_done(app: &Rc<App>, report: &Report, verb: &str, clear_clipboard: bool) {
    {
        let mut s = app.state.borrow_mut();
        s.busy = false;
        // What was cut is where it was cut to now; a failed one stays on
        // the clipboard so that it can be tried again.
        if clear_clipboard && !report.failed() {
            s.session.clipboard = None;
        }
    }
    app.say_report(report, report.say(verb));
    let select = report
        .last
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| vec![name.to_string_lossy().into_owned()])
        .unwrap_or_default();
    // Only a paste into the folder that is shown selects what it made.
    let here = {
        let s = app.state.borrow();
        report.last.as_ref().and_then(|path| path.parent()) == s.session.dir()
    };
    reload_selecting(app, if here { select } else { Vec::new() }, false);
}

fn restore(app: &Rc<App>) {
    if !app.idle() {
        return;
    }
    let planned = {
        let s = app.state.borrow();
        let names: Vec<String> = s
            .session
            .selected_entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        match (&s.session.guard, names.is_empty()) {
            (_, true) => Err("Select what to restore.".to_string()),
            (None, _) => Err(
                "Files cannot find your home folder, so it cannot restore anything.".to_string(),
            ),
            (Some(guard), false) => Ok((guard.clone(), s.session.trash.clone(), names)),
        }
    };
    match planned {
        Ok((guard, bin, names)) => {
            let report = jobs::restore_items(&guard, &bin, &names);
            let home = app.state.borrow().session.home.clone();
            let mut text = report.say("Restored");
            if let (1, Some(path)) = (report.done, &report.last) {
                if let Some(parent) = path.parent() {
                    text = format!("{} Now in {}.", text, tilde(&home, parent));
                }
            }
            app.say_report(&report, text);
            reload_selecting(app, Vec::new(), false);
        }
        Err(error) => {
            app.say(error, true);
            app.render();
        }
    }
}

fn ask_to_empty(app: &Rc<App>) {
    if !app.idle() {
        return;
    }
    let count = app.state.borrow().session.entries.len();
    if count == 0 {
        app.say("The Trash is empty.", false);
        app.render();
        return;
    }
    app.state.borrow_mut().asking = Some(Asking::EmptyTrash);
    if let Some(window) = app.window.upgrade() {
        let files = window.global::<Files>();
        files.set_dialog(DialogSpec {
            title: "Empty the Trash?".into(),
            body: format!(
                "{} will be deleted for good. This cannot be undone.",
                plural(count)
            )
            .into(),
            confirm: "Empty Trash".into(),
            cancel: "Cancel".into(),
            danger: true,
            ..DialogSpec::default()
        });
        files.set_dialog_open(true);
    }
}

fn close_dialog(app: &Rc<App>) -> Option<Asking> {
    if let Some(window) = app.window.upgrade() {
        window.global::<Files>().set_dialog_open(false);
    }
    app.state.borrow_mut().asking.take()
}

fn dialog_confirmed(app: &Rc<App>) {
    let Some(Asking::EmptyTrash) = close_dialog(app) else {
        return;
    };
    let (guard, bin) = {
        let s = app.state.borrow();
        match &s.session.guard {
            Some(guard) => (guard.clone(), s.session.trash.clone()),
            None => return,
        }
    };
    app.state.borrow_mut().busy = true;
    app.clear_notice();
    app.render();
    run_job(
        app,
        move || jobs::empty_trash(&guard, &bin),
        move |app, report| {
            app.state.borrow_mut().busy = false;
            let text = if report.failed() {
                report.say("Deleted")
            } else {
                format!(
                    "Emptied the Trash: {} deleted for good.",
                    plural(report.done)
                )
            };
            app.say_report(&report, text);
            reload_selecting(app, Vec::new(), false);
        },
    );
}

/// What the window is opened with.
pub struct Start {
    pub env: Env,
    pub place: Place,
    /// Said at the start, such as that the folder asked for is not there.
    pub notice: Option<String>,
}

pub fn run(window: FilesWindow, start: Start) -> Result<(), slint::PlatformError> {
    let _app = attach(&window, start, false);
    let result = window.run();
    APP.with(|slot| *slot.borrow_mut() = None);
    result
}

/// Connects the program to the window and starts the first load. With
/// `sync` the work is done at once on this thread, for tests that have no
/// event loop. The app lives as long as the value returned.
pub fn attach(window: &FilesWindow, start: Start, sync: bool) -> Rc<App> {
    let home = start.env.home.clone();
    let bin = start.env.data_home.join("Trash");
    let session = Session::new(home.clone(), bin, start.place);
    let app = Rc::new(App {
        window: window.as_weak(),
        state: RefCell::new(State {
            session,
            places: places(&home),
            env: start.env,
            view: (0, 40),
            notice: start.notice.clone().unwrap_or_default(),
            notice_error: start.notice.is_some(),
            renaming: None,
            busy: false,
            free: None,
            cancel: Arc::new(AtomicBool::new(false)),
            then: Then::default(),
            asking: None,
            top_tick: 0,
            reveal_tick: 0,
            sync,
        }),
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    wire(window, &app);
    load(&app, Then::default());
    app
}

impl App {
    /// What the window says at the bottom, for tests.
    pub fn status_text(&self) -> String {
        let s = self.state.borrow();
        if s.notice.is_empty() {
            s.status()
        } else {
            s.notice.clone()
        }
    }

    /// Names in the folder shown, in order.
    pub fn names(&self) -> Vec<String> {
        let s = self.state.borrow();
        s.session.entries.iter().map(|e| e.name.clone()).collect()
    }

    /// Names that are selected.
    pub fn selected_names(&self) -> Vec<String> {
        let s = self.state.borrow();
        s.session
            .selected_entries()
            .iter()
            .map(|e| e.name.clone())
            .collect()
    }

    pub fn place(&self) -> Place {
        self.state.borrow().session.place.clone()
    }

    /// Selects only the entry called `name`, as a click on it would.
    pub fn select_named(&self, name: &str) {
        let found = self.state.borrow_mut().session.select_name(name);
        if found.is_some() {
            self.render();
        }
    }
}

fn wire(window: &FilesWindow, app: &Rc<App>) {
    let files = window.global::<Files>();
    // Each callback holds the app weakly: the app holds the window.
    macro_rules! on {
        ($setter:ident, |$app:ident $(, $arg:ident)*| $body:expr) => {{
            let weak = Rc::downgrade(app);
            files.$setter(move |$($arg),*| {
                if let Some($app) = weak.upgrade() {
                    $body
                }
            });
        }};
    }
    on!(on_viewport, |app, first, count| {
        let view = (first.max(0) as usize, count.max(1) as usize);
        if app.state.borrow().view != view {
            app.state.borrow_mut().view = view;
            app.render();
        }
    });
    on!(on_click, |app, at, ctrl, shift| {
        if app.state.borrow().renaming.is_none() {
            app.state
                .borrow_mut()
                .session
                .click(at.max(0) as usize, ctrl, shift);
            app.clear_notice();
            app.render();
        }
    });
    on!(on_click_empty, |app| {
        if app.state.borrow().renaming.is_none() {
            app.state.borrow_mut().session.select_none();
            app.clear_notice();
            app.render();
        }
    });
    on!(on_open, |app, at| open_at(&app, at.max(0) as usize));
    on!(on_open_selected, |app| {
        let at = {
            let s = app.state.borrow();
            s.session.only_selected()
        };
        match at {
            Some(at) => open_at(&app, at),
            None => {
                app.say("Select one item to open.", false);
                app.render();
            }
        }
    });
    on!(on_step, |app, by, extend| {
        app.state.borrow_mut().session.step(by, extend);
        app.clear_notice();
        app.render();
    });
    on!(on_go_place, |app, at| {
        let place = app
            .state
            .borrow()
            .places
            .get(at.max(0) as usize)
            .map(|p| p.1.clone());
        if let Some(place) = place {
            navigate(&app, place);
        }
    });
    on!(on_go_crumb, |app, at| {
        let place = app
            .state
            .borrow()
            .session
            .crumbs()
            .get(at.max(0) as usize)
            .map(|c| c.place.clone());
        if let Some(place) = place {
            navigate(&app, place);
        }
    });
    on!(on_back, |app| {
        let went = app.state.borrow_mut().session.go_back();
        if went {
            app.state.borrow_mut().renaming = None;
            app.clear_notice();
            load(
                &app,
                Then {
                    top: true,
                    ..Then::default()
                },
            );
        }
    });
    on!(on_up, |app| {
        let place = app.state.borrow().session.parent_place();
        if let Some(place) = place {
            navigate(&app, place);
        }
    });
    on!(on_refresh, |app| refresh(&app));
    on!(on_new_folder, |app| new_folder(&app));
    on!(on_new_file, |app| new_file(&app));
    on!(on_toggle_hidden, |app, on| {
        app.state.borrow_mut().session.show_hidden = on;
        refresh(&app);
    });
    on!(on_start_rename, |app| start_rename(&app));
    on!(on_rename_cancelled, |app| {
        app.state.borrow_mut().renaming = None;
        app.clear_notice();
        app.render();
    });
    on!(on_trash_selected, |app| trash_selected(&app));
    on!(on_copy, |app| copy_or_cut(&app, false));
    on!(on_cut, |app| copy_or_cut(&app, true));
    on!(on_paste, |app| paste(&app));
    on!(on_select_all, |app| {
        app.state.borrow_mut().session.select_all();
        app.render();
    });
    on!(on_restore, |app| restore(&app));
    on!(on_empty_trash, |app| ask_to_empty(&app));
    on!(on_dialog_confirmed, |app| dialog_confirmed(&app));
    on!(on_dialog_cancelled, |app| {
        close_dialog(&app);
    });

    // The one callback that answers.
    let weak = Rc::downgrade(app);
    files.on_rename_accepted(move |text| match weak.upgrade() {
        Some(app) => rename_accepted(&app, &text),
        None => true,
    });

    // A paste that is half done is not left half done by closing the window.
    let weak = Rc::downgrade(app);
    window.window().on_close_requested(move || {
        if let Some(app) = weak.upgrade() {
            if app.state.borrow().busy {
                app.say(
                    "Files is still working. Close the window when it is done.",
                    true,
                );
                app.render();
                return slint::CloseRequestResponse::KeepWindowShown;
            }
        }
        slint::CloseRequestResponse::HideWindow
    });
}

/// Where a path given on the command line puts the window.
pub fn start_for(env: &Env, arg: Option<&str>, cwd: &Path) -> Start {
    let home = Place::Folder(env.home.clone());
    let (place, notice) = match arg {
        None => (home, None),
        Some(arg) => match open::start_place(arg, cwd) {
            Ok(dir) => (Place::Folder(dir), None),
            Err(notice) => (home, Some(notice)),
        },
    };
    Start {
        env: env.clone(),
        place,
        notice,
    }
}
