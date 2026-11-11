//! Joins the dock model to the Slint component. Everything here runs on the
//! UI thread; window lists and decoded icons arrive from other threads as
//! [`Message`]s.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use sengi_apps::icons::{self, Contexts, Decoder, IconDirs, DECODE_TIMEOUT};
use sengi_apps::Env;

use crate::model::{self, Action, Button, Dock, Icon, Item, Launches, LAUNCHER_KEY, LAUNCH_WAIT};
use crate::windows::{self, Controller, Source, Window};
use crate::{config, launch, ui};

/// The widest the dock may be until the output's real width is known: a
/// width every laptop panel has.
pub const DEFAULT_MAX_WIDTH: u32 = 1280;

/// Long enough for a frame to be drawn at 60 Hz.
const ONE_FRAME: Duration = Duration::from_millis(20);

/// What other threads send to the dock.
pub enum Message {
    /// The windows that are open now.
    Windows(Vec<Window>),
    /// An icon has been decoded, or has turned out not to be usable.
    Icon {
        key: String,
        /// The size it was decoded for, in pixels per side.
        pixels: u32,
        icon: Option<icons::Pixels>,
    },
}

/// Carries a [`Message`] from any thread to the UI thread, where it must be
/// handed to [`App::receive`].
pub type Deliver = Arc<dyn Fn(Message) + Send + Sync>;

/// Where an item's icon has got to.
enum IconState {
    /// Being decoded on another thread. The letter tile stands in.
    Loading,
    /// Decoded, or known to be missing or unusable.
    Ready(Option<Image>),
}

/// One row of the Slint model. Rows outlive their item for a moment while
/// they slide out.
struct Row {
    key: String,
    /// Set when the row starts sliding out, to the count of updates so far.
    leaving: Option<u64>,
}

struct State {
    dock: Dock,
    icon_dirs: IconDirs,
    scale: f32,
    /// The windows last heard of, kept so the items can be worked out
    /// again when the room for them changes.
    windows: Vec<Window>,
    /// The widest the dock may be, in logical pixels.
    max_width: u32,
    /// The items on show: those that fit.
    items: Vec<Item>,
    rows: Vec<Row>,
    /// Icons by item key.
    icons: HashMap<String, IconState>,
    /// Icons to start decoding: item key, icon name, pixels per side.
    wanted: Vec<(String, String, u32)>,
    decoder: Decoder,
    launches: Launches,
    /// The dock's generation the icons were looked up in.
    generation: u64,
    controller: Option<Controller>,
    size: (u32, u32),
    /// How many times the rows have been brought up to date.
    updates: u64,
}

struct Inner {
    ui: ui::Dock,
    rows: Rc<VecModel<ui::DockItem>>,
    state: RefCell<State>,
    deliver: Deliver,
    on_resize: Box<dyn Fn(u32, u32)>,
}

/// The running dock. Cloning gives another handle to the same dock.
#[derive(Clone)]
pub struct App {
    inner: Rc<Inner>,
}

fn to_image(pixels: icons::Pixels) -> Image {
    let buffer =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pixels.data, pixels.size, pixels.size);
    Image::from_rgba8_premultiplied(buffer)
}

impl State {
    /// The size to ask the decoder for. It gives nothing above
    /// `icons::MAX_PIXELS` (256), which the dock's icons stay well under
    /// at any scale an output really has; past that an app would get its
    /// letter tile.
    fn icon_pixels(&self, icon_size: f32) -> u32 {
        (icon_size * self.scale).round().max(1.0) as u32
    }

    fn row_for(&mut self, item: &Item, present: bool, icon_size: f32) -> ui::DockItem {
        let mut image = Image::default();
        let icon = match &item.icon {
            Icon::Search => ui::DockIcon::Search,
            Icon::Terminal => ui::DockIcon::Terminal,
            Icon::Named(name) => {
                if !self.icons.contains_key(&item.key) {
                    // Decoding happens elsewhere; see `start_decoding`.
                    let state = if name.is_empty() {
                        IconState::Ready(None)
                    } else {
                        let pixels = self.icon_pixels(icon_size);
                        self.wanted.push((item.key.clone(), name.clone(), pixels));
                        IconState::Loading
                    };
                    self.icons.insert(item.key.clone(), state);
                }
                match self.icons.get(&item.key) {
                    Some(IconState::Ready(Some(loaded))) => {
                        image = loaded.clone();
                        ui::DockIcon::Image
                    }
                    _ => ui::DockIcon::Letter,
                }
            }
        };
        ui::DockItem {
            name: item.name.as_str().into(),
            icon,
            image,
            letter: item.letter.as_str().into(),
            running: item.running(),
            focused: item.focused(),
            windows: item.window_count() as i32,
            launching: !item.running() && self.launches.pending(&item.key, Instant::now()),
            present,
        }
    }
}

impl App {
    /// Wraps a freshly made Dock component. `deliver` must get each message
    /// to the UI thread and hand it to [`App::receive`] there. `on_resize`
    /// is told the logical size the surface should have whenever it
    /// changes, and once before this returns.
    pub fn new(
        ui: ui::Dock,
        dock: Dock,
        icon_dirs: IconDirs,
        deliver: Deliver,
        on_resize: impl Fn(u32, u32) + 'static,
    ) -> Self {
        let scale = ui.window().scale_factor();
        let rows = Rc::new(VecModel::default());
        ui.set_items(ModelRc::from(rows.clone()));
        let app = Self {
            inner: Rc::new(Inner {
                ui,
                rows,
                state: RefCell::new(State {
                    dock,
                    icon_dirs,
                    scale,
                    windows: Vec::new(),
                    max_width: DEFAULT_MAX_WIDTH,
                    items: Vec::new(),
                    rows: Vec::new(),
                    icons: HashMap::new(),
                    wanted: Vec::new(),
                    decoder: Decoder::default(),
                    launches: Launches::default(),
                    generation: 0,
                    controller: None,
                    size: (0, 0),
                    updates: 0,
                }),
                deliver,
                on_resize: Box::new(on_resize),
            }),
        };

        let weak = Rc::downgrade(&app.inner);
        app.inner.ui.on_clicked(move |index| {
            if let Some(inner) = weak.upgrade() {
                App { inner }.click(index, Button::Primary);
            }
        });
        let weak = Rc::downgrade(&app.inner);
        app.inner.ui.on_middle_clicked(move |index| {
            if let Some(inner) = weak.upgrade() {
                App { inner }.click(index, Button::Secondary);
            }
        });

        let weak = Rc::downgrade(&app.inner);
        app.inner.ui.on_rescaled(move || {
            if let Some(inner) = weak.upgrade() {
                let scale = inner.ui.window().scale_factor();
                App { inner }.set_scale(scale);
            }
        });

        app.set_windows(&[]);
        app
    }

    /// Reads the config and the installed apps, wraps the component and
    /// starts tracking windows. Without a compositor that offers window
    /// tracking the dock still works as a plain launcher.
    pub fn start(
        ui: ui::Dock,
        source: Source,
        deliver: Deliver,
        on_resize: impl Fn(u32, u32) + 'static,
    ) -> Self {
        let env = Env::from_process();
        let data_dirs = env.data_roots();
        let icon_dirs = IconDirs::for_env(&env);
        let dock = Dock::load(config::pinned(&config::paths()), data_dirs, env);
        let app = Self::new(ui, dock, icon_dirs, deliver.clone(), on_resize);

        // Drop updates that change nothing the dock shows, so a terminal
        // retitling itself does not wake the UI thread.
        let mut last: Option<Vec<Window>> = None;
        let tracked = windows::spawn(source, move |list| {
            if last
                .as_deref()
                .is_none_or(|last| windows::differs_for_dock(last, list))
            {
                last = Some(list.to_vec());
                deliver(Message::Windows(list.to_vec()));
            }
        });
        match tracked {
            Ok(controller) => app.inner.state.borrow_mut().controller = Some(controller),
            Err(error) => eprintln!("sengi-dock: windows are not tracked: {error}"),
        }
        app
    }

    pub fn ui(&self) -> &ui::Dock {
        &self.inner.ui
    }

    /// The logical size the surface should have right now.
    pub fn size(&self) -> (u32, u32) {
        self.inner.state.borrow().size
    }

    /// Takes in a message from another thread. Call on the UI thread.
    pub fn receive(&self, message: Message) {
        match message {
            Message::Windows(windows) => self.set_windows(&windows),
            Message::Icon { key, pixels, icon } => self.set_icon(key, pixels, icon),
        }
    }

    fn set_icon(&self, key: String, pixels: u32, icon: Option<icons::Pixels>) {
        let items = {
            let mut state = self.inner.state.borrow_mut();
            let wanted = state.icon_pixels(self.inner.ui.get_icon_size());
            // Late answers for an app that has gone, or for a scale the
            // output no longer has, are dropped.
            if pixels != wanted || !matches!(state.icons.get(&key), Some(IconState::Loading)) {
                return;
            }
            state
                .icons
                .insert(key, IconState::Ready(icon.map(to_image)));
            state.items.clone()
        };
        self.sync(items);
    }

    /// Hands the icons that rows asked for to worker threads. Each decodes
    /// in a child process and gives up after a second, so neither a huge
    /// icon nor a hostile one can hold up the dock.
    fn start_decoding(&self) {
        let (wanted, decoder, dirs) = {
            let mut state = self.inner.state.borrow_mut();
            if state.wanted.is_empty() {
                return;
            }
            (
                std::mem::take(&mut state.wanted),
                state.decoder.clone(),
                state.icon_dirs.clone(),
            )
        };
        let deliver = self.inner.deliver.clone();
        let spawned = std::thread::Builder::new()
            .name("icons".into())
            .spawn(move || {
                for (key, name, pixels) in wanted {
                    let icon = icons::find(&dirs, None, &name, pixels, Contexts::Apps)
                        .and_then(|path| decoder.decode_file(&path, pixels, DECODE_TIMEOUT));
                    deliver(Message::Icon { key, pixels, icon });
                }
            });
        if let Err(error) = spawned {
            eprintln!("sengi-dock: cannot load icons: {error}");
        }
    }

    /// Decodes the icons again for a new output scale.
    fn set_scale(&self, scale: f32) {
        let items = {
            let mut state = self.inner.state.borrow_mut();
            if state.scale == scale {
                return;
            }
            state.scale = scale;
            state.icons.clear();
            state.items.clone()
        };
        self.sync(items);
    }

    /// Says how wide the dock may be: the output's width, less whatever
    /// should stay clear at its sides. A layer surface wider than its
    /// output is centred all the same, which would push the search button
    /// off the screen, so items that do not fit are left off the end.
    pub fn set_max_width(&self, width: u32) {
        let windows = {
            let mut state = self.inner.state.borrow_mut();
            if state.max_width == width {
                return;
            }
            state.max_width = width;
            state.windows.clone()
        };
        self.set_windows(&windows);
    }

    /// How many items fit across the widest the dock may be.
    fn capacity(&self) -> usize {
        let ui = &self.inner.ui;
        let room = self.inner.state.borrow().max_width as f32 - ui.get_side_padding() * 2.0;
        (room / ui.get_slot_width().max(1.0)).floor().max(1.0) as usize
    }

    /// Updates the dock for a new list of windows.
    pub fn set_windows(&self, windows: &[Window]) {
        let capacity = self.capacity();
        let items = {
            let mut state = self.inner.state.borrow_mut();
            state.windows = windows.to_vec();
            let items = model::fit(state.dock.items(windows), capacity);
            // Apps were installed or removed: an icon that was missing may
            // exist now, and one that was found may have changed.
            let reloaded = state.generation != state.dock.generation();
            if reloaded {
                state.generation = state.dock.generation();
                state.icons.clear();
            }
            if state.updates > 0 && items == state.items && !reloaded {
                return;
            }
            items
        };
        self.sync(items);
    }

    /// Brings the Slint rows in line with `items`. Rows for new items slide
    /// in; rows whose item is gone slide out and are removed afterwards.
    fn sync(&self, items: Vec<Item>) {
        let inner = &self.inner;
        let icon_size = inner.ui.get_icon_size();
        let mut state = inner.state.borrow_mut();
        // Nothing slides when the dock first appears.
        let animate = state.updates > 0;
        state.launches.settle(&items, Instant::now());
        state.updates += 1;
        let update = state.updates;
        let mut added = Vec::new();
        let mut cursor = 0;

        for item in &items {
            let mut found = state.rows.iter().position(|row| row.key == item.key);
            if let Some(at) = found.filter(|&at| at < cursor) {
                // Out of place; put it back where it belongs.
                state.rows.remove(at);
                inner.rows.remove(at);
                cursor -= 1;
                found = None;
            }
            match found {
                Some(at) => {
                    state.rows[at].leaving = None;
                    let row = state.row_for(item, true, icon_size);
                    if inner.rows.row_data(at).as_ref() != Some(&row) {
                        inner.rows.set_row_data(at, row);
                    }
                    cursor = at + 1;
                }
                None => {
                    let row = state.row_for(item, !animate, icon_size);
                    state.rows.insert(
                        cursor,
                        Row {
                            key: item.key.clone(),
                            leaving: None,
                        },
                    );
                    inner.rows.insert(cursor, row);
                    if animate {
                        added.push(item.key.clone());
                    }
                    cursor += 1;
                }
            }
        }

        let mut leaving = false;
        for (at, row) in state.rows.iter_mut().enumerate() {
            if row.leaving.is_none() && !items.iter().any(|item| item.key == row.key) {
                row.leaving = Some(update);
                leaving = true;
                if let Some(mut data) = inner.rows.row_data(at) {
                    data.present = false;
                    inner.rows.set_row_data(at, data);
                }
            }
        }

        state.items = items;
        drop(state);

        self.start_decoding();
        self.update_size();
        if !added.is_empty() {
            // Slint only animates a property it has already worked out once,
            // so the new rows are drawn closed for one frame and then opened.
            let weak = Rc::downgrade(inner);
            slint::Timer::single_shot(ONE_FRAME, move || {
                if let Some(inner) = weak.upgrade() {
                    App { inner }.reveal(&added);
                }
            });
        }
        if leaving {
            let wait = Duration::from_millis(inner.ui.get_slide_duration().max(0) as u64);
            let weak = Rc::downgrade(inner);
            slint::Timer::single_shot(wait, move || {
                if let Some(inner) = weak.upgrade() {
                    App { inner }.purge(update);
                }
            });
        }
    }

    fn reveal(&self, keys: &[String]) {
        let state = self.inner.state.borrow();
        for (at, row) in state.rows.iter().enumerate() {
            if row.leaving.is_some() || !keys.contains(&row.key) {
                continue;
            }
            if let Some(mut data) = self.inner.rows.row_data(at) {
                data.present = true;
                self.inner.rows.set_row_data(at, data);
            }
        }
    }

    /// Removes the rows that began sliding out in update `until` or before.
    fn purge(&self, until: u64) {
        {
            let mut state = self.inner.state.borrow_mut();
            let mut at = 0;
            while at < state.rows.len() {
                if state.rows[at].leaving.is_some_and(|since| since <= until) {
                    let row = state.rows.remove(at);
                    self.inner.rows.remove(at);
                    // Its icon is not needed until the app runs again.
                    state.icons.remove(&row.key);
                } else {
                    at += 1;
                }
            }
        }
        self.update_size();
    }

    fn update_size(&self) {
        let ui = &self.inner.ui;
        let size = (
            ui.get_dock_width().round().max(1.0) as u32,
            ui.get_dock_height().round().max(1.0) as u32,
        );
        let changed = {
            let mut state = self.inner.state.borrow_mut();
            let changed = state.size != size;
            state.size = size;
            changed
        };
        if changed {
            (self.inner.on_resize)(size.0, size.1);
        }
    }

    fn click(&self, index: i32, button: Button) {
        let action = {
            let state = self.inner.state.borrow();
            let Some(row) = usize::try_from(index)
                .ok()
                .and_then(|at| state.rows.get(at))
            else {
                return;
            };
            match state.items.iter().find(|item| item.key == row.key) {
                Some(item) => model::on_click(item, button),
                // The row is on its way out.
                None => return,
            }
        };
        // A launch that is still on its way is not repeated by another
        // click. A secondary click asks for a new instance and always gets one.
        let starting = match (&action, button) {
            (Action::OpenLauncher, _) => Some(LAUNCHER_KEY.to_string()),
            (Action::Launch(key), Button::Primary) => Some(key.clone()),
            _ => None,
        };
        if let Some(key) = &starting {
            let mut state = self.inner.state.borrow_mut();
            if !state.launches.begin(key, Instant::now()) {
                return;
            }
        }
        let started = self.perform(action);
        let Some(key) = starting else {
            return;
        };
        if !started {
            self.inner.state.borrow_mut().launches.cancel(&key);
        } else if key != LAUNCHER_KEY {
            // Show that the click was taken, and stop showing it if no
            // window ever comes.
            self.refresh();
            let weak = Rc::downgrade(&self.inner);
            slint::Timer::single_shot(LAUNCH_WAIT, move || {
                if let Some(inner) = weak.upgrade() {
                    App { inner }.refresh();
                }
            });
        }
    }

    /// Redraws the rows from the items as they are.
    fn refresh(&self) {
        let items = self.inner.state.borrow().items.clone();
        self.sync(items);
    }

    /// Carries out a click's action. False if it could not be done.
    fn perform(&self, action: Action) -> bool {
        let state = self.inner.state.borrow();
        let result = match action {
            Action::OpenLauncher => launch::open_launcher(),
            Action::Launch(key) => match state.dock.entry(&key) {
                Some(entry) => launch::launch(entry),
                None => Ok(()),
            },
            Action::Activate(id) => {
                if let Some(controller) = &state.controller {
                    controller.activate(id);
                }
                Ok(())
            }
            Action::Minimize(id) => {
                if let Some(controller) = &state.controller {
                    controller.set_minimized(id, true);
                }
                Ok(())
            }
            Action::Nothing => Ok(()),
        };
        if let Err(error) = &result {
            eprintln!("sengi-dock: {error}");
        }
        result.is_ok()
    }
}
