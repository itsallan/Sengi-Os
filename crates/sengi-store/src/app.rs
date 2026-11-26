//! The store's behaviour: loading the list, searching it, installing, and
//! drawing icons. Flatpak is only ever run on a thread of its own; the
//! window is touched only from the event loop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use slint::{
    ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};

use sengi_apps::icons::{Decoder, Pixels, DECODE_TIMEOUT};

use crate::catalog::App;
use crate::featured::{self, Chip, Featured, Item};
use crate::flatpak::{Error, Flatpak};
use crate::icons;
use crate::progress::Progress;
use crate::rows::{Refused, Rows, State};
use crate::ui::{Row, RowState, Store, StoreWindow};

/// The most apps shown for one search.
const SHOWN: usize = 40;

/// Pixels per side the icons are decoded to; Flathub's are 64.
const ICON_PIXELS: u32 = 64;

/// The longest search that is looked at.
const MAX_QUERY: usize = 64;

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
    chip: Chip,
    /// Flathub was asked for and did not answer, and there is no list
    /// from an earlier time to go by.
    unreachable: bool,
    loading: bool,
    query: String,
    rows: Rows,
    icons: HashMap<String, Option<Pixels>>,
    decoding: bool,
    /// How far the install under way has got, as far as flatpak says.
    progress: Option<Progress>,
    message: Message,
    /// The message is why an install failed, and stays until the user
    /// does something else.
    sticky: bool,
    /// When the list was last asked for, so that typing offline does not
    /// ask again at every key.
    last_attempt: Option<Instant>,
}

/// The soonest the list is asked for again after a failed try, unless the
/// search was empty before.
const RETRY_AFTER: Duration = Duration::from_secs(30);

pub struct Context {
    data: Mutex<Data>,
    flatpak: Flatpak,
    decoder: Decoder,
    data_home: PathBuf,
    /// None in tests, which have no window.
    window: Option<slint::Weak<StoreWindow>>,
}

pub type Shared = Arc<Context>;

/// What one row needs, in a form that may cross to the event loop.
struct RowData {
    id: String,
    name: String,
    summary: String,
    letter: String,
    icon: Option<Pixels>,
    state: State,
    progress: Option<Progress>,
    /// A group's name, not an app.
    heading: bool,
    /// Shown by name only and not to be installed: Flathub is not there.
    unavailable: bool,
}

struct View {
    rows: Vec<RowData>,
    hint: String,
    notice: String,
    message: Message,
    reset: bool,
    /// The row the keys start on when `reset`.
    first: usize,
}

fn to_image(pixels: &Pixels) -> Image {
    Image::from_rgba8_premultiplied(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &pixels.data,
        pixels.size,
        pixels.size,
    ))
}

fn apply(window: &StoreWindow, view: View) {
    let rows: Vec<Row> = view
        .rows
        .iter()
        .map(|row| Row {
            id: SharedString::from(row.id.as_str()),
            name: row.name.as_str().into(),
            summary: row.summary.as_str().into(),
            letter: row.letter.as_str().into(),
            icon: row.icon.as_ref().map(to_image).unwrap_or_default(),
            has_icon: row.icon.is_some(),
            percent: row
                .progress
                .as_ref()
                .and_then(|p| p.percent)
                .map_or(-1, i32::from),
            detail: row
                .progress
                .as_ref()
                .map(Progress::detail)
                .unwrap_or_default()
                .into(),
            heading: row.heading,
            unavailable: row.unavailable,
            state: match row.state {
                State::Available => RowState::Available,
                State::Installing => RowState::Installing,
                State::Installed => RowState::Installed,
                State::Failed => RowState::Failed,
            },
        })
        .collect();
    let store = window.global::<Store>();
    store.set_rows(ModelRc::new(VecModel::from(rows)));
    store.set_hint(view.hint.into());
    store.set_notice(view.notice.into());
    store.set_message(view.message.text.into());
    store.set_message_is_error(view.message.error);
    if view.reset {
        store.set_current(i32::try_from(view.first).unwrap_or(0));
    }
}

impl Context {
    fn new(flatpak: Flatpak, data_home: PathBuf, window: Option<slint::Weak<StoreWindow>>) -> Self {
        Self {
            data: Mutex::new(Data {
                featured: Featured::builtin(),
                ..Data::default()
            }),
            flatpak,
            decoder: Decoder::default(),
            data_home,
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

/// What the list shows now.
fn items(data: &Data) -> Vec<Item> {
    featured::browse(&data.featured, &data.catalog, &data.query, data.chip, SHOWN)
}

/// The IDs of the apps on show.
fn shown_ids(data: &Data) -> Vec<String> {
    items(data)
        .into_iter()
        .filter_map(|item| match item {
            Item::App(index) => Some(data.catalog[index].id.clone()),
            Item::Offline { id, .. } => Some(id),
            Item::Heading(_) => None,
        })
        .collect()
}

fn view(data: &Data, reset: bool) -> View {
    let items = items(data);
    let row = |id: &str, name: &str, summary: &str, unavailable: bool| {
        let state = data.rows.state(id);
        RowData {
            id: id.to_string(),
            name: name.to_string(),
            summary: summary.to_string(),
            letter: sengi_text::initial(name),
            icon: data.icons.get(id).cloned().flatten(),
            state,
            progress: if state == State::Installing {
                data.progress.clone()
            } else {
                None
            },
            heading: false,
            unavailable,
        }
    };
    let rows: Vec<RowData> = items
        .iter()
        .map(|item| match item {
            Item::Heading(name) => RowData {
                id: String::new(),
                name: name.clone(),
                summary: String::new(),
                letter: String::new(),
                icon: None,
                state: State::Available,
                progress: None,
                heading: true,
                unavailable: true,
            },
            Item::App(index) => {
                let app = &data.catalog[*index];
                row(&app.id, &app.name, &app.summary, false)
            }
            Item::Offline { id, name } => row(id, name, "", true),
        })
        .collect();
    let hint = if data.catalog.is_empty() && data.featured.groups.is_empty() {
        if data.loading {
            "Getting the list of apps".to_string()
        } else {
            "The list of apps is not available. Type to try again.".to_string()
        }
    } else if data.query.trim().is_empty() {
        "Type the name of an app".to_string()
    } else if data.chip == Chip::All {
        format!("No app is named \u{201c}{}\u{201d}", data.query.trim())
    } else {
        format!(
            "No recommended app is named \u{201c}{}\u{201d}. Choose All to look at every app.",
            data.query.trim()
        )
    };
    let notice = if data.unreachable && data.catalog.is_empty() && !data.loading {
        "Not connected to Flathub. These apps can be installed once you are online.".to_string()
    } else {
        String::new()
    };
    View {
        first: featured::first(&items),
        rows,
        hint,
        notice,
        message: data.message.clone(),
        reset,
    }
}

/// Draws the window again from the data, from any thread.
fn render(shared: &Shared, reset: bool) {
    let v = view(&shared.lock(), reset);
    shared.post(v);
    decode_icons(shared);
}

fn set_message(shared: &Shared, message: Message) {
    shared.lock().message = message;
    render(shared, false);
}

/// Decodes the icons of the apps on show that have not been tried, a few
/// at a time on a thread of their own, drawing the list as they come.
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
                let wanted: Vec<String> = {
                    let mut data = shared.lock();
                    let wanted: Vec<String> = shown_ids(&data)
                        .into_iter()
                        .filter(|id| !data.icons.contains_key(id))
                        .take(8)
                        .collect();
                    if wanted.is_empty() {
                        data.decoding = false;
                        return;
                    }
                    wanted
                };
                let decoded: Vec<(String, Option<Pixels>)> = wanted
                    .into_iter()
                    .map(|id| {
                        let pixels = icons::find(&shared.data_home, &id).and_then(|path| {
                            shared
                                .decoder
                                .decode_file(&path, ICON_PIXELS, DECODE_TIMEOUT)
                        });
                        (id, pixels)
                    })
                    .collect();
                let v = {
                    let mut data = shared.lock();
                    data.icons.extend(decoded);
                    view(&data, false)
                };
                shared.post(v);
            }
        });
    if spawned.is_err() {
        shared.lock().decoding = false;
    }
}

fn explain(error: &Error) -> String {
    match error {
        Error::Missing => "Flatpak is not installed, so apps cannot be found.".to_string(),
        Error::TimedOut => {
            "Flatpak did not answer in time. Check the network connection.".to_string()
        }
        Error::Failed(text) | Error::Refused(text) => {
            format!("Could not get the list of apps: {text}")
        }
    }
}

/// Reads the apps of Flathub, from what is on the disk first and then from
/// the network, and what is installed. Run on its own thread.
fn load(shared: Shared) {
    {
        let mut data = shared.lock();
        data.loading = true;
        data.last_attempt = Some(Instant::now());
        Context::note(&mut data, Message::info("Getting the list of apps"));
    }
    render(&shared, false);

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
    let cached = if icons::has_appstream(&shared.data_home) {
        flatpak.list_remote(true)
    } else {
        Ok(Vec::new())
    };
    if let Ok(apps) = cached {
        if !apps.is_empty() {
            let mut data = shared.lock();
            data.catalog = apps;
            Context::note(&mut data, Message::info("Updating the list of apps"));
            drop(data);
            render(&shared, false);
        }
    }

    // New icons and descriptions, then the list that goes with them.
    match flatpak.refresh_appstream() {
        Ok(()) => match flatpak.list_remote(false) {
            Ok(apps) if !apps.is_empty() => {
                let mut data = shared.lock();
                data.catalog = apps;
                // Icons that were not there may be now.
                data.icons.retain(|_, pixels| pixels.is_some());
                failure = None;
            }
            Ok(_) => failure = Some(Error::Failed("the list is empty".to_string())),
            Err(error) => failure = Some(error),
        },
        Err(error) => {
            // The refresh failed: a rough list is better than none.
            if shared.lock().catalog.is_empty() {
                if let Ok(apps) = flatpak.list_remote(true) {
                    shared.lock().catalog = apps;
                }
            }
            failure = Some(error);
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
        data.unreachable = false;
        drop(data);
        render(&shared, false);
        return;
    }
    finish_load(&shared, failure);
}

fn finish_load(shared: &Shared, failure: Option<Error>) {
    {
        let mut data = shared.lock();
        data.loading = false;
        data.unreachable = matches!(failure, Some(Error::TimedOut | Error::Failed(_)));
        let message = match failure {
            None => Message::default(),
            Some(error) if data.catalog.is_empty() => Message::error(explain(&error)),
            // The list from last time is good enough to search.
            Some(_) => Message::info("No network: the list may be out of date"),
        };
        Context::note(&mut data, message);
    }
    render(shared, false);
}

fn start_load(shared: &Shared) {
    {
        let mut data = shared.lock();
        if data.loading {
            return;
        }
        data.loading = true;
    }
    let shared = shared.clone();
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

fn search(shared: &Shared, text: &str) {
    let retry = {
        let mut data = shared.lock();
        let was_empty = data.query.trim().is_empty();
        data.query = sengi_text::line(text, MAX_QUERY);
        // Whatever went wrong before has been seen by now.
        data.sticky = false;
        let due = data
            .last_attempt
            .is_none_or(|at| at.elapsed() >= RETRY_AFTER);
        data.catalog.is_empty()
            && !data.loading
            && ((was_empty && !data.query.trim().is_empty()) || due)
    };
    if retry {
        start_load(shared);
    }
    render(shared, true);
}

/// A chip was chosen: 0 is All, 1 Featured, then the groups. Like typing,
/// it asks for the list again if there is none, but no sooner than a
/// search would.
fn choose_chip(shared: &Shared, index: usize) {
    let retry = {
        let mut data = shared.lock();
        data.chip = Chip::from_index(index);
        data.sticky = false;
        data.catalog.is_empty()
            && !data.loading
            && data
                .last_attempt
                .is_none_or(|at| at.elapsed() >= RETRY_AFTER)
    };
    if retry {
        start_load(shared);
    }
    render(shared, true);
}

/// The row an arrow key moves to from `current`, never a heading.
fn step_row(shared: &Shared, current: i32, by: i32) -> i32 {
    let items = items(&shared.lock());
    let current = usize::try_from(current).unwrap_or(0);
    i32::try_from(featured::step(&items, current, by.signum())).unwrap_or(0)
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
            Err(Refused::Done) => return,
            Err(Refused::Busy) => {
                data.message =
                    Message::info("Another app is being installed. Try again when it is done.");
                drop(data);
                render(shared, false);
                return;
            }
        }
        name
    };
    render(shared, false);

    let shared = shared.clone();
    let id = id.to_string();
    let spawned = std::thread::Builder::new()
        .name("store-install".into())
        .spawn({
            let shared = shared.clone();
            let id = id.clone();
            move || run_install(&shared, &id, &name)
        });
    if spawned.is_err() {
        shared.lock().rows.finish(&id, false);
        set_message(&shared, Message::error("Could not start the install"));
    }
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
            let mut data = shared.lock();
            data.sticky = true;
            data.message = Message::error(format!("Could not open {name}: {error}"));
        }
    }
    render(shared, false);
}

/// The install itself, on its own thread.
fn run_install(shared: &Shared, id: &str, name: &str) {
    // The list of what is installed may have failed to load, or the app
    // may have come from elsewhere since. A second copy is not made.
    let result = match shared.flatpak.installed() {
        Ok(installed) if installed.contains(id) => {
            shared.lock().rows.set_installed(installed);
            Ok(true)
        }
        Ok(_) => {
            let report = |progress: Progress| {
                shared.lock().progress = Some(progress);
                render(shared, false);
            };
            shared
                .flatpak
                .install_with(id, Some(&report))
                .map(|()| false)
        }
        Err(error) => Err(Error::Failed(format!(
            "could not check whether it is installed already: {error}"
        ))),
    };
    {
        let mut data = shared.lock();
        data.rows.finish(id, result.is_ok());
        data.progress = None;
        match &result {
            Ok(true) => {
                data.sticky = false;
                data.message = Message::info(format!("{name} is installed already"));
            }
            Ok(false) => {
                data.sticky = false;
                data.message =
                    Message::info(format!("{name} is installed. It is in the launcher now."));
            }
            Err(error) => {
                data.sticky = true;
                data.message = Message::error(format!("Could not install {name}: {error}"));
            }
        }
    }
    render(shared, false);
}

/// Makes the store out of a window and starts loading. When the window is
/// closed, what flatpak is still doing for it is stopped.
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
    let shared: Shared = Arc::new(Context::new(flatpak, data_home, Some(window.as_weak())));
    let store = window.global::<Store>();
    store.set_start_query(query.as_str().into());
    store.on_search({
        let shared = shared.clone();
        move |text| search(&shared, &text)
    });
    store.on_chip_selected({
        let shared = shared.clone();
        move |index| choose_chip(&shared, usize::try_from(index).unwrap_or(0))
    });
    store.on_step_row({
        let shared = shared.clone();
        move |current, by| step_row(&shared, current, by)
    });
    let labels: Vec<SharedString> = Chip::labels(&shared.lock().featured)
        .iter()
        .map(|l| l.as_str().into())
        .collect();
    let half = labels.len().div_ceil(2);
    store.set_chips_first(ModelRc::new(VecModel::from(labels[..half].to_vec())));
    store.set_chips_second(ModelRc::new(VecModel::from(labels[half..].to_vec())));
    store.set_chips(ModelRc::new(VecModel::from(labels)));
    store.on_install({
        let shared = shared.clone();
        move |id| install(&shared, &id)
    });
    store.on_open({
        let shared = shared.clone();
        move |id| open(&shared, &id)
    });
    render(&shared, true);
    start_load(&shared);
    if !query.is_empty() {
        search(&shared, &query);
    }
    let result = window.run();
    shared.flatpak.stop_all();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flatpak::testing::{quick, Fake};

    const LISTING: &str = "org.gimp.GIMP\\tGIMP\\tImages\\norg.x.Y\\tY\\t\\n";

    /// A fake flatpak that answers like the real one for what the store
    /// asks, and fails the install if `install_exit` is not 0.
    fn fake(tag: &str, install_exit: u8, extra: &str) -> Fake {
        Fake::new(
            tag,
            &format!(
                "case \"$*\" in\n\
                 *remotes*) printf 'flathub\\thttps://dl.flathub.org/repo/\\tuser\\n';;\n\
                 *remote-ls*) printf '{LISTING}';;\n\
                 *\" install \"*) echo 'error: no space left' >&2; exit {install_exit};;\n\
                 {extra}\n\
                 esac"
            ),
        )
    }

    fn context(fake: &Fake) -> Shared {
        Arc::new(Context::new(
            fake.flatpak(quick()),
            fake.dir.join("data"),
            None,
        ))
    }

    fn wait_until(shared: &Shared, done: impl Fn(&Data) -> bool) {
        for _ in 0..400 {
            if done(&shared.lock()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("it did not come to pass");
    }

    #[test]
    fn load_fills_the_catalog() {
        let fake = fake("app-load", 0, "");
        let shared = context(&fake);
        load(shared.clone());
        let data = shared.lock();
        assert_eq!(data.catalog.len(), 2);
        assert!(!data.loading);
        assert!(!data.message.error);
    }

    #[test]
    fn a_failed_install_keeps_its_reason_until_the_user_acts() {
        let fake = fake("app-fail", 1, "");
        let shared = context(&fake);
        load(shared.clone());
        install(&shared, "org.gimp.GIMP");
        wait_until(&shared, |d| d.sticky);
        {
            let data = shared.lock();
            assert!(data.message.error);
            assert!(data.message.text.contains("no space left"));
            assert_eq!(data.rows.state("org.gimp.GIMP"), State::Failed);
        }
        // A note from a thread of ours does not wipe it out.
        {
            let mut data = shared.lock();
            Context::note(&mut data, Message::info("Updating the list of apps"));
            assert!(data.message.error);
        }
        search(&shared, "gi");
        assert!(!shared.lock().sticky);
    }

    #[test]
    fn an_install_marks_the_row_installed() {
        let fake = fake("app-ok", 0, "");
        let shared = context(&fake);
        load(shared.clone());
        install(&shared, "org.gimp.GIMP");
        wait_until(&shared, |d| {
            d.rows.state("org.gimp.GIMP") == State::Installed
        });
        assert!(!shared.lock().message.error);
    }

    #[test]
    fn an_install_shows_its_progress_and_then_none() {
        let fake = Fake::new(
            "app-progress",
            "case \"$*\" in\n\
             *remotes*) printf 'flathub\\thttps://dl.flathub.org/repo/\\tuser\\n';;\n\
             *remote-ls*) printf 'org.gimp.GIMP\\tGIMP\\tImages\\n';;\n\
             *\" install \"*) printf 'Installing 2/4…\\n'; printf 'Installing… 45%%  12.3 MB  4.1 MB/s\\r'; sleep 1;;\n\
             esac",
        );
        let shared = context(&fake);
        load(shared.clone());
        install(&shared, "org.gimp.GIMP");
        wait_until(&shared, |d| {
            d.progress.as_ref().is_some_and(|p| p.percent == Some(45))
        });
        search(&shared, "gimp");
        {
            let data = shared.lock();
            let v = view(&data, false);
            let row = v.rows.iter().find(|r| r.id == "org.gimp.GIMP").unwrap();
            let p = row.progress.as_ref().unwrap();
            assert_eq!(p.step, Some((2, 4)));
            assert_eq!(
                p.detail(),
                "Installing 2 of 4 \u{b7} 12.3 MB \u{b7} 4.1 MB/s"
            );
        }
        wait_until(&shared, |d| {
            d.rows.state("org.gimp.GIMP") == State::Installed
        });
        assert!(shared.lock().progress.is_none());
    }

    #[test]
    fn open_runs_only_an_installed_app() {
        let fake = fake("app-open", 0, "");
        let shared = context(&fake);
        load(shared.clone());
        open(&shared, "org.gimp.GIMP");
        std::thread::sleep(Duration::from_millis(200));
        assert!(!fake.calls().iter().any(|c| c.starts_with("run ")));
        install(&shared, "org.gimp.GIMP");
        wait_until(&shared, |d| {
            d.rows.state("org.gimp.GIMP") == State::Installed
        });
        open(&shared, "org.gimp.GIMP");
        wait_until(&shared, |_| {
            fake.calls().iter().any(|c| c == "run org.gimp.GIMP")
        });
        assert!(shared.lock().message.text.contains("Opening GIMP"));
    }

    #[test]
    fn an_app_installed_elsewhere_is_not_installed_again() {
        let fake = Fake::new(
            "app-elsewhere",
            "case \"$*\" in\n\
             *remotes*) printf 'flathub\\thttps://dl.flathub.org/repo/\\tuser\\n';;\n\
             *remote-ls*) printf 'org.gimp.GIMP\\tGIMP\\tImages\\n';;\n\
             list*) echo org.gimp.GIMP;;\n\
             *\" install \"*) exit 1;;\n\
             esac",
        );
        let shared = context(&fake);
        install_unlisted(&shared);
        wait_until(&shared, |d| {
            d.rows.state("org.gimp.GIMP") == State::Installed
        });
        assert!(!fake.calls().iter().any(|c| c.contains(" install ")));
    }

    /// An install for an app that is in the catalog but was not known to be
    /// installed when the list loaded.
    fn install_unlisted(shared: &Shared) {
        shared.lock().catalog = vec![App::new("org.gimp.GIMP", "GIMP", "")];
        install(shared, "org.gimp.GIMP");
    }

    #[test]
    fn offline_typing_does_not_ask_for_the_list_at_every_key() {
        let fake = Fake::new("app-offline", "echo 'error: could not connect' >&2; exit 1");
        let shared = context(&fake);
        load(shared.clone());
        assert!(shared.lock().message.error);
        let calls = fake.calls().len();
        for text in ["f", "fi", "fir", "fire"] {
            search(&shared, text);
        }
        std::thread::sleep(Duration::from_millis(300));
        // The first letter counted as a new search; the rest did not.
        wait_until(&shared, |d| !d.loading);
        let asked = fake.calls().len() - calls;
        assert!(asked <= 1, "{asked} more calls");
    }

    /// A flatpak that lists Firefox, GIMP (named as Flathub names it) and
    /// something that is not recommended.
    fn flathub_fake(tag: &str) -> Fake {
        Fake::new(
            tag,
            "case \"$*\" in\n\
             *remotes*) printf 'flathub\\thttps://dl.flathub.org/repo/\\tuser\\n';;\n\
             *remote-ls*) printf 'org.mozilla.firefox\\tFirefox\\tWeb browser\\n\
             org.gimp.GIMP\\tGNU Image Manipulation Program\\tImages\\n\
             org.x.Other\\tOther\\t\\n';;\n\
             esac",
        )
    }

    fn names_shown(data: &Data) -> Vec<String> {
        view(data, false)
            .rows
            .iter()
            .map(|r| {
                if r.heading {
                    format!("# {}", r.name)
                } else {
                    r.name.clone()
                }
            })
            .collect()
    }

    #[test]
    fn the_start_page_shows_only_what_flathub_has() {
        let fake = flathub_fake("app-featured");
        let shared = context(&fake);
        load(shared.clone());
        let data = shared.lock();
        let v = view(&data, true);
        assert_eq!(
            names_shown(&data),
            [
                "# Browsers",
                "Firefox",
                "# Graphics",
                "GNU Image Manipulation Program"
            ]
        );
        assert_eq!(v.first, 1);
        assert_eq!(v.rows[1].summary, "Web browser");
        assert!(v.rows.iter().all(|r| !r.unavailable || r.heading));
        assert!(v.notice.is_empty());
    }

    #[test]
    fn chips_and_typing_filter_the_start_page() {
        let fake = flathub_fake("app-chips");
        let shared = context(&fake);
        load(shared.clone());
        // Office has nothing on this list of apps.
        choose_chip(&shared, 3);
        assert!(names_shown(&shared.lock()).is_empty());
        choose_chip(&shared, 2);
        assert_eq!(names_shown(&shared.lock()), ["# Browsers", "Firefox"]);
        search(&shared, "gimp");
        assert!(names_shown(&shared.lock()).is_empty());
        choose_chip(&shared, 0);
        // The welcome wizard's word finds GIMP by its ID.
        assert_eq!(
            names_shown(&shared.lock()),
            ["GNU Image Manipulation Program"]
        );
        for word in ["firefox", "gimp"] {
            search(&shared, word);
            assert_eq!(names_shown(&shared.lock()).len(), 1, "{word}");
        }
    }

    #[test]
    fn offline_the_recommended_names_are_shown_and_not_installable() {
        let fake = Fake::new(
            "app-offline-start",
            "echo 'error: could not connect' >&2; exit 1",
        );
        let shared = context(&fake);
        load(shared.clone());
        let data = shared.lock();
        let v = view(&data, true);
        assert!(v.notice.starts_with("Not connected"));
        let apps: Vec<&RowData> = v.rows.iter().filter(|r| !r.heading).collect();
        assert!(apps.len() >= 10);
        assert!(apps.iter().all(|r| r.unavailable && r.summary.is_empty()));
        assert!(apps.iter().any(|r| r.name == "Firefox"));
        assert_eq!(v.rows[0].name, "Browsers");
        drop(data);
        install(&shared, "org.mozilla.firefox");
        std::thread::sleep(Duration::from_millis(100));
        assert!(!fake.calls().iter().any(|c| c.contains(" install ")));
        assert_eq!(
            shared.lock().rows.state("org.mozilla.firefox"),
            State::Available
        );
    }

    #[test]
    fn a_refused_source_is_not_called_not_connected() {
        let fake = Fake::new(
            "app-bad-remote-start",
            "case \"$*\" in *remotes*) printf 'flathub\\thttp://evil/\\tuser\\n';; esac",
        );
        let shared = context(&fake);
        load(shared.clone());
        assert!(view(&shared.lock(), false).notice.is_empty());
    }

    #[test]
    fn the_arrow_keys_skip_the_headings() {
        let fake = flathub_fake("app-keys");
        let shared = context(&fake);
        load(shared.clone());
        // # Browsers, Firefox, # Graphics, GIMP
        assert_eq!(step_row(&shared, 1, 1), 3);
        assert_eq!(step_row(&shared, 3, -1), 1);
        assert_eq!(step_row(&shared, 3, 1), 3);
    }

    #[test]
    fn a_source_that_is_not_flathub_stops_the_load() {
        let fake = Fake::new(
            "app-bad-remote",
            "case \"$*\" in *remotes*) printf 'flathub\\thttp://evil/\\tuser\\n';; esac",
        );
        let shared = context(&fake);
        load(shared.clone());
        let data = shared.lock();
        assert!(data.message.error);
        assert!(data.catalog.is_empty());
    }
}
