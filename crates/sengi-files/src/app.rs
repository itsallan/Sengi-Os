//! Files' window and what it does. The window is touched only from the
//! event loop. A folder is read, and a paste or the emptying of the Trash
//! done, on a thread of its own that posts what it found back here, so a
//! folder of ten thousand files never holds the window up.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use sengi_apps::Env;
use sengi_system::trash;

use crate::devices::{self, Device, Sources};
use crate::fsops::{self, Entry};
use crate::jobs::{self, Binned, Progress, Report};
use crate::menu;
use crate::open;
use crate::session::{place_icon, places, Place, Session};
use crate::ui::{Crumb, DeviceRow, DialogSpec, FileRow, Files, FilesWindow, MenuEntry, PlaceRow};
use crate::watch::Watcher;
use crate::{hotplug, udisks};

/// What to do when a load is done.
#[derive(Default, Clone)]
struct Then {
    /// Names to select.
    select: Vec<String>,
    /// Start renaming the one selected.
    rename: bool,
    /// Scroll to the top: it is a new place.
    top: bool,
    /// Leave the scroll position alone: the folder changed under the window.
    keep_view: bool,
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
    /// What the field of that rename holds.
    typed: String,
    busy: bool,
    free: Option<u64>,
    cancel: Arc<AtomicBool>,
    then: Then,
    asking: Option<Asking>,
    top_tick: i32,
    reveal_tick: i32,
    /// Tests have no event loop: work is done at once, not on a thread.
    sync: bool,
    /// The rows the list shows, changed in place so that a row held by the
    /// pointer is not made again under it.
    rows: Rc<VecModel<FileRow>>,
    watcher: Option<Watcher>,
    /// The folder changed while a name was being typed.
    stale: bool,
    /// Where the drives are looked for, and the ones found.
    sources: Sources,
    devices: Vec<Device>,
    /// How far a copy or move that is running has got, and what to call it.
    progress: Option<Arc<Progress>>,
    doing: &'static str,
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
        Some(short) => short.replacen('T', " ", 1),
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
            return match &self.progress {
                Some(progress) => progress.line(self.doing),
                None => "Working\u{2026}".to_string(),
            };
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

    /// The drive the folder shown is on, if it is on one.
    fn device_here(&self) -> Option<usize> {
        let dir = self.session.dir()?;
        self.devices.iter().position(|device| {
            device
                .mount
                .as_deref()
                .is_some_and(|point| dir.starts_with(point))
        })
    }

    /// The top folders of the drives that are open.
    fn volumes(&self) -> Vec<PathBuf> {
        devices::mounted_in(&self.devices, self.session.media.as_deref())
    }

    /// What a drag carries: the name, or how many.
    fn drag_label(&self) -> String {
        match self.session.selected.len() {
            0 => String::new(),
            1 => self
                .session
                .selected_entries()
                .first()
                .map_or_else(String::new, |entry| entry.name.clone()),
            count => plural(count),
        }
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
        if s.rows.row_count() == rows.len() {
            for (at, row) in rows.into_iter().enumerate() {
                if s.rows.row_data(at).as_ref() != Some(&row) {
                    s.rows.set_row_data(at, row);
                }
            }
        } else {
            s.rows.set_vec(rows);
        }
        files.set_drag_label(s.drag_label().into());
        files.set_first(first as i32);
        files.set_total(total as i32);
        files.set_cursor(s.session.cursor as i32);
        files.set_places(ModelRc::new(VecModel::from(
            s.places
                .iter()
                .map(|(name, _)| PlaceRow {
                    name: name.as_str().into(),
                    icon: place_icon(name).into(),
                })
                .collect::<Vec<_>>(),
        )));
        files.set_devices(ModelRc::new(VecModel::from(
            s.devices
                .iter()
                .map(|device| DeviceRow {
                    name: device.title().into(),
                    detail: device.detail().into(),
                    mounted: device.is_mounted(),
                })
                .collect::<Vec<_>>(),
        )));
        files.set_device(s.device_here().map_or(-1, |at| at as i32));
        files.set_cancelable(s.busy && s.progress.is_some());
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
        files.set_can_forward(s.session.can_forward());
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
    let (generation, place, hidden, cancel, trash_dir, volumes) = {
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
            s.volumes(),
        )
    };
    set_watch(app);
    app.render();
    run_job(
        app,
        move || read_place(&place, hidden, &cancel, &trash_dir, &volumes, generation),
        move |app, result| loaded(app, generation, result),
    );
}

/// Points the watcher at the place being shown.
fn set_watch(app: &Rc<App>) {
    let mut s = app.state.borrow_mut();
    s.stale = false;
    let dir = match &s.session.place {
        Place::Folder(dir) => Some(dir.clone()),
        Place::Trash => Some(s.session.trash.join("files")),
    };
    if let Some(watcher) = &s.watcher {
        // A folder that is not there, such as a Trash never used, is not
        // watched.
        let _ = watcher.watch(dir.as_deref());
    }
}

/// The folder shown changed under the window, by another program or by
/// this one: reads it again, keeping the selection and the scroll.
pub fn folder_changed(app: &Rc<App>) {
    {
        let mut s = app.state.borrow_mut();
        // A paste or a trashing reads the folder itself when it is done.
        if s.busy {
            return;
        }
        // The name being typed would be lost: wait for it.
        if s.renaming.is_some() {
            s.stale = true;
            return;
        }
    }
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
            keep_view: true,
            ..Then::default()
        },
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
    volumes: &[PathBuf],
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
        Place::Trash => read_trash(trash_dir, volumes),
    }
}

/// The user's own Trash and the Trash of each drive that is open, as one list.
fn read_trash(trash_dir: &Path, volumes: &[PathBuf]) -> Read {
    let entry = |item: trash::Item, volume: Option<PathBuf>| Entry {
        name: item.name,
        folder: item.folder,
        link: false,
        size: 0,
        modified: 0,
        original: item.original,
        deleted: item.date,
        volume,
    };
    let mut entries: Vec<Entry> = trash::list(trash_dir)
        .map_err(|error| read_error(trash_dir, &error))?
        .into_iter()
        .map(|item| entry(item, None))
        .collect();
    for volume in volumes {
        // A drive that cannot be read, or has no Trash, adds nothing.
        if let Ok(items) = jobs::list_volume_trash(volume) {
            entries.extend(
                items
                    .into_iter()
                    .map(|item| entry(item, Some(volume.clone()))),
            );
        }
    }
    fsops::sort(&mut entries);
    Ok((entries, 0))
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
        } else if !then.select.is_empty() && !then.keep_view {
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
    app.state.borrow_mut().typed = name.to_string();
    if let Some(window) = app.window.upgrade() {
        window.global::<Files>().set_rename_text(name.into());
    }
}

fn navigate(app: &Rc<App>, place: Place) {
    // The name being typed belongs to the folder that is being left.
    let refused = settle_rename(app).and_then(|ended| ended.error);
    {
        let mut s = app.state.borrow_mut();
        s.session.go(place);
        s.view.0 = 0;
    }
    match refused {
        Some(error) => app.say(error, true),
        None => app.clear_notice(),
    }
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
            keep_view: false,
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
    // A name still being typed is kept first. What is read and selected next
    // is the new item.
    settle_rename(app);
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

/// A rename that something else brought to an end.
struct Ended {
    /// The folder should be read again: the order may have changed.
    reread: bool,
    error: Option<String>,
}

/// A click somewhere else ends a rename and keeps what was typed; a name that
/// cannot be taken leaves the old one. The entry is changed where it stands,
/// so that the row numbers the click came with still hold.
fn settle_rename(app: &Rc<App>) -> Option<Ended> {
    let mut s = app.state.borrow_mut();
    let at = s.renaming.take()?;
    let mut ended = Ended {
        reread: s.stale,
        error: None,
    };
    let dir = s.session.dir()?.to_path_buf();
    let old = s.session.entries.get(at)?.name.clone();
    match fsops::rename(&dir, &old, &s.typed) {
        Ok(new) => {
            ended.reread |= new != old;
            s.session.entries[at].name = new;
        }
        Err(error) => ended.error = Some(error.to_string()),
    }
    Some(ended)
}

/// Ends any rename, does what was asked for, and then reads the folder again
/// and says what was wrong with the name.
fn after_rename(app: &Rc<App>, action: impl FnOnce()) {
    let ended = settle_rename(app);
    action();
    let Some(ended) = ended else { return };
    if let Some(error) = ended.error {
        app.say(error, true);
    }
    if ended.reread {
        folder_changed(app);
    } else {
        app.render();
    }
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
            s.session.changeable_selection().and_then(|paths| {
                let guard = s
                    .session
                    .guard_ref()
                    .cloned()
                    .ok_or("Nothing can be changed.")?;
                Ok((paths, s.session.trash.clone(), guard))
            })
        }
    };
    match planned {
        Ok((paths, bin, guard)) => {
            let report = jobs::trash_items(&guard, &bin, &paths);
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
    let cut = clipboard.cut;
    start_paste(
        app,
        clipboard.items,
        cut,
        dest,
        if cut { "Moved" } else { "Pasted" },
        cut,
    );
}

/// Copies or moves `items` into `dest` on a thread of its own, with a line
/// at the bottom that says how far it has got and a button that stops it.
fn start_paste(
    app: &Rc<App>,
    items: Vec<PathBuf>,
    cut: bool,
    dest: PathBuf,
    verb: &'static str,
    clear_clipboard: bool,
) {
    let progress = Arc::new(Progress::default());
    {
        let mut s = app.state.borrow_mut();
        s.busy = true;
        s.progress = Some(progress.clone());
        s.doing = if cut { "Moving" } else { "Copying" };
    }
    app.clear_notice();
    app.render();
    run_job(
        app,
        move || {
            // The line at the bottom is drawn at most a few times a second.
            let last: Cell<Option<Instant>> = Cell::new(None);
            let tick = || {
                let due = last
                    .get()
                    .is_none_or(|at| at.elapsed() >= Duration::from_millis(120));
                if due {
                    last.set(Some(Instant::now()));
                    post(|app| app.render());
                }
            };
            jobs::paste_with(&items, cut, &dest, &progress, &tick)
        },
        move |app, report| job_done(app, &report, verb, clear_clipboard),
    );
}

fn job_done(app: &Rc<App>, report: &Report, verb: &str, clear_clipboard: bool) {
    {
        let mut s = app.state.borrow_mut();
        s.busy = false;
        s.progress = None;
        // What was cut is where it was cut to now; a failed or stopped one
        // stays on the clipboard so that it can be tried again.
        if clear_clipboard && !report.failed() && !report.cancelled {
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
        let names: Vec<Binned> = s
            .session
            .selected_entries()
            .iter()
            .map(|entry| Binned {
                name: entry.name.clone(),
                volume: entry.volume.clone(),
            })
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
    let (guard, bin, volumes) = {
        let s = app.state.borrow();
        match &s.session.guard {
            Some(guard) => (guard.clone(), s.session.trash.clone(), s.volumes()),
            None => return,
        }
    };
    app.state.borrow_mut().busy = true;
    app.clear_notice();
    app.render();
    run_job(
        app,
        move || jobs::empty_trash(&guard, &bin, &volumes),
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

/// One step along the history: back, or forward again.
fn walk(app: &Rc<App>, back: bool) {
    let refused = settle_rename(app).and_then(|ended| ended.error);
    let went = {
        let mut s = app.state.borrow_mut();
        if back {
            s.session.go_back()
        } else {
            s.session.go_forward()
        }
    };
    if went {
        match refused {
            Some(error) => app.say(error, true),
            None => app.clear_notice(),
        }
        load(
            app,
            Then {
                top: true,
                ..Then::default()
            },
        );
    }
}

/// A right-click on row `at`, or on the empty space with -1: the item
/// clicked is selected unless it is part of the selection already, and the
/// menu is made from what that leaves.
fn context_menu(app: &Rc<App>, at: i32) {
    // The row whose name is being typed has no menu of its own.
    let renaming = app.state.borrow().renaming;
    if renaming.is_some_and(|row| row as i32 == at) {
        return;
    }
    after_rename(app, || show_menu(app, at));
}

fn show_menu(app: &Rc<App>, at: i32) {
    let rows = {
        let mut s = app.state.borrow_mut();
        if at >= 0 {
            let at = at as usize;
            if !s.session.selected.contains(&at) {
                s.session.click(at, false, false);
            }
        } else {
            s.session.select_none();
        }
        menu::rows(&s.session, at >= 0)
    };
    app.clear_notice();
    app.render();
    if let Some(window) = app.window.upgrade() {
        let files = window.global::<Files>();
        files.set_menu(ModelRc::new(VecModel::from(
            rows.into_iter()
                .map(|row| MenuEntry {
                    id: row.id.into(),
                    label: row.label.into(),
                    enabled: row.enabled,
                    separator: row.separator,
                    checked: row.checked,
                })
                .collect::<Vec<_>>(),
        )));
        files.set_menu_open(true);
    }
}

/// A row of the menu was chosen: does what the same key would.
fn menu_chosen(app: &Rc<App>, id: &str) {
    if let Some(window) = app.window.upgrade() {
        window.global::<Files>().set_menu_open(false);
    }
    match id {
        "open" => {
            let at = app.state.borrow().session.only_selected();
            if let Some(at) = at {
                open_at(app, at);
            }
        }
        "rename" => start_rename(app),
        "copy" => copy_or_cut(app, false),
        "cut" => copy_or_cut(app, true),
        "trash" => trash_selected(app),
        "restore" => restore(app),
        "empty-trash" => ask_to_empty(app),
        "new-folder" => new_folder(app),
        "new-file" => new_file(app),
        "paste" => paste(app),
        "hidden" => {
            let on = !app.state.borrow().session.show_hidden;
            app.state.borrow_mut().session.show_hidden = on;
            refresh(app);
        }
        _ => {}
    }
}

/// The selection was dropped on a folder row (`kind` 0, `target` its index)
/// or on a place (`kind` 1). It moves there, by the rules of a cut and paste;
/// on the Trash it is trashed.
fn drop_items(app: &Rc<App>, kind: i32, target: i32) {
    if !app.idle() || app.state.borrow().renaming.is_some() {
        return;
    }
    let target = target.max(0) as usize;
    let dest = {
        let s = app.state.borrow();
        if kind == 0 {
            match s.session.entries.get(target) {
                Some(entry) if entry.folder && !s.session.selected.contains(&target) => {
                    s.session.path_of(target).map(Place::Folder)
                }
                _ => None,
            }
        } else if kind == 1 {
            s.places.get(target).map(|place| place.1.clone())
        } else {
            // A drive in the sidebar: it has to be open to be dropped on.
            match s.devices.get(target) {
                Some(device) => match &device.mount {
                    Some(point) => Some(Place::Folder(point.clone())),
                    None => {
                        let text = format!("Open \"{}\" first.", device.title());
                        drop(s);
                        app.say(text, true);
                        app.render();
                        return;
                    }
                },
                None => None,
            }
        }
    };
    let Some(dest) = dest else { return };
    let Place::Folder(dest) = dest else {
        // The Trash: the same as the Delete key.
        trash_selected(app);
        return;
    };
    let plan = app.state.borrow().session.drop_plan(&dest);
    match plan {
        Ok(items) => start_paste(app, items, true, dest, "Moved", false),
        Err(error) => {
            app.say(error, true);
            app.render();
        }
    }
}

/// A drive in the sidebar was clicked: an open one is shown, one that is
/// not is opened first, by `udisksctl`, on a thread of its own.
fn open_device(app: &Rc<App>, at: usize) {
    settle_rename(app);
    if !app.idle() {
        return;
    }
    let (device, env, sources) = {
        let s = app.state.borrow();
        let Some(device) = s.devices.get(at) else {
            return;
        };
        (device.clone(), s.env.clone(), s.sources.clone())
    };
    if let Some(point) = &device.mount {
        navigate(app, Place::Folder(point.clone()));
        return;
    }
    {
        let mut s = app.state.borrow_mut();
        s.busy = true;
        s.progress = None;
    }
    app.say(format!("Opening \"{}\"\u{2026}", device.title()), false);
    app.render();
    let name = device.name.clone();
    run_job(
        app,
        move || {
            let result = udisks::mount(&env, &device);
            (result, devices::discover(&sources))
        },
        move |app, (result, found)| device_opened(app, &name, result, found),
    );
}

fn device_opened(app: &Rc<App>, name: &str, result: Result<(), String>, found: Vec<Device>) {
    let point = found
        .iter()
        .find(|device| device.name == name)
        .and_then(|device| device.mount.clone());
    {
        let mut s = app.state.borrow_mut();
        s.busy = false;
        s.devices = found;
    }
    match (result, point) {
        (Ok(()), Some(point)) => navigate(app, Place::Folder(point)),
        (Ok(()), None) => {
            app.say("The drive was opened but cannot be found.", true);
            app.render();
        }
        (Err(error), _) => {
            app.say(error, true);
            app.render();
        }
    }
}

/// The Eject button of a drive: Files leaves it, every volume on its disk is
/// unmounted and the disk is powered off, and the answer is said at the
/// bottom, naming what is using the drive when it is busy.
fn eject_device(app: &Rc<App>, at: usize) {
    settle_rename(app);
    if !app.idle() {
        return;
    }
    let (device, all, env, sources) = {
        let s = app.state.borrow();
        let Some(device) = s.devices.get(at) else {
            return;
        };
        (
            device.clone(),
            s.devices.clone(),
            s.env.clone(),
            s.sources.clone(),
        )
    };
    // A window that is looking into the drive lets go of it first.
    let inside = {
        let s = app.state.borrow();
        s.session.dir().is_some_and(|dir| {
            all.iter()
                .filter(|other| other.disk == device.disk)
                .filter_map(|other| other.mount.as_deref())
                .any(|point| dir.starts_with(point))
        })
    };
    if inside {
        let home = app.state.borrow().session.home.clone();
        navigate(app, Place::Folder(home));
    }
    {
        let mut s = app.state.borrow_mut();
        s.busy = true;
        s.progress = None;
    }
    app.say(format!("Ejecting \"{}\"\u{2026}", device.title()), false);
    app.render();
    run_job(
        app,
        move || {
            let result = udisks::eject(&env, &all, &device);
            (result, devices::discover(&sources))
        },
        |app, (result, found)| {
            {
                let mut s = app.state.borrow_mut();
                s.busy = false;
                s.devices = found;
            }
            match result {
                Ok(text) => app.say(text, false),
                Err(error) => app.say(error, true),
            }
            reload_selecting(app, Vec::new(), false);
        },
    );
}

/// The kernel said a drive came or went, or was opened or closed.
pub fn devices_changed(app: &Rc<App>, found: Vec<Device>) {
    let (changed, gone, in_trash) = {
        let mut s = app.state.borrow_mut();
        let changed = s.devices != found;
        s.devices = found;
        // The folder shown was on a drive that is no longer open.
        let gone = match (s.session.dir(), s.session.media.as_deref()) {
            (Some(dir), Some(media)) => dir
                .strip_prefix(media)
                .ok()
                .and_then(|rest| rest.components().next())
                .map(|first| media.join(first.as_os_str()))
                .filter(|top| !s.devices.iter().any(|d| d.mount.as_deref() == Some(top))),
            _ => None,
        };
        (changed, gone, s.session.in_trash())
    };
    if let Some(top) = gone {
        let name = top
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let home = app.state.borrow().session.home.clone();
        if !app.state.borrow().busy {
            navigate(app, Place::Folder(home));
            app.say(format!("\"{name}\" is not there any more."), true);
        }
    } else if changed && in_trash && !app.state.borrow().busy {
        refresh(app);
    }
    app.render();
}

/// What the window is opened with.
pub struct Start {
    pub env: Env,
    pub place: Place,
    /// Said at the start, such as that the folder asked for is not there.
    pub notice: Option<String>,
    /// `/run/media/USER`, where the drives are opened.
    pub media: Option<PathBuf>,
    /// Where the drives are looked for.
    pub sources: Sources,
}

/// Where the user's drives are mounted: `/run/media/USER`.
pub fn media_root(user: Option<std::ffi::OsString>, home: &Path) -> Option<PathBuf> {
    let user = user
        .filter(|user| !user.is_empty())
        .or_else(|| home.file_name().map(|name| name.to_os_string()))?;
    Some(PathBuf::from("/run/media").join(user))
}

pub fn run(window: FilesWindow, start: Start) -> Result<(), slint::PlatformError> {
    let app = attach(&window, start, false);
    match Watcher::spawn(|| post(folder_changed)) {
        Ok(watcher) => {
            app.state.borrow_mut().watcher = Some(watcher);
            set_watch(&app);
        }
        Err(error) => eprintln!("sengi-files: not watching folders: {error}"),
    }
    // Drives coming and going: one more thread that sleeps until the kernel
    // says so.
    let sources = app.state.borrow().sources.clone();
    let hotplug = hotplug::spawn(move || {
        let found = devices::discover(&sources);
        post(move |app| devices_changed(app, found));
    });
    if let Err(error) = hotplug {
        eprintln!("sengi-files: not watching for drives: {error}");
    }
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
    let session = Session::new(home.clone(), bin, start.place).with_media(start.media);
    let found = devices::discover(&start.sources);
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
            typed: String::new(),
            busy: false,
            free: None,
            cancel: Arc::new(AtomicBool::new(false)),
            then: Then::default(),
            asking: None,
            top_tick: 0,
            reveal_tick: 0,
            sync,
            rows: Rc::new(VecModel::default()),
            watcher: None,
            stale: false,
            sources: start.sources,
            devices: found,
            progress: None,
            doing: "Copying",
        }),
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    let rows = app.state.borrow().rows.clone();
    window.global::<Files>().set_rows(ModelRc::from(rows));
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
        let at = at.max(0) as usize;
        if app.state.borrow().renaming != Some(at) {
            after_rename(&app, || {
                app.state.borrow_mut().session.click(at, ctrl, shift);
                app.clear_notice();
                app.render();
            });
        }
    });
    on!(on_click_empty, |app| after_rename(&app, || {
        app.state.borrow_mut().session.select_none();
        app.clear_notice();
        app.render();
    }));
    on!(on_rename_edited, |app, text| {
        app.state.borrow_mut().typed = text.to_string();
        // A field made again, after scrolling away and back, starts with it.
        if let Some(window) = app.window.upgrade() {
            window.global::<Files>().set_rename_text(text);
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
    on!(on_back, |app| walk(&app, true));
    on!(on_forward, |app| walk(&app, false));
    on!(on_context, |app, at| context_menu(&app, at));
    on!(on_menu_chosen, |app, id| menu_chosen(&app, &id));
    on!(on_drop_items, |app, kind, target| drop_items(
        &app, kind, target
    ));
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
        // The rows move, so a name being typed is kept first.
        after_rename(&app, || {
            app.state.borrow_mut().session.show_hidden = on;
            refresh(&app);
        });
    });
    on!(on_start_rename, |app| start_rename(&app));
    on!(on_rename_cancelled, |app| {
        let stale = {
            let mut s = app.state.borrow_mut();
            s.renaming = None;
            s.stale
        };
        app.clear_notice();
        if stale {
            folder_changed(&app);
        } else {
            app.render();
        }
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
    on!(on_go_device, |app, at| open_device(
        &app,
        at.max(0) as usize
    ));
    on!(on_eject_device, |app, at| eject_device(
        &app,
        at.max(0) as usize
    ));
    on!(on_cancel_job, |app| {
        if let Some(progress) = &app.state.borrow().progress {
            progress.cancel.store(true, Ordering::Relaxed);
        }
    });
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
    let user = std::env::var_os("USER").or_else(|| std::env::var_os("LOGNAME"));
    Start {
        env: env.clone(),
        place,
        notice,
        media: media_root(user, &env.home),
        sources: Sources::system(),
    }
}
