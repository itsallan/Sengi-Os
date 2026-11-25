//! Connects the editor window to the file: loading, the unsaved mark,
//! saving, finding, and asking before anything is lost.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use slint::{CloseRequestResponse, ComponentHandle};

use crate::doc::{self, Refusal};
use crate::find;
use crate::ui::{DialogSpec, Edit, EditWindow};

/// What the program was started with.
pub struct Start {
    pub home: PathBuf,
    pub path: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Asking {
    /// Close with changes that were not saved.
    Discard,
    /// Save a file that has no name yet.
    SaveAs,
}

struct State {
    home: PathBuf,
    path: Option<PathBuf>,
    /// The text as it is on disk, to tell whether anything changed.
    saved: String,
    /// The file was not opened, so there is nothing to edit or save.
    refused: bool,
    asking: Option<Asking>,
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
            path: start.path.clone(),
            saved: String::new(),
            refused: false,
            asking: None,
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

impl App {
    fn with_edit<T>(&self, f: impl FnOnce(Edit<'_>) -> T) -> Option<T> {
        self.window
            .upgrade()
            .map(|window| f(window.global::<Edit>()))
    }

    fn open(&self, path: Option<&Path>) {
        let name = name_of(path);
        let mut notice = (String::new(), false);
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
                        notice = (
                            format!("New file. \"{name}\" is made when you save."),
                            false,
                        );
                    } else {
                        refusal = format!(
                            "The folder for \"{name}\" does not exist, so the file cannot be made."
                        );
                    }
                }
                Err(why) => refusal = why.say(&name),
            }
        }
        {
            let mut s = self.state.borrow_mut();
            s.saved = text.clone();
            s.refused = !refusal.is_empty();
        }
        self.with_edit(|edit| {
            edit.set_text(text.into());
            edit.set_cursor(0);
            edit.set_anchor(0);
            edit.set_refusal(refusal.into());
            edit.set_notice(notice.0.into());
            edit.set_notice_is_error(notice.1);
        });
        self.refresh();
    }

    /// The title, the unsaved mark and the line at the bottom.
    fn refresh(&self) {
        let (name, dirty) = {
            let s = self.state.borrow();
            let dirty = self.text().is_some_and(|t| t != s.saved);
            (name_of(s.path.as_deref()), dirty)
        };
        let (line, column) = match self.text() {
            Some(text) => find::line_and_column(
                &text,
                self.with_edit(|e| e.get_cursor()).unwrap_or(0).max(0) as usize,
            ),
            None => (1, 1),
        };
        self.with_edit(|edit| {
            edit.set_name(name.clone().into());
            edit.set_dirty(dirty);
            edit.set_title(
                format!(
                    "{}{name} \u{2014} Text Editor",
                    if dirty { "* " } else { "" }
                )
                .into(),
            );
            let status = if edit.get_refusal().is_empty() {
                format!("Line {line}, column {column}")
            } else {
                String::new()
            };
            edit.set_status(status.into());
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

    /// The line at the bottom, for tests.
    pub fn status_text(&self) -> String {
        self.with_edit(|edit| {
            let notice = edit.get_notice();
            if notice.is_empty() {
                edit.get_status().to_string()
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
        self.refresh();
    }

    fn save(&self) {
        if self.state.borrow().refused {
            return;
        }
        let path = self.state.borrow().path.clone();
        let Some(path) = path else {
            self.ask_name();
            return;
        };
        let Some(text) = self.text() else { return };
        match doc::save(&path, &text) {
            Ok(()) => {
                self.state.borrow_mut().saved = text;
                self.say(&format!("Saved \"{}\".", name_of(Some(&path))), false);
            }
            Err(why) => {
                self.say(
                    &format!("Cannot save \"{}\": {why}", name_of(Some(&path))),
                    true,
                );
            }
        }
        self.refresh();
    }

    fn hide(&self) {
        if let Some(window) = self.window.upgrade() {
            let _ = window.hide();
        }
    }

    fn ask(&self, asking: Asking, spec: DialogSpec) {
        self.state.borrow_mut().asking = Some(asking);
        self.with_edit(|edit| {
            edit.set_dialog(spec);
            edit.set_dialog_open(true);
        });
    }

    fn ask_name(&self) {
        self.ask(
            Asking::SaveAs,
            DialogSpec {
                title: "Save as".into(),
                body: "Type a file name. It is saved in your home folder unless you type a path."
                    .into(),
                input: true,
                placeholder: "notes.txt".into(),
                confirm: "Save".into(),
                cancel: "Cancel".into(),
                minimum: 1,
                ..DialogSpec::default()
            },
        );
    }

    /// Closes the window, after asking if there is something to lose.
    fn close(&self) {
        if !self.is_dirty() {
            self.hide();
            return;
        }
        let name = name_of(self.state.borrow().path.as_deref());
        self.ask(
            Asking::Discard,
            DialogSpec {
                title: "Close without saving?".into(),
                body: format!("The changes to \"{name}\" will be lost.").into(),
                confirm: "Discard changes".into(),
                cancel: "Keep editing".into(),
                danger: true,
                ..DialogSpec::default()
            },
        );
    }

    fn close_dialog(&self) -> Option<Asking> {
        self.with_edit(|edit| edit.set_dialog_open(false));
        self.state.borrow_mut().asking.take()
    }

    fn dialog_confirmed(&self, typed: &str) {
        let asking = self.state.borrow().asking;
        match asking {
            Some(Asking::Discard) => {
                self.close_dialog();
                self.hide();
            }
            Some(Asking::SaveAs) => self.save_as(typed),
            None => {}
        }
    }

    fn save_as(&self, typed: &str) {
        let target = doc::place_named(typed, &self.state.borrow().home);
        let problem = if typed.trim().is_empty() {
            Some("Type a name.")
        } else if target.is_dir() {
            Some("That is a folder.")
        } else if target.exists() {
            Some("A file with that name exists already.")
        } else if !target.parent().is_some_and(Path::is_dir) {
            Some("That folder does not exist.")
        } else {
            None
        };
        if let Some(problem) = problem {
            self.with_edit(|edit| {
                let mut spec = edit.get_dialog();
                spec.problem = problem.into();
                edit.set_dialog(spec);
            });
            return;
        }
        self.close_dialog();
        self.state.borrow_mut().path = Some(target);
        self.save();
        // A name that could not be written to is not kept.
        if self.is_dirty() {
            self.state.borrow_mut().path = None;
            self.refresh();
        }
    }

    fn find_from(&self, forward: bool, keep_current: bool) {
        let Some((text, query, cursor, anchor)) = self.with_edit(|e| {
            (
                e.get_text().to_string(),
                e.get_find_text().to_string(),
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
    on!(on_save, |app| app.save());
    on!(on_close, |app| app.close());
    on!(on_text_edited, |app| app.edited());
    on!(on_caret_moved, |app| app.refresh());
    on!(on_find_changed, |app, _text| app.find_from(true, true));
    on!(on_find_next, |app| app.find_from(true, false));
    on!(on_find_previous, |app| app.find_from(false, false));
    on!(on_dialog_confirmed, |app, text| app.dialog_confirmed(&text));
    on!(on_dialog_cancelled, |app| {
        app.close_dialog();
    });

    // The window's own close button asks the same question as Ctrl+W.
    let weak = Rc::downgrade(app);
    window
        .window()
        .on_close_requested(move || match weak.upgrade() {
            Some(app) if app.is_dirty() && app.state.borrow().asking.is_none() => {
                app.close();
                CloseRequestResponse::KeepWindowShown
            }
            Some(app) if app.state.borrow().asking.is_some() => {
                CloseRequestResponse::KeepWindowShown
            }
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
