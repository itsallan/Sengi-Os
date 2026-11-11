//! The window and what is shared between its pages: the settings, the one
//! dialog and the one chooser, and the way back to the UI thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::cli::Page;
use crate::derive;
use crate::settings::Settings;
use crate::store::{Dirs, Value};
use crate::ui::{ChooserState, DialogSpec, DialogState, Item, PageEntry, SettingsWindow};

/// A chooser never lays out more rows than this. A longer list says so
/// and is narrowed by typing.
pub const MAX_CHOICES: usize = 60;

/// One page's side of the window. All of it runs on the UI thread.
pub trait Controller {
    /// The page has come into view. Called again each time it does.
    fn show(&self, app: &Rc<App>);
    /// Another page has taken its place.
    fn hide(&self, _app: &Rc<App>) {}
    /// The window is closing for good.
    fn shutdown(&self) {}
    /// For finding the page again by its type; every page returns itself.
    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any>;
}

type Confirm = Box<dyn FnMut(&Rc<App>, String)>;
type Cancel = Box<dyn FnOnce(&Rc<App>)>;
type Chosen = Box<dyn FnOnce(&Rc<App>, String)>;

struct OpenDialog {
    confirm: Confirm,
    cancel: Option<Cancel>,
}

/// One row of a chooser.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub current: bool,
}

struct OpenChooser {
    choices: Vec<Choice>,
    choose: Chosen,
}

pub struct App {
    pub window: SettingsWindow,
    pub dirs: Dirs,
    pub settings: RefCell<Settings>,
    pages: RefCell<Vec<(Page, Rc<dyn Controller>)>>,
    current: Cell<Option<Page>>,
    dialog: RefCell<Option<OpenDialog>>,
    chooser: RefCell<Option<OpenChooser>>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Run `task` on the UI thread with the application, from any thread. If
/// the window has gone the task is dropped.
pub fn post(task: impl FnOnce(&Rc<App>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        let app = APP.with(|app| app.borrow().clone());
        if let Some(app) = app {
            task(&app);
        }
    });
}

/// The words of a row, as Slint wants them.
pub fn item(id: &str, title: &str, detail: &str) -> Item {
    Item {
        id: id.into(),
        title: title.into(),
        detail: detail.into(),
        enabled: true,
        ..Item::default()
    }
}

pub fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

/// The rows of `choices` that match what was typed: every word of it
/// somewhere in the title, the detail or the id, whatever the case.
pub fn narrow<'a>(choices: &'a [Choice], typed: &str) -> Vec<&'a Choice> {
    let words: Vec<String> = typed.split_whitespace().map(str::to_lowercase).collect();
    choices
        .iter()
        .filter(|choice| {
            let hay = format!("{} {} {}", choice.title, choice.detail, choice.id).to_lowercase();
            // Zone names are written with underscores and typed with spaces.
            let hay = hay.replace('_', " ");
            words.iter().all(|word| hay.contains(word.as_str()))
        })
        .collect()
}

impl App {
    pub fn new(window: SettingsWindow, dirs: Dirs, settings: Settings) -> Rc<App> {
        let app = Rc::new(App {
            window,
            dirs,
            settings: RefCell::new(settings),
            pages: RefCell::default(),
            current: Cell::new(None),
            dialog: RefCell::default(),
            chooser: RefCell::default(),
        });
        APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));

        let weak = Rc::downgrade(&app);
        app.window.on_page_selected(move |name| {
            if let (Some(app), Some(page)) = (weak.upgrade(), Page::parse(&name)) {
                app.show(page);
            }
        });

        let dialog = app.window.global::<DialogState>();
        let weak = Rc::downgrade(&app);
        dialog.on_confirmed(move |text| {
            let Some(app) = weak.upgrade() else { return };
            // Taken out while it runs, so the handler may open another
            // dialog; put back if it left this one open.
            let open = app.dialog.borrow_mut().take();
            if let Some(mut open) = open {
                (open.confirm)(&app, text.into());
                let mut slot = app.dialog.borrow_mut();
                if slot.is_none() && app.window.global::<DialogState>().get_open() {
                    *slot = Some(open);
                }
            }
        });
        let weak = Rc::downgrade(&app);
        dialog.on_cancelled(move || {
            if let Some(app) = weak.upgrade() {
                app.cancel_dialog();
            }
        });

        let chooser = app.window.global::<ChooserState>();
        let weak = Rc::downgrade(&app);
        chooser.on_chosen(move |id| {
            let Some(app) = weak.upgrade() else { return };
            let open = app.chooser.borrow_mut().take();
            app.window.global::<ChooserState>().set_open(false);
            app.window.invoke_focus_sidebar();
            if let Some(open) = open {
                (open.choose)(&app, id.into());
            }
        });
        let weak = Rc::downgrade(&app);
        chooser.on_cancelled(move || {
            if let Some(app) = weak.upgrade() {
                app.chooser.borrow_mut().take();
                app.window.global::<ChooserState>().set_open(false);
                app.window.invoke_focus_sidebar();
            }
        });
        let weak = Rc::downgrade(&app);
        chooser.on_filtered(move |typed| {
            if let Some(app) = weak.upgrade() {
                app.fill_chooser(&typed, false);
            }
        });
        app
    }

    /// Add a page. They are listed in the order of [`Page::ALL`].
    pub fn register(&self, page: Page, controller: Rc<dyn Controller>) {
        let mut pages = self.pages.borrow_mut();
        pages.push((page, controller));
        pages.sort_by_key(|(page, _)| Page::ALL.iter().position(|p| p == page));
        let entries: Vec<PageEntry> = pages
            .iter()
            .map(|(page, _)| PageEntry {
                name: page.name().into(),
                title: page.title().into(),
                icon: icon(*page).into(),
            })
            .collect();
        self.window.set_pages(model(entries));
    }

    fn controller(&self, page: Page) -> Option<Rc<dyn Controller>> {
        self.pages
            .borrow()
            .iter()
            .find(|(p, _)| *p == page)
            .map(|(_, controller)| controller.clone())
    }

    /// The page of type `T`, if this build has it.
    pub fn page<T: 'static>(&self) -> Option<Rc<T>> {
        let pages: Vec<_> = self.pages.borrow().iter().map(|(_, c)| c.clone()).collect();
        pages
            .into_iter()
            .find_map(|controller| controller.as_any().downcast::<T>().ok())
    }

    /// The first page there is.
    pub fn first_page(&self) -> Option<Page> {
        self.pages.borrow().first().map(|(page, _)| *page)
    }

    /// Bring `page` into view. One that is not there is not shown.
    pub fn show(self: &Rc<App>, page: Page) {
        let Some(next) = self.controller(page) else {
            return;
        };
        if self.current.get() == Some(page) {
            return;
        }
        if let Some(previous) = self.current.get().and_then(|p| self.controller(p)) {
            previous.hide(self);
        }
        self.current.set(Some(page));
        self.window.set_page(page.name().into());
        next.show(self);
    }

    pub fn current(&self) -> Option<Page> {
        self.current.get()
    }

    /// Tell every page the window is closing.
    pub fn shutdown(&self) {
        let pages: Vec<_> = self.pages.borrow().iter().map(|(_, c)| c.clone()).collect();
        for page in pages {
            page.shutdown();
        }
        APP.with(|slot| slot.borrow_mut().take());
    }

    /// Ask a question. `confirm` is called with what was typed, if the
    /// dialog has a field, each time the confirm button is pressed; the
    /// dialog stays open until someone calls [`App::close_dialog`].
    /// `cancel` is called if the user backs out.
    pub fn open_dialog(
        &self,
        spec: DialogSpec,
        confirm: impl FnMut(&Rc<App>, String) + 'static,
        cancel: impl FnOnce(&Rc<App>) + 'static,
    ) {
        let state = self.window.global::<DialogState>();
        state.set_spec(spec);
        state.set_countdown("".into());
        state.set_busy(false);
        state.set_open(true);
        *self.dialog.borrow_mut() = Some(OpenDialog {
            confirm: Box::new(confirm),
            cancel: Some(Box::new(cancel)),
        });
    }

    /// A question with one answer that matters: closes on either button
    /// and calls `confirmed` only for the confirming one.
    pub fn ask(&self, spec: DialogSpec, confirmed: impl FnOnce(&Rc<App>) + 'static) {
        let mut confirmed = Some(confirmed);
        self.open_dialog(
            spec,
            move |app, _| {
                app.close_dialog();
                if let Some(confirmed) = confirmed.take() {
                    confirmed(app);
                }
            },
            |_| {},
        );
    }

    /// Change what the open dialog says without closing it.
    pub fn update_dialog(&self, change: impl FnOnce(&mut DialogSpec)) {
        let state = self.window.global::<DialogState>();
        if state.get_open() {
            let mut spec = state.get_spec();
            change(&mut spec);
            state.set_spec(spec);
        }
    }

    pub fn set_dialog_busy(&self, busy: bool) {
        self.window.global::<DialogState>().set_busy(busy);
    }

    /// Empty the dialog's field and put the caret back in it.
    pub fn wipe_dialog_field(&self) {
        let state = self.window.global::<DialogState>();
        state.set_wipe(state.get_wipe().wrapping_add(1));
    }

    pub fn set_dialog_countdown(&self, text: &str) {
        self.window
            .global::<DialogState>()
            .set_countdown(text.into());
    }

    pub fn dialog_open(&self) -> bool {
        self.window.global::<DialogState>().get_open()
    }

    pub fn close_dialog(&self) {
        self.dialog.borrow_mut().take();
        self.window.global::<DialogState>().set_open(false);
        self.window.invoke_focus_sidebar();
    }

    /// Close the dialog as Escape does, running its cancel handler.
    pub fn cancel_dialog(self: &Rc<App>) {
        let open = self.dialog.borrow_mut().take();
        self.window.global::<DialogState>().set_open(false);
        self.window.invoke_focus_sidebar();
        if let Some(cancel) = open.and_then(|mut open| open.cancel.take()) {
            cancel(self);
        }
    }

    /// Offer `choices` to pick one from.
    pub fn choose(
        &self,
        title: &str,
        searchable: bool,
        choices: Vec<Choice>,
        chosen: impl FnOnce(&Rc<App>, String) + 'static,
    ) {
        let state = self.window.global::<ChooserState>();
        state.set_title(title.into());
        state.set_searchable(searchable);
        state.set_placeholder("Search".into());
        *self.chooser.borrow_mut() = Some(OpenChooser {
            choices,
            choose: Box::new(chosen),
        });
        self.fill_chooser("", true);
        state.set_open(true);
    }

    fn fill_chooser(&self, typed: &str, opening: bool) {
        let state = self.window.global::<ChooserState>();
        let chooser = self.chooser.borrow();
        let Some(open) = chooser.as_ref() else { return };
        let matching = narrow(&open.choices, typed);
        let total = matching.len();
        // On opening, the list starts at the current choice even when
        // that is far down a long one.
        let at = matching.iter().position(|choice| choice.current);
        let first = match at {
            Some(at) if opening && total > MAX_CHOICES => {
                at.saturating_sub(MAX_CHOICES / 2).min(total - MAX_CHOICES)
            }
            _ => 0,
        };
        let rows: Vec<Item> = matching
            .iter()
            .skip(first)
            .take(MAX_CHOICES)
            .map(|choice| Item {
                checked: choice.current,
                action: "Choose".into(),
                ..item(&choice.id, &choice.title, &choice.detail)
            })
            .collect();
        state.set_empty(if total == 0 { "Nothing matches" } else { "" }.into());
        state.set_current(match at {
            Some(at) if at >= first && at < first + MAX_CHOICES => (at - first) as i32,
            _ => 0,
        });
        state.set_items(model(rows));
    }

    /// Store one setting. A failure is shown in a dialog and returned.
    pub fn set(self: &Rc<App>, section: &str, key: &str, value: Value) -> bool {
        let result = self.settings.borrow_mut().set(section, key, value);
        match result {
            Ok(()) => true,
            Err(problem) => {
                eprintln!("sengi-settings: {problem}");
                self.tell("The setting could not be saved", &problem);
                false
            }
        }
    }

    /// Say something that needs no answer.
    pub fn tell(&self, title: &str, body: &str) {
        self.open_dialog(
            DialogSpec {
                title: title.into(),
                body: body.into(),
                cancel: "Close".into(),
                ..DialogSpec::default()
            },
            |_, _| {},
            |_| {},
        );
    }

    /// Write labwc's files from the settings and have it read them, off
    /// the UI thread. Problems come back in a dialog.
    pub fn apply_to_session(&self) {
        let dirs = self.dirs.clone();
        let settings = self.settings.borrow().clone();
        std::thread::spawn(move || {
            let applied = derive::apply(&dirs, &settings);
            if applied.changed() {
                derive::reconfigure();
            }
            let problems = applied.problems();
            if !problems.is_empty() {
                post(move |app| {
                    app.tell("The setting could not be applied", &problems.join("\n\n"));
                });
            }
        });
    }
}

fn icon(page: Page) -> &'static str {
    match page {
        Page::Network => "signal",
        Page::Bluetooth => "bluetooth",
        Page::Sound => "speaker",
        Page::Display => "display",
        Page::Power => "battery",
        Page::DateTime => "clock",
        Page::Keyboard => "keyboard",
        Page::Mouse => "mouse",
        Page::Notifications => "bell",
        Page::About => "info",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(id: &str, title: &str, detail: &str) -> Choice {
        Choice {
            id: id.to_string(),
            title: title.to_string(),
            detail: detail.to_string(),
            current: false,
        }
    }

    #[test]
    fn typing_narrows_by_every_word() {
        let choices = [
            choice("Europe/Paris", "Paris", "France"),
            choice("America/New_York", "New York", "United States"),
            choice(
                "America/Argentina/Buenos_Aires",
                "Buenos Aires",
                "Argentina",
            ),
        ];
        let ids = |typed: &str| -> Vec<&str> {
            narrow(&choices, typed)
                .iter()
                .map(|choice| choice.id.as_str())
                .collect()
        };
        assert_eq!(ids("").len(), 3);
        assert_eq!(ids("  "), ids(""));
        assert_eq!(ids("paris"), ["Europe/Paris"]);
        assert_eq!(ids("FRANCE"), ["Europe/Paris"]);
        assert_eq!(ids("new york"), ["America/New_York"]);
        assert_eq!(ids("york states"), ["America/New_York"]);
        assert_eq!(ids("america").len(), 2);
        assert_eq!(ids("buenos aires"), ["America/Argentina/Buenos_Aires"]);
        assert!(ids("atlantis").is_empty());
    }

    #[test]
    fn every_page_has_an_icon() {
        for page in Page::ALL {
            assert!(!icon(page).is_empty());
        }
    }
}
