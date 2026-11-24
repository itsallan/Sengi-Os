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

use crate::catalog::{self, App};
use crate::flatpak::{Error, Flatpak};
use crate::icons;
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
    loading: bool,
    query: String,
    rows: Rows,
    icons: HashMap<String, Option<Pixels>>,
    decoding: bool,
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
}

struct View {
    rows: Vec<RowData>,
    hint: String,
    message: Message,
    reset: bool,
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
    store.set_message(view.message.text.into());
    store.set_message_is_error(view.message.error);
    if view.reset {
        store.set_current(0);
    }
}

impl Context {
    fn new(flatpak: Flatpak, data_home: PathBuf, window: Option<slint::Weak<StoreWindow>>) -> Self {
        Self {
            data: Mutex::new(Data::default()),
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

fn view(data: &Data, reset: bool) -> View {
    let found = catalog::search(&data.catalog, &data.query, SHOWN);
    let rows: Vec<RowData> = found
        .iter()
        .map(|&index| {
            let app = &data.catalog[index];
            RowData {
                id: app.id.clone(),
                name: app.name.clone(),
                summary: app.summary.clone(),
                letter: sengi_text::initial(&app.name),
                icon: data.icons.get(&app.id).cloned().flatten(),
                state: data.rows.state(&app.id),
            }
        })
        .collect();
    let hint = if data.catalog.is_empty() {
        if data.loading {
            "Getting the list of apps".to_string()
        } else {
            "The list of apps is not available. Type to try again.".to_string()
        }
    } else if data.query.trim().is_empty() {
        "Type the name of an app".to_string()
    } else {
        format!("No app is named \u{201c}{}\u{201d}", data.query.trim())
    };
    View {
        rows,
        hint,
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
                    let found = catalog::search(&data.catalog, &data.query, SHOWN);
                    let wanted: Vec<String> = found
                        .iter()
                        .map(|&i| data.catalog[i].id.clone())
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
        Error::Failed(text) => format!("Could not get the list of apps: {text}"),
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

/// The install itself, on its own thread.
fn run_install(shared: &Shared, id: &str, name: &str) {
    // The list of what is installed may have failed to load, or the app
    // may have come from elsewhere since. A second copy is not made.
    let result = match shared.flatpak.installed() {
        Ok(installed) if installed.contains(id) => {
            shared.lock().rows.set_installed(installed);
            Ok(true)
        }
        Ok(_) => shared.flatpak.install(id).map(|()| false),
        Err(error) => Err(Error::Failed(format!(
            "could not check whether it is installed already: {error}"
        ))),
    };
    {
        let mut data = shared.lock();
        data.rows.finish(id, result.is_ok());
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
    let shared: Shared = Arc::new(Context::new(flatpak, data_home, Some(window.as_weak())));
    let store = window.global::<Store>();
    store.on_search({
        let shared = shared.clone();
        move |text| search(&shared, &text)
    });
    store.on_install({
        let shared = shared.clone();
        move |id| install(&shared, &id)
    });
    render(&shared, true);
    start_load(&shared);
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
