//! Joins the dock model to the Slint component. Everything here runs on the
//! UI thread; only the window list arrives from elsewhere.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::icons::{self, IconDirs};
use crate::model::{self, Action, Button, Dock, Icon, Item};
use crate::windows::{self, Controller, Source, Window};
use crate::{config, desktop, launch, ui};

/// Long enough for a frame to be drawn at 60 Hz.
const ONE_FRAME: Duration = Duration::from_millis(20);

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
    items: Vec<Item>,
    rows: Vec<Row>,
    /// Decoded icons by item key. None means no icon was found and the
    /// letter tile is shown.
    icons: HashMap<String, Option<Image>>,
    controller: Option<Controller>,
    size: (u32, u32),
    /// How many times the rows have been brought up to date.
    updates: u64,
}

struct Inner {
    ui: ui::Dock,
    rows: Rc<VecModel<ui::DockItem>>,
    state: RefCell<State>,
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
    fn row_for(&mut self, item: &Item, present: bool, icon_size: f32) -> ui::DockItem {
        let mut image = Image::default();
        let icon = match &item.icon {
            Icon::Search => ui::DockIcon::Search,
            Icon::Terminal => ui::DockIcon::Terminal,
            Icon::Named(name) => {
                let pixels = (icon_size * self.scale).round().max(1.0) as u32;
                let dirs = &self.icon_dirs;
                let loaded = self
                    .icons
                    .entry(item.key.clone())
                    .or_insert_with(|| icons::load_named(dirs, name, pixels).map(to_image));
                match loaded {
                    Some(loaded) => {
                        image = loaded.clone();
                        ui::DockIcon::Image
                    }
                    None => ui::DockIcon::Letter,
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
            present,
        }
    }
}

impl App {
    /// Wraps a freshly made Dock component. `on_resize` is told the logical
    /// size the surface should have whenever it changes, and once before
    /// this returns.
    pub fn new(
        ui: ui::Dock,
        dock: Dock,
        icon_dirs: IconDirs,
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
                    items: Vec::new(),
                    rows: Vec::new(),
                    icons: HashMap::new(),
                    controller: None,
                    size: (0, 0),
                    updates: 0,
                }),
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
    /// starts tracking windows. `deliver` is called on the tracker's thread
    /// with each new window list; it must get the list to the UI thread and
    /// call [`App::set_windows`] there. Without a compositor that offers
    /// window tracking the dock still works as a plain launcher.
    pub fn start(
        ui: ui::Dock,
        source: Source,
        mut deliver: impl FnMut(Vec<Window>) + Send + 'static,
        on_resize: impl Fn(u32, u32) + 'static,
    ) -> Self {
        let data_dirs = desktop::data_dirs();
        let home = std::env::var("HOME").ok();
        let icon_dirs = IconDirs::from_data_dirs(home.as_deref(), &data_dirs);
        let dock = Dock::load(config::pinned(&config::paths()), data_dirs);
        let app = Self::new(ui, dock, icon_dirs, on_resize);

        // Drop updates that change nothing the dock shows, so a terminal
        // retitling itself does not wake the UI thread.
        let mut last: Option<Vec<Window>> = None;
        let tracked = windows::spawn(source, move |list| {
            if last
                .as_deref()
                .is_none_or(|last| windows::differs_for_dock(last, list))
            {
                last = Some(list.to_vec());
                deliver(list.to_vec());
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

    /// Updates the dock for a new list of windows.
    pub fn set_windows(&self, windows: &[Window]) {
        let items = {
            let mut state = self.inner.state.borrow_mut();
            let items = state.dock.items(windows);
            if state.updates > 0 && items == state.items {
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
        self.perform(action);
    }

    fn perform(&self, action: Action) {
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
        if let Err(error) = result {
            eprintln!("sengi-dock: {error}");
        }
    }
}
