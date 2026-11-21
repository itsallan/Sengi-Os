//! The window and what is shared between its pages: the settings, the one
//! dialog and the one chooser, and the way back to the UI thread.
//!
//! There is one dialog and several pages that may want it, some of them
//! with news that arrives seconds after it was asked for. So the dialog
//! has an owner: the part of the program (a page's module) that opened
//! it. An owner may change or close its own dialog as it likes. A
//! notice waits until no question is open, the owner's own included, and
//! anyone else's question first backs the open one out as Escape would,
//! so that its owner hears of it. Nobody can close or write into a dialog
//! that is not theirs; a page that tries has lost track of its dialog,
//! which is said on standard error and stops a debug build.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::panic::Location;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::cli::Page;
use crate::derive;
use crate::settings::{self, Change, Settings};
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

/// The part of the program a dialog belongs to: see [`owner_of`].
type Owner = &'static str;

/// Notices kept waiting for the dialog. A ninth is dropped, not the
/// first: when that many pile up, what came first is the cause and the
/// rest mostly its echo.
const MAX_NOTICES: usize = 8;

/// Something said that needs no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Notice {
    owner: Owner,
    title: String,
    body: String,
}

struct OpenDialog {
    confirm: Confirm,
    cancel: Option<Cancel>,
    /// What it says, if it is only a notice, so it can wait its turn
    /// again when a question takes its place.
    notice: Option<Notice>,
}

/// Who is asking for the dialog: the module the call was written in.
/// `src/network/page.rs` and `src/network/mod.rs` are both `network`;
/// `src/power.rs` is `power.rs`.
fn owner_of(file: &'static str) -> Owner {
    let inside = file.rsplit_once("src/").map_or(file, |(_, inside)| inside);
    inside.split('/').next().unwrap_or(inside)
}

/// What to do about a dialog that wants to open.
#[derive(Debug, PartialEq, Eq)]
enum Admit {
    /// The dialog is free, or its owner is changing it: show it.
    Show,
    /// A notice of someone else's is up: put it back in the queue first.
    AfterRequeue,
    /// A question of someone else's is up: back it out first.
    AfterCancel,
    /// Someone else has the dialog and this is only a notice: it waits.
    Wait,
}

/// `open` is who has the dialog now and whether theirs is a notice.
fn admit(open: Option<(Owner, bool)>, owner: Owner, is_notice: bool) -> Admit {
    match open {
        None => Admit::Show,
        // News does not take the place of a question, not even its
        // owner's: an owner whose question is over closes it first.
        Some((_, false)) if is_notice => Admit::Wait,
        Some((holder, _)) if holder == owner => Admit::Show,
        Some(_) if is_notice => Admit::Wait,
        Some((_, true)) => Admit::AfterRequeue,
        Some((_, false)) => Admit::AfterCancel,
    }
}

/// What became of a call that changes or closes the caller's dialog.
#[derive(Debug, PartialEq, Eq)]
enum Reach {
    /// The open dialog is the caller's.
    Mine,
    /// No dialog is open, or the open one is someone else's, named here.
    Missed(Option<Owner>),
}

fn reach(open: Option<(Owner, bool)>, owner: Owner) -> Reach {
    match open {
        Some((holder, _)) if holder == owner => Reach::Mine,
        Some((holder, _)) => Reach::Missed(Some(holder)),
        None => Reach::Missed(None),
    }
}

/// The words for a call that did not reach a dialog.
fn missed_words(what: &str, owner: Owner, holder: Option<Owner>) -> String {
    match holder {
        Some(holder) => format!("{owner} tried to {what} the dialog, which is {holder}'s"),
        None => format!("{owner} tried to {what} a dialog that is not open"),
    }
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

/// How long closing the window waits for the last change to be written
/// and applied.
const LAST_APPLY: Duration = Duration::from_secs(5);

struct Slot<T> {
    waiting: Option<T>,
    closed: bool,
    over: bool,
}

/// One thread that does the newest thing it was asked and skips what was
/// overtaken. A request made while one is being carried out waits; one
/// made while another waits is merged into it. So there are never two at
/// once, and the last one asked for is always the last one done.
pub struct Latest<T> {
    shared: Arc<(Mutex<Slot<T>>, Condvar)>,
    merge: fn(&mut T, T),
}

impl<T> Clone for Latest<T> {
    fn clone(&self) -> Latest<T> {
        Latest {
            shared: self.shared.clone(),
            merge: self.merge,
        }
    }
}

impl<T: Send + 'static> Latest<T> {
    /// `merge` folds a newer request into one still waiting.
    pub fn spawn(
        name: &str,
        merge: fn(&mut T, T),
        mut run: impl FnMut(T) + Send + 'static,
    ) -> Latest<T> {
        let shared = Arc::new((
            Mutex::new(Slot {
                waiting: None,
                closed: false,
                over: false,
            }),
            Condvar::new(),
        ));
        let theirs = shared.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("settings-{name}"))
            .spawn(move || {
                let (slot, told) = &*theirs;
                loop {
                    let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
                    while guard.waiting.is_none() && !guard.closed {
                        guard = told.wait(guard).unwrap_or_else(|p| p.into_inner());
                    }
                    let Some(job) = guard.waiting.take() else {
                        guard.over = true;
                        told.notify_all();
                        return;
                    };
                    drop(guard);
                    run(job);
                }
            });
        if let Err(error) = spawned {
            eprintln!("sengi-settings: could not start the {name} worker: {error}");
            shared.0.lock().unwrap_or_else(|p| p.into_inner()).over = true;
        }
        Latest { shared, merge }
    }

    pub fn request(&self, job: T) {
        let (slot, told) = &*self.shared;
        let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
        if guard.closed {
            return;
        }
        match guard.waiting.as_mut() {
            Some(waiting) => (self.merge)(waiting, job),
            None => guard.waiting = Some(job),
        }
        told.notify_all();
    }

    /// Take no more requests, and wait for at most `limit` until what
    /// was asked for has been done. False if it had not by then.
    pub fn finish(&self, limit: Duration) -> bool {
        let (slot, told) = &*self.shared;
        let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
        guard.closed = true;
        told.notify_all();
        let (guard, _) = told
            .wait_timeout_while(guard, limit, |slot| !slot.over)
            .unwrap_or_else(|p| p.into_inner());
        guard.over
    }
}

/// What is owed to the disk and to the session.
#[derive(Default)]
struct Job {
    /// The keys to change in the settings file, oldest first, each with
    /// the number the window knows it by.
    changes: Vec<(u64, Change)>,
    /// Whether labwc's files should be written from the settings.
    session: bool,
    /// Things to do once both are done, such as restarting a program
    /// that reads the settings file as it starts.
    after: Vec<Box<dyn FnOnce() + Send>>,
}

fn merge_jobs(waiting: &mut Job, newer: Job) {
    waiting.changes.extend(newer.changes);
    waiting.session |= newer.session;
    waiting.after.extend(newer.after);
}

/// `written` is the worker's own reading of the settings: the one copy
/// that is written from, by way of [`Settings::change`], which looks at
/// the file as it is now before it changes a key in it.
fn carry_out(dirs: &Dirs, written: &mut Settings, job: Job) {
    if let Some((upto, _)) = job.changes.last() {
        let upto = *upto;
        let changes: Vec<Change> = job.changes.into_iter().map(|(_, change)| change).collect();
        match written.change(&changes) {
            Ok(()) => post(move |app| app.landed(upto)),
            Err(problem) => {
                eprintln!("sengi-settings: {problem}");
                post(move |app| {
                    // What is shown should be what is stored.
                    app.landed(upto);
                    app.reload();
                    app.tell("The setting could not be saved", &problem);
                });
            }
        }
    }
    if job.session {
        // From the file, as the next login will do it.
        written.catch_up();
        let applied = derive::apply(dirs, written);
        if applied.changed() {
            derive::reconfigure();
        }
        let problems = applied.problems();
        if !problems.is_empty() {
            post(move |app| {
                app.tell("The setting could not be applied", &problems.join("\n\n"));
            });
        }
    }
    for task in job.after {
        task();
    }
}

pub struct App {
    pub window: SettingsWindow,
    pub dirs: Dirs,
    pub settings: RefCell<Settings>,
    /// Changes shown in `settings` that the worker has yet to write,
    /// oldest first. They are put back on top of the file whenever it
    /// has to be read again before they are in it.
    unwritten: RefCell<Vec<(u64, Change)>>,
    changes_made: Cell<u64>,
    applier: Latest<Job>,
    pages: RefCell<Vec<(Page, Rc<dyn Controller>)>>,
    current: Cell<Option<Page>>,
    dialog: RefCell<Option<OpenDialog>>,
    /// The open dialog's cancel handler while its confirm handler runs
    /// and the dialog itself is out of `dialog`. A question that takes
    /// the dialog's place from inside that handler finds it here.
    confirming: RefCell<Option<Cancel>>,
    /// Whose the open dialog is, also while its confirm handler runs.
    dialog_owner: Cell<Option<Owner>>,
    dialog_is_notice: Cell<bool>,
    notices: RefCell<VecDeque<Notice>>,
    chooser: RefCell<Option<OpenChooser>>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

type LastWords = Box<dyn Fn() + Send>;

static LAST_WORDS: Mutex<Vec<LastWords>> = Mutex::new(Vec::new());
static TERMINATING: AtomicBool = AtomicBool::new(false);

/// Something that must be done before the process ends when it is told
/// to end by a signal: undoing a display change still on trial, landing
/// the last setting. It is run on the thread that hears of the signal,
/// not the UI thread, which may be the reason the signal was sent; so it
/// may only touch what any thread may.
pub fn on_terminate(hook: impl Fn() + Send + 'static) {
    LAST_WORDS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(Box::new(hook));
}

/// As [`on_terminate`], for what cannot wait behind the rest: it is done
/// before everything registered so far. Whoever sent the signal may send
/// a harder one soon, and a display left in a mode nobody agreed to is
/// worse than a setting applied a moment late.
pub fn on_terminate_first(hook: impl Fn() + Send + 'static) {
    LAST_WORDS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(0, Box::new(hook));
}

/// The process has been told to end: do what was registered with
/// [`on_terminate_first`] and then with [`on_terminate`], in order.
/// Returns when all of it is done.
pub fn terminate() {
    TERMINATING.store(true, Ordering::SeqCst);
    let hooks = LAST_WORDS.lock().unwrap_or_else(|p| p.into_inner());
    for hook in hooks.iter() {
        hook();
    }
}

pub fn terminating() -> bool {
    TERMINATING.load(Ordering::SeqCst)
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
        let applier = Latest::spawn("apply", merge_jobs, {
            let dirs = dirs.clone();
            let mut written = settings.clone();
            move |job| carry_out(&dirs, &mut written, job)
        });
        let app = Rc::new(App {
            window,
            unwritten: RefCell::default(),
            changes_made: Cell::new(0),
            applier,
            dirs,
            settings: RefCell::new(settings),
            pages: RefCell::default(),
            current: Cell::new(None),
            dialog: RefCell::default(),
            confirming: RefCell::default(),
            dialog_owner: Cell::new(None),
            dialog_is_notice: Cell::new(false),
            notices: RefCell::default(),
            chooser: RefCell::default(),
        });
        APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
        on_terminate({
            let applier = app.applier.clone();
            move || {
                applier.finish(LAST_APPLY);
            }
        });

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
            let owner = app.dialog_owner.get();
            if let Some(mut open) = open {
                *app.confirming.borrow_mut() = open.cancel.take();
                (open.confirm)(&app, text.into());
                // Gone if someone else's question backed this one out.
                open.cancel = app.confirming.borrow_mut().take();
                let mut slot = app.dialog.borrow_mut();
                if slot.is_none()
                    && app.window.global::<DialogState>().get_open()
                    && app.dialog_owner.get() == owner
                {
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

    /// Tell every page the window is closing, and see the last change
    /// written and applied.
    pub fn shutdown(&self) {
        let pages: Vec<_> = self.pages.borrow().iter().map(|(_, c)| c.clone()).collect();
        for page in pages {
            page.shutdown();
        }
        if !self.applier.finish(LAST_APPLY) {
            eprintln!("sengi-settings: the last change was still being applied at exit");
        }
        APP.with(|slot| slot.borrow_mut().take());
    }

    fn here() -> Option<Rc<App>> {
        APP.with(|app| app.borrow().clone())
    }

    /// Who has the dialog, and whether theirs is a notice.
    fn holder(&self) -> Option<(Owner, bool)> {
        let open = self.window.global::<DialogState>().get_open();
        self.dialog_owner
            .get()
            .filter(|_| open)
            .map(|owner| (owner, self.dialog_is_notice.get()))
    }

    fn owns(&self, owner: Owner) -> bool {
        reach(self.holder(), owner) == Reach::Mine
    }

    /// As [`App::owns`], for a caller that believes the dialog is its
    /// own. If it is not, the call does nothing, and that is never what
    /// its author meant: a dialog is left open, or its words unchanged.
    fn mine(&self, owner: Owner, what: &str) -> bool {
        match reach(self.holder(), owner) {
            Reach::Mine => true,
            Reach::Missed(holder) => {
                let words = missed_words(what, owner, holder);
                eprintln!("sengi-settings: {words}");
                debug_assert!(false, "{words}");
                false
            }
        }
    }

    /// Each change empties the dialog's field. Whatever was typed into
    /// one dialog must never be found in the next.
    fn wipe(&self) {
        let state = self.window.global::<DialogState>();
        state.set_wipe(state.get_wipe().wrapping_add(1));
    }

    fn put(&self, owner: Owner, spec: DialogSpec, dialog: OpenDialog) {
        let state = self.window.global::<DialogState>();
        state.set_spec(spec);
        state.set_countdown("".into());
        state.set_busy(false);
        self.wipe();
        state.set_open(true);
        self.dialog_owner.set(Some(owner));
        self.dialog_is_notice.set(dialog.notice.is_some());
        *self.dialog.borrow_mut() = Some(dialog);
    }

    /// Close the dialog without telling anyone, and show what waited.
    fn dismiss(&self) {
        self.dialog.borrow_mut().take();
        self.dialog_owner.set(None);
        self.window.global::<DialogState>().set_open(false);
        self.wipe();
        self.window.invoke_focus_sidebar();
        self.next_notice();
    }

    fn next_notice(&self) {
        if self.window.global::<DialogState>().get_open() {
            return;
        }
        let next = self.notices.borrow_mut().pop_front();
        if let Some(notice) = next {
            self.show_notice(notice);
        }
    }

    fn show_notice(&self, notice: Notice) {
        self.put(
            notice.owner,
            DialogSpec {
                title: notice.title.as_str().into(),
                body: notice.body.as_str().into(),
                cancel: "Close".into(),
                ..DialogSpec::default()
            },
            OpenDialog {
                confirm: Box::new(|_, _| {}),
                cancel: None,
                notice: Some(notice),
            },
        );
    }

    fn open_as(&self, owner: Owner, spec: DialogSpec, confirm: Confirm, cancel: Cancel) {
        // Backing a question out runs its owner's code, which may put up
        // something of its own; that is dealt with the same way, a few
        // times at most.
        for _ in 0..4 {
            match admit(self.holder(), owner, false) {
                Admit::Show | Admit::Wait => break,
                Admit::AfterRequeue => {
                    let open = self.dialog.borrow_mut().take();
                    self.dialog_owner.set(None);
                    if let Some(notice) = open.and_then(|open| open.notice) {
                        self.notices.borrow_mut().push_front(notice);
                    }
                }
                Admit::AfterCancel => {
                    let open = self.dialog.borrow_mut().take();
                    self.dialog_owner.set(None);
                    // Out of its slot if this is its own confirm handler
                    // asking, by way of another part of the program.
                    let cancel = open
                        .and_then(|mut open| open.cancel.take())
                        .or_else(|| self.confirming.borrow_mut().take());
                    if let (Some(cancel), Some(app)) = (cancel, App::here()) {
                        cancel(&app);
                    }
                }
            }
        }
        self.put(
            owner,
            spec,
            OpenDialog {
                confirm,
                cancel: Some(cancel),
                notice: None,
            },
        );
    }

    /// Ask a question. `confirm` is called with what was typed, if the
    /// dialog has a field, each time the confirm button is pressed; the
    /// dialog stays open until its owner calls [`App::close_dialog`].
    /// `cancel` is called if the user backs out, or if another part of
    /// the program needs the dialog for a question of its own.
    #[track_caller]
    pub fn open_dialog(
        &self,
        spec: DialogSpec,
        confirm: impl FnMut(&Rc<App>, String) + 'static,
        cancel: impl FnOnce(&Rc<App>) + 'static,
    ) {
        let owner = owner_of(Location::caller().file());
        self.open_as(owner, spec, Box::new(confirm), Box::new(cancel));
    }

    /// A question with one answer that matters: closes on either button
    /// and calls `confirmed` only for the confirming one.
    #[track_caller]
    pub fn ask(&self, spec: DialogSpec, confirmed: impl FnOnce(&Rc<App>) + 'static) {
        let owner = owner_of(Location::caller().file());
        let mut confirmed = Some(confirmed);
        self.open_as(
            owner,
            spec,
            Box::new(move |app, _| {
                app.dismiss();
                if let Some(confirmed) = confirmed.take() {
                    confirmed(app);
                }
            }),
            Box::new(|_| {}),
        );
    }

    /// Change what the open dialog says without closing it.
    #[track_caller]
    pub fn update_dialog(&self, change: impl FnOnce(&mut DialogSpec)) {
        if self.mine(owner_of(Location::caller().file()), "change") {
            let state = self.window.global::<DialogState>();
            let mut spec = state.get_spec();
            change(&mut spec);
            state.set_spec(spec);
        }
    }

    #[track_caller]
    pub fn set_dialog_busy(&self, busy: bool) {
        if self.mine(owner_of(Location::caller().file()), "change") {
            self.window.global::<DialogState>().set_busy(busy);
        }
    }

    /// Empty the dialog's field and put the caret back in it.
    #[track_caller]
    pub fn wipe_dialog_field(&self) {
        if self.mine(owner_of(Location::caller().file()), "empty") {
            self.wipe();
        }
    }

    #[track_caller]
    pub fn set_dialog_countdown(&self, text: &str) {
        if self.owns(owner_of(Location::caller().file())) {
            self.window
                .global::<DialogState>()
                .set_countdown(text.into());
        }
    }

    /// Whether the caller's own dialog is open.
    #[track_caller]
    pub fn dialog_open(&self) -> bool {
        self.owns(owner_of(Location::caller().file()))
    }

    /// Close the caller's own dialog. Someone else's is left alone, and
    /// so the caller should know its own is open: see [`App::mine`].
    #[track_caller]
    pub fn close_dialog(&self) {
        if self.mine(owner_of(Location::caller().file()), "close") {
            self.dismiss();
        }
    }

    /// Close the dialog as Escape does, running its cancel handler.
    pub fn cancel_dialog(self: &Rc<App>) {
        let open = self.dialog.borrow_mut().take();
        self.dialog_owner.set(None);
        self.window.global::<DialogState>().set_open(false);
        self.wipe();
        self.window.invoke_focus_sidebar();
        if let Some(cancel) = open.and_then(|mut open| open.cancel.take()) {
            cancel(self);
        }
        self.next_notice();
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
        state.set_generation(state.get_generation().wrapping_add(1));
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

    /// Store one setting. It is in effect in memory at once and written
    /// to the file by the worker. False, and a dialog, if it cannot be.
    pub fn set(self: &Rc<App>, section: &str, key: &str, value: Value) -> bool {
        let stored = self.store(section, key, value);
        if let Err(problem) = &stored {
            eprintln!("sengi-settings: {problem}");
            self.tell("The setting could not be saved", problem);
        }
        stored.is_ok()
    }

    fn store(&self, section: &str, key: &str, value: Value) -> Result<(), String> {
        let value = settings::check(section, key, &value)
            .map_err(|why| format!("{section}.{key} {why}"))?;
        self.catch_up();
        if self.settings.borrow().get(section, key).as_ref() == Some(&value) {
            return Ok(());
        }
        if self.settings.borrow().user_path().is_none() {
            return Err("there is no home directory to keep settings in".to_string());
        }
        let change = (section.to_string(), key.to_string(), value);
        self.settings.borrow_mut().hold(&change);
        let number = self.changes_made.get() + 1;
        self.changes_made.set(number);
        self.unwritten.borrow_mut().push((number, change.clone()));
        self.applier.request(Job {
            changes: vec![(number, change)],
            ..Job::default()
        });
        Ok(())
    }

    /// Show the file as it is now, if it is no longer the one that was
    /// read: its owner may have edited it, and the worker writes it.
    fn catch_up(&self) {
        let mut settings = self.settings.borrow_mut();
        if settings.catch_up() {
            for (_, change) in self.unwritten.borrow().iter() {
                settings.hold(change);
            }
        }
    }

    /// The worker has dealt with every change up to this one.
    fn landed(&self, upto: u64) {
        self.unwritten
            .borrow_mut()
            .retain(|(number, _)| *number > upto);
    }

    /// Read the settings files again, after a write that failed.
    fn reload(&self) {
        let mut settings = Settings::load(&self.dirs).0;
        for (_, change) in self.unwritten.borrow().iter() {
            settings.hold(change);
        }
        *self.settings.borrow_mut() = settings;
    }

    /// Run `task` on the worker once every change made so far has been
    /// written and applied.
    pub fn after_saved(&self, task: impl FnOnce() + Send + 'static) {
        self.applier.request(Job {
            after: vec![Box::new(task)],
            ..Job::default()
        });
    }

    /// Say something that needs no answer. If another part of the
    /// program has the dialog, it is said when that is done with.
    #[track_caller]
    pub fn tell(&self, title: &str, body: &str) {
        let notice = Notice {
            owner: owner_of(Location::caller().file()),
            title: title.to_string(),
            body: body.to_string(),
        };
        if admit(self.holder(), notice.owner, true) == Admit::Wait {
            let mut waiting = self.notices.borrow_mut();
            if waiting.len() < MAX_NOTICES && !waiting.contains(&notice) {
                waiting.push_back(notice);
            }
        } else {
            self.show_notice(notice);
        }
    }

    /// Write labwc's files from the settings and have it read them, on
    /// the worker. Problems come back in a dialog.
    pub fn apply_to_session(&self) {
        self.applier.request(Job {
            session: true,
            ..Job::default()
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
    use std::path::PathBuf;

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
    fn a_burst_of_requests_is_a_few_applies_and_ends_on_the_last() {
        use std::sync::mpsc;
        let (started, has_started) = mpsc::channel();
        let (go, gate) = mpsc::channel::<()>();
        let done = Arc::new(Mutex::new(Vec::new()));
        let latest = Latest::spawn("test", |waiting: &mut u32, newer| *waiting = newer, {
            let done = done.clone();
            move |value| {
                let _ = started.send(());
                // The first apply is held up until the burst is over.
                let _ = gate.recv();
                done.lock().unwrap().push(value);
            }
        });
        latest.request(0);
        has_started.recv_timeout(Duration::from_secs(5)).unwrap();
        for value in 1..100 {
            latest.request(value);
        }
        drop(go);
        assert!(latest.finish(Duration::from_secs(5)));
        // One was under way; the other 99 became one.
        assert_eq!(*done.lock().unwrap(), [0, 99]);
        // Nothing is taken after the end.
        latest.request(100);
        assert!(latest.finish(Duration::from_millis(10)));
        assert_eq!(done.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_waiting_job_keeps_every_change_in_order_and_every_errand() {
        let ran = Arc::new(Mutex::new(Vec::new()));
        let errand = |name: &'static str| -> Box<dyn FnOnce() + Send> {
            let ran = ran.clone();
            Box::new(move || ran.lock().unwrap().push(name))
        };
        let change = |number: u64, seconds| {
            let change = ("power".to_string(), "lock-after".to_string(), seconds);
            (number, change)
        };
        let mut waiting = Job {
            changes: vec![change(1, Value::Number(60))],
            after: vec![errand("first")],
            ..Job::default()
        };
        merge_jobs(
            &mut waiting,
            Job {
                changes: vec![change(2, Value::Number(120))],
                session: true,
                after: vec![errand("second")],
            },
        );
        // An errand alone does not undo what is owed.
        merge_jobs(&mut waiting, Job::default());
        assert_eq!(
            waiting.changes,
            [change(1, Value::Number(60)), change(2, Value::Number(120))]
        );
        assert!(waiting.session);
        for task in waiting.after {
            task();
        }
        assert_eq!(*ran.lock().unwrap(), ["first", "second"]);
    }

    #[test]
    fn what_was_registered_runs_when_the_process_is_told_to_end() {
        let ran = Arc::new(Mutex::new(Vec::new()));
        let says = |word: &'static str| {
            let ran = ran.clone();
            move || ran.lock().unwrap().push(word)
        };
        on_terminate(says("settings"));
        on_terminate(says("later"));
        // Registered last and done first, as the display's undoing is.
        on_terminate_first(says("display"));
        terminate();
        assert!(terminating());
        assert_eq!(*ran.lock().unwrap(), ["display", "settings", "later"]);
    }

    #[test]
    fn a_dialog_belongs_to_the_module_that_opened_it() {
        assert_eq!(
            owner_of("crates/sengi-settings/src/network/page.rs"),
            "network"
        );
        assert_eq!(
            owner_of("crates/sengi-settings/src/network/mod.rs"),
            "network"
        );
        assert_eq!(
            owner_of("crates/sengi-settings/src/display/page.rs"),
            "display"
        );
        assert_eq!(owner_of("crates/sengi-settings/src/power.rs"), "power.rs");
        assert_eq!(owner_of("src/app.rs"), "app.rs");
        assert_eq!(owner_of("elsewhere.rs"), "elsewhere.rs");
        // Where this very call is written.
        assert_eq!(owner_of(Location::caller().file()), "app.rs");
    }

    #[test]
    fn the_one_dialog_is_not_taken_from_under_its_owner() {
        let question = Some(("network", false));
        let notice = Some(("network", true));
        // Free: anything shows.
        assert_eq!(admit(None, "display", false), Admit::Show);
        assert_eq!(admit(None, "display", true), Admit::Show);
        // The owner goes on to its next question, or to its next news.
        assert_eq!(admit(question, "network", false), Admit::Show);
        assert_eq!(admit(notice, "network", true), Admit::Show);
        assert_eq!(admit(notice, "network", false), Admit::Show);
        // Its news does not take the place of its own question: a late
        // refusal must not close a passphrase half typed.
        assert_eq!(admit(question, "network", true), Admit::Wait);
        // Someone else's news waits, behind a question or a notice.
        assert_eq!(admit(question, "bluetooth", true), Admit::Wait);
        assert_eq!(admit(notice, "app.rs", true), Admit::Wait);
        // Someone else's question cannot wait, but the one it displaces
        // is backed out, never answered: a keep-or-revert dialog that
        // loses its place goes back, it does not keep.
        assert_eq!(
            admit(Some(("display", false)), "bluetooth", false),
            Admit::AfterCancel
        );
        assert_eq!(admit(notice, "bluetooth", false), Admit::AfterRequeue);
    }

    #[test]
    fn a_close_that_reaches_no_dialog_is_told_apart() {
        let question = Some(("network", false));
        assert_eq!(reach(question, "network"), Reach::Mine);
        assert_eq!(reach(Some(("network", true)), "network"), Reach::Mine);
        assert_eq!(reach(question, "bluetooth"), Reach::Missed(Some("network")));
        assert_eq!(reach(None, "network"), Reach::Missed(None));
        assert_eq!(
            missed_words("close", "bluetooth", Some("network")),
            "bluetooth tried to close the dialog, which is network's"
        );
        assert_eq!(
            missed_words("close", "network", None),
            "network tried to close a dialog that is not open"
        );
    }

    /// The names of the functions that only work on the caller's own
    /// dialog, and of the two that make a dialog the caller's.
    const TOUCHES: [&str; 6] = [
        ".close_dialog(",
        ".update_dialog(",
        ".set_dialog_busy(",
        ".wipe_dialog_field(",
        ".set_dialog_countdown(",
        ".dialog_open(",
    ];
    const OPENS: [&str; 2] = [".open_dialog(", ".ask("];

    fn sources(dir: &std::path::Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(path);
            }
        }
    }

    /// A dialog is owned by the module its call is written in. A page
    /// that opened its dialog in one module and closed it from another,
    /// say from a helper moved elsewhere, would have every close ignored.
    #[test]
    fn whoever_changes_or_closes_a_dialog_is_a_module_that_opens_one() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        sources(&src, &mut files);
        assert!(files.len() > 10);
        let (mut openers, mut touchers) = (Vec::new(), Vec::new());
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap();
            let inside = file.strip_prefix(&src).unwrap().to_str().unwrap();
            let owner = inside.split('/').next().unwrap().to_string();
            if owner == "app.rs" {
                continue;
            }
            if OPENS.iter().any(|call| text.contains(call)) {
                openers.push(owner.clone());
            }
            if TOUCHES.iter().any(|call| text.contains(call)) {
                touchers.push((owner, inside.to_string()));
            }
        }
        // The pages that ask questions are found at all.
        for page in ["network", "bluetooth", "display"] {
            assert!(openers.iter().any(|owner| owner == page), "{page}");
        }
        for (owner, file) in touchers {
            assert!(
                openers.contains(&owner),
                "{file} changes or closes a dialog, but nothing in {owner} opens one"
            );
        }
    }

    #[test]
    fn every_page_has_an_icon() {
        for page in Page::ALL {
            assert!(!icon(page).is_empty());
        }
    }
}
