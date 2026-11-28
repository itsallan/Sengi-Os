//! The store's behaviour: loading the list, the pages and the search,
//! installing and removing, and drawing icons. Flatpak is only ever run on
//! a thread of its own; the window is touched only from the event loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};

use sengi_apps::icons::{Decoder, Pixels};
use sengi_text::progress::Progress;

use crate::catalog::App;
use crate::featured::{self, Featured, Item, Page};
use crate::flatpak::{Cancel, Error, Flatpak};
use crate::icons;
use crate::nav::{self, Grid, Key};
use crate::online::{self, Retry};
use crate::problem::Problem;
use crate::rows::{Refused, Rows, State};
use crate::ui::{Card, RowState, Store, StorePage, StoreWindow, Tile};

/// The most apps shown for one search.
const SHOWN: usize = 48;

/// Logical pixels per side an icon is drawn at: the token store.app-icon.
const ICON_SIZE: f32 = 48.0;

/// The most pixels per side an icon is decoded to; Flathub's largest.
const MAX_ICON_PIXELS: u32 = 128;

/// How long one icon may take: more than the bar gives a tray icon, because
/// a first start decodes while flatpak is writing to the disk.
const DECODE_WAIT: Duration = Duration::from_secs(3);

/// Said under the page while a list from last time is brought up to date.
const UPDATING: &str = "Updating the list of apps";

/// The longest search that is looked at.
const MAX_QUERY: usize = 64;

/// The soonest the list is asked for again after a failed try because the
/// user typed or moved, unless the search was empty before.
const RETRY_AFTER: Duration = Duration::from_secs(30);

const BUSY: &str = "Another app is being installed or removed. Try again when it is done.";

/// What is asked before an app is removed.
const REMOVE_BODY: &str = "Its settings and data are kept, and are used again if you install \
    it again. Shared parts that no other app uses are removed too.";

/// A removal that waits to be confirmed.
struct Asking {
    id: String,
    name: String,
}

/// Why an open app is not removed. Flatpak would remove it and leave it
/// running, and removing the same app again before it is closed then fails
/// half way.
fn still_open(name: &str) -> String {
    format!("{name} is open. Close it, then remove it.")
}

#[derive(Clone, Default)]
struct Message {
    text: String,
    error: bool,
}

impl Message {
    fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }
}

#[derive(Default)]
struct Data {
    catalog: Vec<App>,
    featured: Featured,
    page: Page,
    query: String,
    /// Why the list could not be had or brought up to date.
    problem: Option<Problem>,
    loading: bool,
    rows: Rows,
    icons: icons::Cache,
    /// Pixels per side the icons are decoded to, for this screen.
    icon_pixels: u32,
    decoding: bool,
    /// How far the install under way has got, as far as flatpak says.
    progress: Option<Progress>,
    /// The app being installed, and how to stop it.
    stop: Option<(String, Cancel)>,
    asking: Option<Asking>,
    message: Message,
    /// The message is why an install failed, and stays until the user
    /// does something else.
    sticky: bool,
    /// When the list was last asked for, so that typing offline does not
    /// ask again at every key.
    last_attempt: Option<Instant>,
    retry: Retry,
}

/// Turns an icon file into pixels of the size asked for.
type Decode = Box<dyn Fn(&Path, u32) -> Option<Pixels> + Send + Sync>;

pub struct Context {
    data: Mutex<Data>,
    flatpak: Flatpak,
    decode: Decode,
    data_home: PathBuf,
    proc_net: PathBuf,
    /// The window is closed: nothing new is started.
    closing: AtomicBool,
    /// The last removal's thread, which is waited for before exiting.
    removal: Mutex<Option<JoinHandle<()>>>,
    /// None in tests, which have no window.
    window: Option<slint::Weak<StoreWindow>>,
}

pub type Shared = Arc<Context>;

/// What one card needs, in a form that may cross to the event loop.
struct CardData {
    id: String,
    name: String,
    summary: String,
    glyph: String,
    letter: String,
    icon: Option<Pixels>,
    state: State,
    progress: Option<Progress>,
    /// The install was told to stop and has not yet.
    cancelling: bool,
    removable: bool,
    /// Shown by name only and not to be installed: there is no list.
    unavailable: bool,
}

struct View {
    page: StorePage,
    title: String,
    tiles: Vec<(String, String)>,
    cards: Vec<CardData>,
    hint: String,
    banner: String,
    banner_retry: bool,
    banner_busy: bool,
    message: Message,
    /// The question to confirm a removal, or nothing.
    remove_title: String,
    remove_body: String,
    /// Where the keys are to be, when the page changed.
    current: Option<i32>,
}

fn to_image(pixels: &Pixels) -> Image {
    Image::from_rgba8_premultiplied(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &pixels.data,
        pixels.size,
        pixels.size,
    ))
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn apply(window: &StoreWindow, view: View) {
    let cards: Vec<Card> = view
        .cards
        .iter()
        .map(|card| Card {
            id: SharedString::from(card.id.as_str()),
            name: card.name.as_str().into(),
            summary: card.summary.as_str().into(),
            glyph: card.glyph.as_str().into(),
            letter: card.letter.as_str().into(),
            icon: card.icon.as_ref().map(to_image).unwrap_or_default(),
            has_icon: card.icon.is_some(),
            percent: card
                .progress
                .as_ref()
                .and_then(|p| p.percent)
                .map_or(-1, i32::from),
            detail: card
                .progress
                .as_ref()
                .map(Progress::detail)
                .unwrap_or_default()
                .into(),
            pace: card
                .progress
                .as_ref()
                .map(Progress::pace)
                .unwrap_or_default()
                .into(),
            unavailable: card.unavailable,
            cancelling: card.cancelling,
            removable: card.removable,
            state: match card.state {
                State::Available => RowState::Available,
                State::Installing => RowState::Installing,
                State::Installed => RowState::Installed,
                State::Removing => RowState::Removing,
                State::Failed => RowState::Failed,
            },
        })
        .collect();
    let tiles: Vec<Tile> = view
        .tiles
        .iter()
        .map(|(name, icon)| Tile {
            name: name.as_str().into(),
            icon: icon.as_str().into(),
        })
        .collect();
    let store = window.global::<Store>();
    let total = i32::try_from(tiles.len() + cards.len()).unwrap_or(i32::MAX);
    store.set_page(view.page);
    // The tiles never change while they are shown, and are left alone so
    // that they are not made again under the pointer.
    if store.get_tiles().row_count() != tiles.len() {
        store.set_tiles(model(tiles));
    }
    store.set_cards(model(cards));
    store.set_title(view.title.into());
    store.set_hint(view.hint.into());
    store.set_banner(view.banner.into());
    store.set_banner_retry(view.banner_retry);
    store.set_banner_busy(view.banner_busy);
    store.set_message(view.message.text.into());
    store.set_message_is_error(view.message.error);
    // The words stay when the question goes, so that the dialog does not
    // change, and take the keyboard back, while it is being closed.
    let asking = !view.remove_title.is_empty();
    if asking {
        store.set_remove_body(view.remove_body.into());
        store.set_remove_title(view.remove_title.into());
    }
    store.set_asking(asking);
    match view.current {
        Some(current) => store.set_current(current.min(total - 1)),
        None if store.get_current() >= total => store.set_current(total - 1),
        None => {}
    }
}

impl Context {
    fn new(
        flatpak: Flatpak,
        data_home: PathBuf,
        window: Option<slint::Weak<StoreWindow>>,
        decode: Decode,
    ) -> Self {
        Self {
            data: Mutex::new(Data {
                featured: Featured::builtin(),
                icon_pixels: ICON_SIZE as u32,
                ..Data::default()
            }),
            flatpak,
            decode,
            data_home,
            proc_net: PathBuf::from(online::PROC_NET),
            closing: AtomicBool::new(false),
            removal: Mutex::new(None),
            window,
        }
    }

    fn post(&self, v: View) {
        if let Some(window) = &self.window {
            let _ = window.upgrade_in_event_loop(move |window| apply(&window, v));
        }
    }

    /// A note from a thread of ours, which does not wipe out the reason an
    /// install failed.
    fn note(data: &mut Data, message: Message) {
        if !data.sticky {
            data.message = message;
        }
    }

    fn lock(&self) -> MutexGuard<'_, Data> {
        // A thread that panicked has left nothing half done that matters.
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The cards on show now.
fn items(data: &Data) -> Vec<Item> {
    featured::browse(&data.featured, &data.catalog, &data.query, data.page, SHOWN)
}

fn searching(data: &Data) -> bool {
    !data.query.trim().is_empty()
}

/// How many category tiles are on show: they are on the start page only.
fn tile_count(data: &Data) -> usize {
    if data.page == Page::Start && !searching(data) {
        data.featured.groups.len()
    } else {
        0
    }
}

/// The IDs of the apps on show.
fn shown_ids(data: &Data) -> Vec<String> {
    items(data)
        .into_iter()
        .map(|item| match item {
            Item::App(index) => data.catalog[index].id.clone(),
            Item::Offline { id, .. } => id,
        })
        .collect()
}

/// The banner over the page: its words, whether it offers Retry, and
/// whether it says work is going on.
fn banner(data: &Data) -> (String, bool, bool) {
    match data.problem {
        Some(problem) => {
            let text = if data.catalog.is_empty() {
                problem.banner()
            } else {
                problem.stale()
            };
            (text.to_string(), !data.loading, data.loading)
        }
        None if data.loading && data.catalog.is_empty() => (
            "Getting the list of apps. The first time can take a few minutes.".to_string(),
            false,
            true,
        ),
        None => (String::new(), false, false),
    }
}

fn view(data: &Data, current: Option<i32>) -> View {
    let cards: Vec<CardData> = items(data)
        .into_iter()
        .map(|item| {
            let (id, name, summary, unavailable) = match item {
                Item::App(index) => {
                    let app = &data.catalog[index];
                    (app.id.clone(), app.name.clone(), app.summary.clone(), false)
                }
                Item::Offline { id, name } => (id, name, String::new(), true),
            };
            let state = data.rows.state(&id);
            let cancelling = data
                .stop
                .as_ref()
                .is_some_and(|(stopped, stop)| *stopped == id && stop.cancelled());
            CardData {
                cancelling,
                removable: data.rows.may_remove(&id),
                glyph: data
                    .featured
                    .group_of(&id)
                    .map(|g| g.icon.clone())
                    .unwrap_or_default(),
                letter: sengi_text::initial(&name),
                icon: data.icons.get(&id).cloned(),
                progress: (state == State::Installing && !cancelling)
                    .then(|| data.progress.clone())
                    .flatten(),
                state,
                id,
                name,
                summary,
                unavailable,
            }
        })
        .collect();
    let tiles = data
        .featured
        .groups
        .iter()
        .take(tile_count(data))
        .map(|g| (g.name.clone(), g.icon.clone()))
        .collect();
    let query = data.query.trim();
    let (page, title) = match data.page {
        _ if searching(data) => (
            StorePage::Search,
            format!("Results for \u{201c}{query}\u{201d}"),
        ),
        Page::Start => (StorePage::Start, String::new()),
        Page::Category(index) => (
            StorePage::Category,
            data.featured
                .groups
                .get(index)
                .map(|g| g.name.clone())
                .unwrap_or_default(),
        ),
    };
    let hint = if !cards.is_empty() {
        String::new()
    } else if searching(data) && data.catalog.is_empty() {
        format!("No recommended app is named \u{201c}{query}\u{201d}.")
    } else if searching(data) {
        format!("No app is named \u{201c}{query}\u{201d}.")
    } else {
        "Flathub has none of these apps right now.".to_string()
    };
    let (banner, banner_retry, banner_busy) = banner(data);
    View {
        page,
        title,
        tiles,
        cards,
        hint,
        banner,
        banner_retry,
        banner_busy,
        message: data.message.clone(),
        remove_title: data
            .asking
            .as_ref()
            .map(|asking| format!("Remove {}?", asking.name))
            .unwrap_or_default(),
        remove_body: data
            .asking
            .as_ref()
            .map(|_| REMOVE_BODY.to_string())
            .unwrap_or_default(),
        current,
    }
}

/// Draws the window again from the data, from any thread.
fn render(shared: &Shared, current: Option<i32>) {
    let v = view(&shared.lock(), current);
    shared.post(v);
    decode_icons(shared);
}

fn set_message(shared: &Shared, message: Message) {
    shared.lock().message = message;
    render(shared, None);
}

/// Decodes the icons of the apps on show that are not known yet, a few at a
/// time on a thread of their own, drawing the page as they come. Without
/// appstream data there is nothing to look in, and nothing is noted.
fn decode_icons(shared: &Shared) {
    {
        let mut data = shared.lock();
        if data.decoding {
            return;
        }
        data.decoding = true;
    }
    let shared = shared.clone();
    let spawned = std::thread::Builder::new()
        .name("store-icons".into())
        .spawn({
            let shared = shared.clone();
            move || loop {
                let stamp = icons::stamp(&shared.data_home);
                let (wanted, stamp, pixels) = {
                    let mut data = shared.lock();
                    let wanted: Vec<String> = match &stamp {
                        Some(stamp) => shown_ids(&data)
                            .into_iter()
                            .filter(|id| data.icons.wanted(id, stamp))
                            .take(8)
                            .collect(),
                        None => Vec::new(),
                    };
                    match stamp {
                        Some(stamp) if !wanted.is_empty() => (wanted, stamp, data.icon_pixels),
                        _ => {
                            data.decoding = false;
                            return;
                        }
                    }
                };
                let aliases = aliases_for(&shared, &wanted, &stamp);
                let decoded: Vec<(String, Option<Pixels>, bool)> = wanted
                    .into_iter()
                    .map(|id| {
                        let files = icons::find(&shared.data_home, &id, &aliases);
                        let icon = files.iter().find_map(|path| (shared.decode)(path, pixels));
                        (id, icon, !files.is_empty())
                    })
                    .collect();
                let (v, changed) = {
                    let mut data = shared.lock();
                    let mut changed = false;
                    for (id, icon, there) in decoded {
                        changed |= icon.is_some();
                        data.icons.note(&id, &stamp, icon, there);
                    }
                    (view(&data, None), changed)
                };
                if changed {
                    shared.post(v);
                }
            }
        });
    if spawned.is_err() {
        shared.lock().decoding = false;
    }
}

/// The other names of icons: read from the appstream data once, and only
/// when an app has no picture under its own ID.
fn aliases_for(shared: &Shared, wanted: &[String], stamp: &str) -> Arc<icons::Aliases> {
    if let Some(known) = shared.lock().icons.aliases(stamp) {
        return known;
    }
    let none = icons::Aliases::new();
    if wanted
        .iter()
        .all(|id| !icons::find(&shared.data_home, id, &none).is_empty())
    {
        return Arc::new(none);
    }
    let read = Arc::new(icons::aliases(&shared.data_home));
    shared.lock().icons.set_aliases(stamp, read.clone());
    read
}

/// Reads the apps of Flathub, from what is on the disk first and then from
/// the network, and what is installed. Run on its own thread.
fn load(shared: Shared) {
    {
        let mut data = shared.lock();
        data.loading = true;
        data.last_attempt = Some(Instant::now());
    }
    render(&shared, None);

    let flatpak = &shared.flatpak;
    let mut failure = flatpak.ensure_remote().err();
    if failure.is_some() {
        // Missing, or a source that is not Flathub: nothing is fetched.
        finish_load(&shared, failure);
        return;
    }
    // Without this a system-wide app would look not installed.
    let installed_known = match flatpak.installed() {
        Ok(installed) => {
            shared.lock().rows.set_installed(installed);
            true
        }
        Err(_) => false,
    };
    // Without the appstream data the list has no real names and no icons,
    // so on a first start it waits for the refresh below.
    if icons::has_appstream(&shared.data_home) {
        if let Ok(apps) = flatpak.list_remote(true) {
            if !apps.is_empty() {
                let mut data = shared.lock();
                data.catalog = apps;
                if data.problem.is_none() {
                    Context::note(&mut data, Message::info(UPDATING));
                }
                drop(data);
                render(&shared, None);
            }
        }
    }

    // New icons and descriptions, then the list that goes with them.
    match flatpak.refresh_appstream() {
        Ok(()) => match flatpak.list_remote(false) {
            Ok(apps) if !apps.is_empty() => {
                shared.lock().catalog = apps;
                failure = None;
            }
            Ok(_) => failure = Some(Error::Failed("the list is empty".to_string())),
            Err(error) => failure = Some(error),
        },
        Err(error) => failure = Some(error),
    }
    // A rough list is better than none.
    if failure.is_some() && shared.lock().catalog.is_empty() {
        if let Ok(apps) = flatpak.list_remote(true) {
            shared.lock().catalog = apps;
        }
    }
    if failure.is_none() && !installed_known {
        // Not an error of the list; install checks again before it acts.
        let mut data = shared.lock();
        Context::note(
            &mut data,
            Message::info("Could not tell which apps are installed yet"),
        );
        data.loading = false;
        data.problem = None;
        data.retry.reset();
        drop(data);
        render(&shared, None);
        return;
    }
    finish_load(&shared, failure);
}

fn finish_load(shared: &Shared, failure: Option<Error>) {
    let routes = online::default_routes(&shared.proc_net);
    {
        let mut data = shared.lock();
        data.loading = false;
        data.problem = failure.as_ref().map(Problem::of);
        match failure {
            None => data.retry.reset(),
            Some(error) => {
                // What flatpak said, for whoever looks into it.
                eprintln!("sengi-store: could not get the list of apps: {error}");
                data.retry.failed(routes);
            }
        }
        // The banner says what there is to say about the list.
        if data.message.text == UPDATING {
            data.message = Message::default();
        }
    }
    render(shared, None);
}

fn start_load(shared: &Shared) {
    {
        let mut data = shared.lock();
        if data.loading || shared.closing.load(Ordering::Relaxed) {
            return;
        }
        data.loading = true;
    }
    let spawned = std::thread::Builder::new()
        .name("store-load".into())
        .spawn({
            let shared = shared.clone();
            move || load(shared)
        });
    if spawned.is_err() {
        shared.lock().loading = false;
    }
}

/// Whether using the Store is a reason to ask for the list again now.
fn due_by_use(data: &Data) -> bool {
    data.catalog.is_empty()
        && !data.loading
        && data
            .last_attempt
            .is_none_or(|at| at.elapsed() >= RETRY_AFTER)
}

/// One look, every few seconds, at whether the world has changed: new
/// appstream data (then there may be icons, and a list), or a network that
/// has come back. `last` is the data seen the time before.
fn look(shared: &Shared, last: &mut Option<String>, elapsed: Duration) {
    let stamp = icons::stamp(&shared.data_home);
    let arrived = stamp != *last;
    *last = stamp;
    let routes = online::default_routes(&shared.proc_net);
    let load = {
        let mut data = shared.lock();
        let failed = !data.loading && data.problem.is_some_and(Problem::network);
        let due = failed && data.retry.due(routes, elapsed);
        due || (arrived && failed && data.catalog.is_empty() && last.is_some())
    };
    if load {
        start_load(shared);
    }
    if arrived {
        render(shared, None);
    }
}

/// Looks, now and then, until the window is closed. It sleeps in between.
fn watch(shared: &Shared) {
    let shared = shared.clone();
    let _ = std::thread::Builder::new()
        .name("store-watch".into())
        .spawn(move || {
            let mut last = icons::stamp(&shared.data_home);
            let mut at = Instant::now();
            while !shared.closing.load(Ordering::Relaxed) {
                std::thread::sleep(online::LOOK_EVERY);
                let elapsed = at.elapsed();
                at = Instant::now();
                look(&shared, &mut last, elapsed);
            }
        });
}

/// The Retry button: asked for by the user, so there is no waiting.
fn retry_now(shared: &Shared) {
    {
        let mut data = shared.lock();
        data.sticky = false;
        data.message = Message::default();
        data.retry.reset();
    }
    start_load(shared);
    render(shared, None);
}

fn search(shared: &Shared, text: &str) {
    let (retry, current) = {
        let mut data = shared.lock();
        let was_empty = !searching(&data);
        data.query = sengi_text::line(text, MAX_QUERY);
        // Whatever went wrong before has been seen by now.
        data.sticky = false;
        if data.message.error {
            data.message = Message::default();
        }
        let first_letter = was_empty && searching(&data);
        let retry = due_by_use(&data) || (first_letter && data.catalog.is_empty() && !data.loading);
        // Return installs the best result; with nothing typed nothing is
        // chosen.
        (retry, if searching(&data) { 0 } else { nav::FIELD })
    };
    if retry {
        start_load(shared);
    }
    render(shared, Some(current));
}

fn open_category(shared: &Shared, index: usize) {
    let retry = {
        let mut data = shared.lock();
        if index >= data.featured.groups.len() {
            return;
        }
        data.page = Page::Category(index);
        data.query.clear();
        data.sticky = false;
        due_by_use(&data)
    };
    if retry {
        start_load(shared);
    }
    render(shared, Some(0));
}

/// From a category to the start page, with its tile chosen. A search is
/// cleared by the field, which then says so through `search`.
fn back(shared: &Shared) {
    let current = {
        let mut data = shared.lock();
        let current = match data.page {
            Page::Category(index) => i32::try_from(index).unwrap_or(nav::FIELD),
            Page::Start => nav::FIELD,
        };
        data.page = Page::Start;
        data.sticky = false;
        current
    };
    render(shared, Some(current));
}

/// Where a key leads in the grids now on show.
fn step(shared: &Shared, current: i32, key: i32, tile_columns: i32, card_columns: i32) -> i32 {
    let Some(key) = Key::from_code(key) else {
        return current;
    };
    let data = shared.lock();
    let columns = |n: i32| usize::try_from(n).unwrap_or(1);
    nav::step(
        &[
            Grid {
                count: tile_count(&data),
                columns: columns(tile_columns),
            },
            Grid {
                count: items(&data).len(),
                columns: columns(card_columns),
            },
        ],
        current,
        key,
    )
}

/// The screen has `factor` physical pixels to a logical one.
fn scaled(shared: &Shared, factor: f32) {
    let pixels = ((ICON_SIZE * factor).round() as u32).clamp(ICON_SIZE as u32, MAX_ICON_PIXELS);
    {
        let mut data = shared.lock();
        if data.icon_pixels == pixels {
            return;
        }
        data.icon_pixels = pixels;
        data.icons = icons::Cache::default();
    }
    render(shared, None);
}

fn install(shared: &Shared, id: &str) {
    let name = {
        let mut data = shared.lock();
        let Some(app) = data.catalog.iter().find(|app| app.id == id) else {
            return;
        };
        let name = app.name.clone();
        match data.rows.begin(id) {
            Ok(()) => {
                data.sticky = false;
                data.message = Message::info(format!("Installing {name}"));
            }
            Err(Refused::Done | Refused::NotYours) => return,
            Err(Refused::Busy) => {
                data.message = Message::info(BUSY);
                drop(data);
                render(shared, None);
                return;
            }
        }
        name
    };
    let stop = Cancel::default();
    shared.lock().stop = Some((id.to_string(), stop.clone()));
    render(shared, None);

    let id = id.to_string();
    let spawned = std::thread::Builder::new()
        .name("store-install".into())
        .spawn({
            let shared = shared.clone();
            let id = id.clone();
            move || run_install(&shared, &id, &name, &stop)
        });
    if spawned.is_err() {
        {
            let mut data = shared.lock();
            data.rows.finish(&id, false);
            data.stop = None;
        }
        set_message(shared, Message::error("Could not start the install"));
    }
}

/// Stops the install of `id`, if it is the one under way. The card says so
/// until flatpak is gone.
fn cancel(shared: &Shared, id: &str) {
    {
        let mut data = shared.lock();
        let Some((_, stop)) = data.stop.as_ref().filter(|(under_way, _)| under_way == id) else {
            return;
        };
        if stop.cancelled() {
            return;
        }
        stop.cancel();
        data.progress = None;
        data.sticky = false;
        let name = name_of(&data, id);
        data.message = Message::info(format!("Cancelling the install of {name}"));
    }
    render(shared, None);
}

/// What an app is called, or its ID if nothing says.
fn name_of(data: &Data, id: &str) -> String {
    if let Some(app) = data.catalog.iter().find(|app| app.id == id) {
        return app.name.clone();
    }
    // Without a list only the recommended names are on show.
    items(data)
        .into_iter()
        .find_map(|item| match item {
            Item::Offline { id: other, name } if other == id => Some(name),
            _ => None,
        })
        .unwrap_or_else(|| id.to_string())
}

/// The Remove button: asks first, unless the app is open. Nothing is
/// removed from here.
fn ask_remove(shared: &Shared, id: &str) {
    {
        let mut data = shared.lock();
        if data.rows.busy() {
            if data.rows.state(id) == State::Installed {
                data.message = Message::info(BUSY);
                drop(data);
                render(shared, None);
            }
            return;
        }
        if !data.rows.may_remove(id) {
            return;
        }
    }
    let id = id.to_string();
    let spawned = std::thread::Builder::new().name("store-ask".into()).spawn({
        let shared = shared.clone();
        move || {
            // When flatpak cannot say, the removal says what is wrong.
            let open = matches!(shared.flatpak.is_running(&id), Ok(true));
            {
                let mut data = shared.lock();
                if data.rows.busy() || !data.rows.may_remove(&id) {
                    return;
                }
                let name = name_of(&data, &id);
                if open {
                    data.sticky = true;
                    data.message = Message::error(still_open(&name));
                } else {
                    data.sticky = false;
                    data.message = Message::default();
                    data.asking = Some(Asking { id, name });
                }
            }
            render(&shared, None);
        }
    });
    if spawned.is_err() {
        set_message(shared, Message::error("Could not start the removal"));
    }
}

/// The question was answered no.
fn keep(shared: &Shared) {
    shared.lock().asking = None;
    render(shared, None);
}

/// The question was answered yes.
fn confirm_remove(shared: &Shared) {
    let (id, name) = {
        let mut data = shared.lock();
        let Some(Asking { id, name }) = data.asking.take() else {
            return;
        };
        match data.rows.begin_removal(&id) {
            Ok(()) => {
                data.sticky = false;
                data.message = Message::info(format!("Removing {name}"));
            }
            Err(refused) => {
                if refused == Refused::Busy {
                    data.message = Message::info(BUSY);
                }
                drop(data);
                render(shared, None);
                return;
            }
        }
        (id, name)
    };
    render(shared, None);
    let spawned = std::thread::Builder::new()
        .name("store-remove".into())
        .spawn({
            let shared = shared.clone();
            let id = id.clone();
            move || run_remove(&shared, &id, &name)
        });
    match spawned {
        Ok(thread) => {
            *shared.removal.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread);
        }
        Err(_) => {
            shared.lock().rows.finish_removal(&id, false);
            set_message(shared, Message::error("Could not start the removal"));
        }
    }
}

/// The removal itself, on its own thread.
fn run_remove(shared: &Shared, id: &str, name: &str) {
    let flatpak = &shared.flatpak;
    // It may have been opened while the question was on the screen.
    if matches!(flatpak.is_running(id), Ok(true)) {
        {
            let mut data = shared.lock();
            data.rows.finish_removal(id, false);
            data.sticky = true;
            data.message = Message::error(still_open(name));
        }
        render(shared, None);
        return;
    }
    let result = flatpak.uninstall(id);
    if result.is_ok() {
        // The app is gone whether or not this works.
        if let Err(error) = flatpak.remove_unused() {
            eprintln!("sengi-store: could not remove unused runtimes: {error}");
        }
    }
    // What is there now: a copy the system has stays, and a removal that
    // failed may have got part of the way.
    let installed = flatpak.installed();
    {
        let mut data = shared.lock();
        data.rows.finish_removal(id, result.is_ok());
        if let Ok(installed) = installed {
            data.rows.set_installed(installed);
        }
        match &result {
            Ok(()) => {
                data.sticky = false;
                data.message = Message::info(format!("{name} was removed."));
            }
            Err(error) => {
                eprintln!("sengi-store: could not remove {id}: {error}");
                data.sticky = true;
                data.message = Message::error(Problem::of(error).remove(name));
            }
        }
    }
    render(shared, None);
}

/// Starts an installed app. The Store does not wait for it.
fn open(shared: &Shared, id: &str) {
    let (known, name) = {
        let data = shared.lock();
        let name = data
            .catalog
            .iter()
            .find(|app| app.id == id)
            .map_or_else(|| id.to_string(), |app| app.name.clone());
        (data.rows.may_open(id), name)
    };
    if !known {
        return;
    }
    match shared.flatpak.open(id) {
        Ok(()) => {
            let mut data = shared.lock();
            data.sticky = false;
            data.message = Message::info(format!("Opening {name}"));
        }
        Err(error) => {
            eprintln!("sengi-store: could not open {id}: {error}");
            let mut data = shared.lock();
            data.sticky = true;
            data.message = Message::error(format!("Could not open {name}."));
        }
    }
    render(shared, None);
}

/// The install itself, on its own thread.
fn run_install(shared: &Shared, id: &str, name: &str, stop: &Cancel) {
    // The list of what is installed may have failed to load, or the app
    // may have come from elsewhere since. A second copy is not made.
    let result = match shared.flatpak.installed() {
        Ok(installed) if installed.all.contains(id) => {
            shared.lock().rows.set_installed(installed);
            Ok(true)
        }
        Ok(_) => {
            let report = |progress: Progress| {
                let mut data = shared.lock();
                if !stop.cancelled() {
                    data.progress = Some(progress);
                    drop(data);
                    render(shared, None);
                }
            };
            shared
                .flatpak
                .install_with(id, Some(&report), stop)
                .map(|()| false)
        }
        Err(error) => Err(error),
    };
    // A cancel may come when flatpak has done its work already, so it is
    // asked what is there now.
    let arrived = (result.is_err() && stop.cancelled())
        .then(|| shared.flatpak.installed().ok())
        .flatten()
        .filter(|installed| installed.all.contains(id));
    {
        let mut data = shared.lock();
        data.progress = None;
        data.stop = None;
        let done = |data: &mut Data, text: String| {
            data.rows.finish(id, true);
            data.sticky = false;
            data.message = Message::info(text);
        };
        let late = format!("{name} was installed before it could be cancelled.");
        match &result {
            Ok(true) => done(&mut data, format!("{name} is installed already")),
            Ok(false) if stop.cancelled() => done(&mut data, late),
            Ok(false) => done(
                &mut data,
                format!("{name} is installed. It is in the launcher now."),
            ),
            Err(_) if arrived.is_some() => done(&mut data, late),
            // Cancelled, or the window was closed.
            Err(error) if stop.cancelled() || *error == Error::Cancelled => {
                data.rows.stopped(id);
                data.sticky = false;
                data.message = Message::info(format!("Cancelled. {name} was not installed."));
            }
            Err(error) => {
                eprintln!("sengi-store: could not install {id}: {error}");
                data.rows.finish(id, false);
                data.sticky = true;
                data.message = Message::error(Problem::of(error).install(name));
            }
        }
        if let Some(installed) = arrived {
            data.rows.set_installed(installed);
        }
    }
    render(shared, None);
}

/// What is done when the window is closed: an install under way is stopped,
/// as Cancel would, and a removal is waited for.
fn close(shared: &Shared) {
    shared.closing.store(true, Ordering::Relaxed);
    shared.flatpak.stop_all();
    let removal = shared
        .removal
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(thread) = removal {
        let _ = thread.join();
    }
}

/// Makes the store out of a window and starts loading. When the window is
/// closed, an install under way is stopped and a removal is finished.
pub fn run(
    window: StoreWindow,
    flatpak: Flatpak,
    data_home: PathBuf,
) -> Result<(), slint::PlatformError> {
    run_with_query(window, flatpak, data_home, "")
}

/// As [`run`], with the search field already holding `query` (`--search`).
pub fn run_with_query(
    window: StoreWindow,
    flatpak: Flatpak,
    data_home: PathBuf,
    query: &str,
) -> Result<(), slint::PlatformError> {
    let query = sengi_text::line(query, MAX_QUERY);
    let decoder = Decoder::default();
    let shared: Shared = Arc::new(Context::new(
        flatpak,
        data_home,
        Some(window.as_weak()),
        Box::new(move |path, pixels| decoder.decode_file(path, pixels, DECODE_WAIT)),
    ));
    let store = window.global::<Store>();
    store.set_start_query(query.as_str().into());
    store.on_search({
        let shared = shared.clone();
        move |text| search(&shared, &text)
    });
    store.on_open_category({
        let shared = shared.clone();
        move |index| open_category(&shared, usize::try_from(index).unwrap_or(usize::MAX))
    });
    store.on_back({
        let shared = shared.clone();
        move || back(&shared)
    });
    store.on_retry({
        let shared = shared.clone();
        move || retry_now(&shared)
    });
    store.on_step({
        let shared = shared.clone();
        move |current, key, tiles, cards| step(&shared, current, key, tiles, cards)
    });
    store.on_scaled({
        let shared = shared.clone();
        move |factor| scaled(&shared, factor)
    });
    store.on_install({
        let shared = shared.clone();
        move |id| install(&shared, &id)
    });
    store.on_open({
        let shared = shared.clone();
        move |id| open(&shared, &id)
    });
    store.on_cancel({
        let shared = shared.clone();
        move |id| cancel(&shared, &id)
    });
    store.on_ask_remove({
        let shared = shared.clone();
        move |id| ask_remove(&shared, &id)
    });
    store.on_confirm_remove({
        let shared = shared.clone();
        move || confirm_remove(&shared)
    });
    store.on_keep({
        let shared = shared.clone();
        move || keep(&shared)
    });
    scaled(&shared, window.window().scale_factor());
    render(&shared, Some(nav::FIELD));
    start_load(&shared);
    if !query.is_empty() {
        search(&shared, &query);
    }
    watch(&shared);
    let result = window.run();
    close(&shared);
    result
}

#[cfg(test)]
mod tests;
