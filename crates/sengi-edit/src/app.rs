//! Connects the editor window to the file: loading, the unsaved mark,
//! saving, finding, the file dialog, and asking before anything is lost.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use slint::platform::WindowEvent;
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, SharedString, Timer, VecModel};

use crate::browse;
use crate::doc::{self, Format, Refusal, TAB_WIDTH};
use crate::find;
use crate::ui::{Browse, Edit, EditWindow, FileEntry, Question};

/// The longest selection that is taken for the words to find.
const FIND_FROM_SELECTION: usize = 100;

/// What is said of a file that is too large to change.
const LARGE: &str = "A large file: it can be read and searched, but not changed.";

/// The name offered for a page that has none.
const NEW_NAME: &str = "Untitled.txt";

/// What the program was started with.
pub struct Start {
    pub home: PathBuf,
    pub path: Option<PathBuf>,
}

/// What is waiting for the page to be saved or given up.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Then {
    Close,
    New,
    Open,
}

/// The question on screen.
#[derive(Clone)]
enum Asking {
    /// There are changes that were not saved.
    Unsaved(Then),
    /// The name chosen to save under belongs to another file.
    Replace(PathBuf),
}

#[derive(Clone, Copy)]
enum Mode {
    Open,
    SaveAs(Option<Then>),
}

/// The file dialog while it is up.
struct Browser {
    mode: Mode,
    folder: PathBuf,
    entries: Vec<browse::Entry>,
    /// The name being saved under, kept while folders are walked through.
    proposed: String,
}

struct State {
    home: PathBuf,
    path: Option<PathBuf>,
    /// The text as it is on disk, to tell whether anything changed.
    saved: SharedString,
    format: Format,
    /// The file was not opened, so there is nothing to edit or save.
    refused: bool,
    /// Too large to be changed: shown, searched and copied from.
    read_only: bool,
    asking: Option<Asking>,
    browser: Option<Browser>,
}

pub struct App {
    window: slint::Weak<EditWindow>,
    state: RefCell<State>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

pub fn run(window: EditWindow, start: Start) -> Result<(), slint::PlatformError> {
    let _app = attach(&window, start);
    let result = window.run();
    APP.with(|slot| *slot.borrow_mut() = None);
    result
}

/// Connects the program to the window and loads the file. The app lives as
/// long as the value returned.
pub fn attach(window: &EditWindow, start: Start) -> Rc<App> {
    let app = Rc::new(App {
        window: window.as_weak(),
        state: RefCell::new(State {
            home: start.home,
            path: None,
            saved: SharedString::new(),
            format: Format::PLAIN,
            refused: false,
            read_only: false,
            asking: None,
            browser: None,
        }),
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    wire(window, &app);
    app.open(start.path.as_deref());
    app
}

fn name_of(path: Option<&Path>) -> String {
    path.and_then(Path::file_name).map_or_else(
        || "Untitled".to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// "1 character", "12,345 characters".
fn characters(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    let noun = if count == 1 {
        "character"
    } else {
        "characters"
    };
    format!("{grouped} {noun}")
}

impl App {
    fn with_edit<T>(&self, f: impl FnOnce(Edit<'_>) -> T) -> Option<T> {
        self.window
            .upgrade()
            .map(|window| f(window.global::<Edit>()))
    }

    /// Shows `path`, or an empty page.
    fn open(&self, path: Option<&Path>) {
        let name = name_of(path);
        let mut notice = String::new();
        let mut text = String::new();
        let mut refusal = String::new();
        if let Some(path) = path {
            match doc::load(path) {
                Ok(loaded) => text = loaded,
                Err(Refusal::Unreadable(_)) if !path.exists() => {
                    let parent_ok = path
                        .parent()
                        .is_none_or(|p| p.as_os_str().is_empty() || p.is_dir());
                    if parent_ok {
                        notice = format!("New file. \"{name}\" is made when you save.");
                    } else {
                        refusal = format!(
                            "The folder for \"{name}\" does not exist, so the file cannot be made."
                        );
                    }
                }
                Err(why) => refusal = why.say(&name),
            }
        }
        let read_only = text.len() as u64 > doc::EDIT_BYTES;
        if read_only {
            notice = LARGE.to_string();
        }
        let (text, format) = doc::normalise(text);
        let text = SharedString::from(text);
        {
            let mut s = self.state.borrow_mut();
            s.path = path.map(Path::to_path_buf);
            s.saved = text.clone();
            s.format = format;
            s.refused = !refusal.is_empty();
            s.read_only = read_only;
        }
        self.show_text(text);
        self.with_edit(|edit| {
            edit.set_cursor(0);
            edit.set_anchor(0);
            edit.set_refusal(refusal.into());
            edit.set_read_only(read_only);
            edit.set_notice(notice.into());
            edit.set_notice_is_error(false);
            edit.set_find_open(false);
            edit.set_find_result("".into());
            edit.set_reset_tick(edit.get_reset_tick() + 1);
        });
        self.refresh();
    }

    /// Puts another document in the text area. The area forgets what could
    /// be undone only when the text it is given differs from the text it has,
    /// so a document with the very same words goes in by way of another.
    fn show_text(&self, text: SharedString) {
        self.with_edit(|edit| {
            if edit.get_text() != text {
                edit.set_text(text);
                return;
            }
            edit.set_text(format!("{text}\n").into());
            let window = self.window.clone();
            Timer::single_shot(Duration::from_millis(1), move || {
                if let Some(window) = window.upgrade() {
                    window.global::<Edit>().set_text(text);
                }
            });
        });
    }

    /// The title, the unsaved mark and the line at the bottom.
    fn refresh(&self) {
        let Some((text, cursor, anchor)) = self.with_edit(|e| {
            (
                e.get_text(),
                e.get_cursor().max(0) as usize,
                e.get_anchor().max(0) as usize,
            )
        }) else {
            return;
        };
        let (name, dirty, untitled, format) = {
            let s = self.state.borrow();
            (
                name_of(s.path.as_deref()),
                !s.refused && text != s.saved,
                s.path.is_none(),
                s.format,
            )
        };
        let (line, column) = find::line_and_column(&text, cursor);
        let selected = find::characters_between(&text, cursor, anchor);
        let mut position = format!("Line {line}, Column {column}");
        if selected > 0 {
            position.push_str(&format!(" ({selected} selected)"));
        }
        let lines = text.bytes().filter(|b| *b == b'\n').count() + 1;
        self.with_edit(|edit| {
            edit.set_name(name.as_str().into());
            edit.set_untitled(untitled);
            edit.set_dirty(dirty);
            edit.set_title(
                format!(
                    "{}{name} \u{2014} Text Editor",
                    if dirty { "* " } else { "" }
                )
                .into(),
            );
            edit.set_position(position.into());
            edit.set_line(line as i32);
            edit.set_lines(lines as i32);
            edit.set_size(characters(text.chars().count()).into());
            let indent = if format.tabs { "Tabs, " } else { "" };
            edit.set_line_ending(format!("{indent}{}", format.ending.name()).into());
        });
    }

    fn say(&self, text: &str, error: bool) {
        self.with_edit(|edit| {
            edit.set_notice(text.into());
            edit.set_notice_is_error(error);
        });
    }

    /// The text in the window, for tests and for saving.
    pub fn text(&self) -> Option<String> {
        self.with_edit(|edit| edit.get_text().to_string())
    }

    /// What the line at the bottom says, for tests: what just happened, or
    /// else where the caret is.
    pub fn status_text(&self) -> String {
        self.with_edit(|edit| {
            let notice = edit.get_notice();
            if notice.is_empty() {
                edit.get_position().to_string()
            } else {
                notice.to_string()
            }
        })
        .unwrap_or_default()
    }

    pub fn is_dirty(&self) -> bool {
        self.with_edit(|edit| edit.get_dirty()).unwrap_or(false)
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.state.borrow().path.clone()
    }

    fn edited(&self) {
        self.say("", false);
        self.with_edit(|edit| edit.set_find_result("".into()));
        self.refresh();
    }

    /// Puts the caret after what a redo brought back.
    fn redone(&self, before: &str) {
        self.with_edit(|edit| {
            let after = edit.get_text();
            if after != before {
                let caret = find::end_of_change(before, &after) as i32;
                edit.set_select_from(caret);
                edit.set_select_to(caret);
                edit.set_select_tick(edit.get_select_tick() + 1);
            }
        });
    }

    /// Writes the page to `path`, and says how it went.
    fn write(&self, path: &Path) -> Result<(), String> {
        let Some(text) = self.with_edit(|e| e.get_text()) else {
            return Err("the window is gone".to_string());
        };
        let format = self.state.borrow().format;
        let name = name_of(Some(path));
        match doc::save(path, &format.restore(&text)) {
            Ok(()) => {
                {
                    let mut s = self.state.borrow_mut();
                    s.saved = text;
                    s.path = Some(path.to_path_buf());
                }
                self.say(&format!("Saved \"{name}\"."), false);
                self.refresh();
                Ok(())
            }
            Err(why) => {
                let why = format!("Cannot save \"{name}\": {why}");
                self.say(&why, true);
                Err(why)
            }
        }
    }

    /// Saves, asking for a name first if the page has none, and goes on to
    /// `then` once the page is safe.
    fn save(&self, then: Option<Then>) {
        let (refused, read_only, path) = {
            let s = self.state.borrow();
            (s.refused, s.read_only, s.path.clone())
        };
        if refused || read_only {
            return;
        }
        match path {
            None => self.browse(Mode::SaveAs(then)),
            Some(path) => {
                if self.write(&path).is_ok() {
                    if let Some(then) = then {
                        self.proceed(then);
                    }
                }
            }
        }
    }

    fn busy(&self) -> bool {
        let s = self.state.borrow();
        s.asking.is_some() || s.browser.is_some()
    }

    /// Does `then`, after asking if there is something to lose.
    fn guard(&self, then: Then) {
        if self.busy() {
            return;
        }
        if !self.is_dirty() {
            self.proceed(then);
            return;
        }
        let name = name_of(self.state.borrow().path.as_deref());
        self.ask(
            Asking::Unsaved(then),
            Question {
                title: format!("Save changes to \"{name}\"?").into(),
                body: "Your changes will be lost if you do not save them.".into(),
                confirm: "Save".into(),
                other: "Discard".into(),
                danger: false,
            },
        );
    }

    fn proceed(&self, then: Then) {
        match then {
            Then::Close => {
                if let Some(window) = self.window.upgrade() {
                    let _ = window.hide();
                }
            }
            Then::New => self.open(None),
            Then::Open => self.browse(Mode::Open),
        }
    }

    fn ask(&self, asking: Asking, question: Question) {
        self.state.borrow_mut().asking = Some(asking);
        self.with_edit(|edit| {
            edit.set_question(question);
            edit.set_asking(true);
        });
    }

    fn stop_asking(&self) -> Option<Asking> {
        self.with_edit(|edit| edit.set_asking(false));
        self.state.borrow_mut().asking.take()
    }

    fn question_confirmed(&self) {
        match self.stop_asking() {
            Some(Asking::Unsaved(then)) => self.save(Some(then)),
            Some(Asking::Replace(path)) => self.save_under(&path),
            None => {}
        }
    }

    fn question_other(&self) {
        if let Some(Asking::Unsaved(then)) = self.stop_asking() {
            self.proceed(then);
        }
    }

    fn question_cancelled(&self) {
        if let Some(Asking::Replace(_)) = self.stop_asking() {
            self.with_edit(|edit| edit.set_name_tick(edit.get_name_tick() + 1));
        }
    }

    fn browse(&self, mode: Mode) {
        let (folder, home, proposed) = {
            let s = self.state.borrow();
            let folder = s
                .path
                .as_deref()
                .and_then(Path::parent)
                .filter(|p| p.is_dir())
                .unwrap_or(&s.home)
                .to_path_buf();
            let proposed = match mode {
                Mode::Open => String::new(),
                Mode::SaveAs(_) => s
                    .path
                    .as_deref()
                    .map_or_else(|| NEW_NAME.to_string(), |p| name_of(Some(p))),
            };
            (folder, s.home.clone(), proposed)
        };
        self.state.borrow_mut().browser = Some(Browser {
            mode,
            folder: folder.clone(),
            entries: Vec::new(),
            proposed: proposed.clone(),
        });
        self.with_edit(|edit| {
            edit.set_file_name(proposed.into());
            edit.set_entry(-1);
            edit.set_entries(ModelRc::default());
        });
        if !self.enter(&folder) {
            // A folder that cannot be read: start from home, which at least
            // gives the dialog something to say.
            self.show_folder(&home, Vec::new(), 0, "This folder cannot be read.");
        }
        self.with_edit(|edit| {
            edit.set_browsing(true);
            edit.set_name_tick(edit.get_name_tick() + 1);
        });
    }

    /// Lists `folder` in the dialog. False if it cannot be read, in which
    /// case the dialog stays where it was and says so.
    fn enter(&self, folder: &Path) -> bool {
        match browse::list(folder) {
            Ok((entries, left_out)) => {
                self.show_folder(folder, entries, left_out, "");
                true
            }
            Err(error) => {
                self.problem(&format!("That folder cannot be read: {error}"));
                false
            }
        }
    }

    fn show_folder(
        &self,
        folder: &Path,
        entries: Vec<browse::Entry>,
        left_out: usize,
        problem: &str,
    ) {
        let (mode, home, proposed) = {
            let mut s = self.state.borrow_mut();
            let home = s.home.clone();
            let Some(browser) = s.browser.as_mut() else {
                return;
            };
            browser.folder = folder.to_path_buf();
            browser.entries = entries.clone();
            (browser.mode, home, browser.proposed.clone())
        };
        let (title, confirm) = match mode {
            Mode::Open => ("Open a file", "Open"),
            Mode::SaveAs(_) => ("Save as", "Save"),
        };
        let rows: Vec<FileEntry> = entries
            .into_iter()
            .map(|e| FileEntry {
                name: e.name.into(),
                folder: e.folder,
            })
            .collect();
        let note = if left_out > 0 {
            format!("{left_out} more are not listed. Type a name to reach them.")
        } else {
            String::new()
        };
        self.with_edit(|edit| {
            edit.set_browse(Browse {
                title: title.into(),
                confirm: confirm.into(),
                folder: browse::shown(folder, &home).into(),
                can_up: folder.parent().is_some(),
                note: note.into(),
                problem: problem.into(),
            });
            edit.set_entries(ModelRc::new(VecModel::from(rows)));
            edit.set_entry(-1);
            edit.set_file_name(proposed.into());
        });
    }

    fn problem(&self, problem: &str) {
        self.with_edit(|edit| {
            let mut browse = edit.get_browse();
            browse.problem = problem.into();
            edit.set_browse(browse);
        });
    }

    fn close_browser(&self) -> Option<Mode> {
        self.with_edit(|edit| edit.set_browsing(false));
        self.state.borrow_mut().browser.take().map(|b| b.mode)
    }

    fn folder_up(&self) {
        let parent = self
            .state
            .borrow()
            .browser
            .as_ref()
            .and_then(|b| b.folder.parent().map(Path::to_path_buf));
        if let Some(parent) = parent {
            self.enter(&parent);
        }
    }

    fn entry_at(&self, index: i32) -> Option<browse::Entry> {
        let s = self.state.borrow();
        let browser = s.browser.as_ref()?;
        browser.entries.get(usize::try_from(index).ok()?).cloned()
    }

    /// Marks a row and puts its name in the field.
    fn entry_picked(&self, index: i32) {
        let Some(entry) = self.entry_at(index) else {
            return;
        };
        let typed = if entry.folder {
            format!("{}/", entry.name)
        } else {
            entry.name
        };
        self.with_edit(|edit| {
            edit.set_entry(index);
            edit.set_file_name(typed.into());
        });
        self.problem("");
    }

    fn entry_chosen(&self, index: i32) {
        self.entry_picked(index);
        self.file_accepted();
    }

    fn entry_stepped(&self, direction: i32) {
        let count = self
            .state
            .borrow()
            .browser
            .as_ref()
            .map_or(0, |b| b.entries.len() as i32);
        if count == 0 {
            return;
        }
        let current = self.with_edit(|e| e.get_entry()).unwrap_or(-1);
        let next = if current < 0 {
            0
        } else {
            (current + direction).clamp(0, count - 1)
        };
        self.entry_picked(next);
    }

    fn name_typed(&self) {
        let typed = self
            .with_edit(|e| e.get_file_name().to_string())
            .unwrap_or_default();
        if let Some(browser) = self.state.borrow_mut().browser.as_mut() {
            if matches!(browser.mode, Mode::SaveAs(_)) && !typed.contains('/') {
                browser.proposed = typed;
            }
        }
        self.with_edit(|edit| edit.set_entry(-1));
        self.problem("");
    }

    fn file_accepted(&self) {
        let typed = self
            .with_edit(|e| e.get_file_name().to_string())
            .unwrap_or_default();
        let (mode, folder, home, current) = {
            let s = self.state.borrow();
            let Some(browser) = s.browser.as_ref() else {
                return;
            };
            (
                browser.mode,
                browser.folder.clone(),
                s.home.clone(),
                s.path.clone(),
            )
        };
        if typed.trim().is_empty() {
            self.problem(match mode {
                Mode::Open => "Pick a file or type its name.",
                Mode::SaveAs(_) => "Type a name.",
            });
            return;
        }
        let target = browse::resolve(&typed, &folder, &home);
        if target.is_dir() {
            self.enter(&target);
            return;
        }
        let folder_meant = typed.trim_end().ends_with('/');
        match mode {
            Mode::Open => {
                if folder_meant || !target.exists() {
                    self.problem("There is no file of that name.");
                    return;
                }
                self.close_browser();
                self.open(Some(&target));
            }
            Mode::SaveAs(_) => {
                if folder_meant || !target.parent().is_some_and(Path::is_dir) {
                    self.problem("That folder does not exist.");
                } else if target.exists() && current.as_deref() != Some(target.as_path()) {
                    let name = name_of(Some(&target));
                    self.ask(
                        Asking::Replace(target),
                        Question {
                            title: format!("Replace \"{name}\"?").into(),
                            body: "A file with that name is there already. What is in it will be lost.".into(),
                            confirm: "Replace".into(),
                            other: "".into(),
                            danger: true,
                        },
                    );
                } else {
                    self.save_under(&target);
                }
            }
        }
    }

    /// Saves under the name the dialog arrived at. The dialog stays, saying
    /// why, if that cannot be done.
    fn save_under(&self, target: &Path) {
        match self.write(target) {
            Ok(()) => {
                if let Some(Mode::SaveAs(Some(then))) = self.close_browser() {
                    self.proceed(then);
                }
            }
            Err(why) => {
                self.say("", false);
                self.problem(&why);
                self.with_edit(|edit| edit.set_name_tick(edit.get_name_tick() + 1));
            }
        }
    }

    /// Ctrl+F with words selected looks for those words.
    fn find_opened(&self) {
        let Some((text, cursor, anchor)) = self.with_edit(|e| {
            (
                e.get_text(),
                e.get_cursor().max(0) as usize,
                e.get_anchor().max(0) as usize,
            )
        }) else {
            return;
        };
        let selected = text
            .get(cursor.min(anchor)..cursor.max(anchor))
            .unwrap_or("");
        if !selected.is_empty()
            && !selected.contains('\n')
            && selected.chars().count() <= FIND_FROM_SELECTION
        {
            self.with_edit(|edit| edit.set_find_text(selected.into()));
        }
        self.find_from(true, true);
    }

    fn find_from(&self, forward: bool, keep_current: bool) {
        let Some((text, query, cursor, anchor)) = self.with_edit(|e| {
            (
                e.get_text(),
                e.get_find_text(),
                e.get_cursor().max(0) as usize,
                e.get_anchor().max(0) as usize,
            )
        }) else {
            return;
        };
        let (low, high) = (cursor.min(anchor), cursor.max(anchor));
        let hit = if forward {
            find::next(&text, &query, if keep_current { low } else { high })
        } else {
            find::previous(&text, &query, low)
        };
        self.with_edit(|edit| match hit {
            Some((start, end)) => {
                edit.set_select_from(start as i32);
                edit.set_select_to(end as i32);
                edit.set_select_tick(edit.get_select_tick() + 1);
                let (which, total) = find::count(&text, &query, start);
                edit.set_find_result(format!("{which} of {total}").into());
            }
            None => edit.set_find_result(if query.is_empty() { "" } else { "No match" }.into()),
        });
        self.refresh();
    }

    /// Tab types spaces up to the next stop: the text field has no way to
    /// type a tab. They go in as typed keys, so that undo knows them.
    fn tab(&self) {
        if self.state.borrow().read_only {
            return;
        }
        let Some((text, cursor, anchor)) = self.with_edit(|e| {
            (
                e.get_text(),
                e.get_cursor().max(0) as usize,
                e.get_anchor().max(0) as usize,
            )
        }) else {
            return;
        };
        let (low, high) = (cursor.min(anchor), cursor.max(anchor));
        // Lines that are selected are not to be replaced by a stray Tab.
        if text.get(low..high).is_none_or(|s| s.contains('\n')) {
            return;
        }
        let (_, column) = find::line_and_column(&text, low);
        let spaces: SharedString = " ".repeat(TAB_WIDTH - (column - 1) % TAB_WIDTH).into();
        let window = self.window.clone();
        // Not while the Tab key is still being handled.
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(window) = window.upgrade() {
                let window = window.window();
                window.dispatch_event(WindowEvent::KeyPressed {
                    text: spaces.clone(),
                });
                window.dispatch_event(WindowEvent::KeyReleased { text: spaces });
            }
        });
    }

    /// The window's close button. True if the window may go.
    fn may_close(&self) -> bool {
        if !self.is_dirty() {
            return true;
        }
        let asking = self.state.borrow().asking.is_some();
        if !asking {
            self.close_browser();
            self.guard(Then::Close);
        }
        false
    }
}

fn wire(window: &EditWindow, app: &Rc<App>) {
    let edit = window.global::<Edit>();
    macro_rules! on {
        ($setter:ident, |$app:ident $(, $arg:ident)*| $body:expr) => {{
            let weak = Rc::downgrade(app);
            edit.$setter(move |$($arg),*| {
                if let Some($app) = weak.upgrade() {
                    $body
                }
            });
        }};
    }
    on!(on_new_file, |app| app.guard(Then::New));
    on!(on_open_file, |app| app.guard(Then::Open));
    on!(on_save, |app| if !app.busy() {
        app.save(None)
    });
    on!(on_save_as, |app| {
        let cannot = {
            let s = app.state.borrow();
            s.refused || s.read_only
        };
        if !cannot && !app.busy() {
            app.browse(Mode::SaveAs(None));
        }
    });
    on!(on_close, |app| app.guard(Then::Close));
    on!(on_text_edited, |app| app.edited());
    on!(on_caret_moved, |app| app.refresh());
    on!(on_tab, |app| app.tab());
    on!(on_redone, |app, before| app.redone(&before));
    on!(on_find_opened, |app| app.find_opened());
    on!(on_find_changed, |app, _text| app.find_from(true, true));
    on!(on_find_next, |app| app.find_from(true, false));
    on!(on_find_previous, |app| app.find_from(false, false));
    on!(on_question_confirmed, |app| app.question_confirmed());
    on!(on_question_other, |app| app.question_other());
    on!(on_question_cancelled, |app| app.question_cancelled());
    on!(on_folder_up, |app| app.folder_up());
    on!(on_entry_picked, |app, index| app.entry_picked(index));
    on!(on_entry_chosen, |app, index| app.entry_chosen(index));
    on!(on_entry_stepped, |app, direction| app
        .entry_stepped(direction));
    on!(on_name_typed, |app| app.name_typed());
    on!(on_file_accepted, |app| app.file_accepted());
    on!(on_file_cancelled, |app| {
        app.close_browser();
    });

    // The window's own close button asks the same question as Ctrl+W.
    let weak = Rc::downgrade(app);
    window
        .window()
        .on_close_requested(move || match weak.upgrade() {
            Some(app) if !app.may_close() => CloseRequestResponse::KeepWindowShown,
            _ => CloseRequestResponse::HideWindow,
        });
}

/// What a command line argument names: a path, or a `file://` address.
pub fn start_for(home: &Path, arg: Option<&str>, cwd: &Path) -> Result<Start, String> {
    let path = arg.map(|arg| sengi_open::resolve(arg, cwd)).transpose()?;
    Ok(Start {
        home: home.to_path_buf(),
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_are_counted_in_groups() {
        assert_eq!(characters(0), "0 characters");
        assert_eq!(characters(1), "1 character");
        assert_eq!(characters(999), "999 characters");
        assert_eq!(characters(1000), "1,000 characters");
        assert_eq!(characters(1234567), "1,234,567 characters");
    }
}
