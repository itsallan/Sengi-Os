//! The welcome's behaviour: it joins the steps to the window and runs the
//! commands that carry out a choice.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use sengi_config::settings::{Layout, THEME_HIGH_CONTRAST, THEME_LIGHT};
use sengi_config::{HIGH_CONTRAST_ENABLED, LIGHT_THEME_ENABLED};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::marker;
use crate::model::{Step, Wizard};
use crate::system::{self, Entry, Net, Runner, Wallpaper};
use crate::ui::{Item, ThemeSwitch, Welcome, WelcomeWindow};

/// How many layouts are listed at once.
const LISTED: usize = 5;
/// How often the network is looked at while its step is shown.
const NET_EVERY: Duration = Duration::from_secs(3);

/// Where things are, and what is chosen already. Tests give their own.
pub struct Start {
    pub state_home: PathBuf,
    pub net_dir: PathBuf,
    pub xkb_list: PathBuf,
    pub wallpaper_dir: PathBuf,
    /// `appearance.wallpaper` as it is: "solid" or a path.
    pub wallpaper: String,
    /// `keyboard.layouts` as it is.
    pub layouts: Vec<Layout>,
    pub runner: Rc<dyn Runner>,
}

struct State {
    wizard: Wizard,
    all_layouts: Vec<Entry>,
    layouts: Vec<Layout>,
    wallpapers: Vec<Wallpaper>,
    /// 0 is the plain colour, then the pictures.
    wallpaper: usize,
    filter: String,
    net: Net,
}

pub struct App {
    window: slint::Weak<WelcomeWindow>,
    start: Start,
    state: RefCell<State>,
    timer: slint::Timer,
}

/// Opens the welcome for real.
pub fn run(window: WelcomeWindow, start: Start) -> Result<(), slint::PlatformError> {
    let app = attach(&window, start);
    let result = window.run();
    // Closed by any way at all counts as seen.
    app.finish();
    result
}

/// Connects the program to the window. The app lives as long as the value
/// returned.
pub fn attach(window: &WelcomeWindow, start: Start) -> Rc<App> {
    let wallpapers = system::wallpapers(&start.wallpaper_dir);
    let wallpaper = wallpapers
        .iter()
        .position(|w| w.path.to_string_lossy() == start.wallpaper)
        .map_or(0, |at| at + 1);
    let net = system::read_net(&start.net_dir);
    let state = State {
        wizard: Wizard::new(net == Net::Wired),
        all_layouts: system::load_layouts(&start.xkb_list),
        layouts: start.layouts.clone(),
        wallpapers,
        wallpaper,
        filter: String::new(),
        net,
    };
    let app = Rc::new(App {
        window: window.as_weak(),
        start,
        state: RefCell::new(state),
        timer: slint::Timer::default(),
    });
    let welcome = window.global::<Welcome>();
    welcome.set_offer_light(LIGHT_THEME_ENABLED);
    welcome.set_offer_high_contrast(HIGH_CONTRAST_ENABLED);
    wire(window, &app);
    app.show();
    app
}

fn strings(list: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        list.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    ))
}

impl App {
    fn with_welcome<T>(&self, f: impl FnOnce(Welcome<'_>) -> T) -> Option<T> {
        self.window
            .upgrade()
            .map(|window| f(window.global::<Welcome>()))
    }

    /// The window for the step the wizard is on.
    pub fn show(self: &Rc<Self>) {
        let state = self.state.borrow();
        let (at, of) = state.wizard.position();
        let step = state.wizard.current();
        let counter = format!("Step {at} of {of}");
        let names = std::iter::once("Plain colour".to_string())
            .chain(state.wallpapers.iter().map(|w| w.title.clone()));
        let names = strings(names);
        let (first, last) = (state.wizard.is_first(), state.wizard.is_last());
        let (wallpaper, suggestions) = (
            state.wallpaper as i32,
            strings(
                system::SUGGESTIONS
                    .iter()
                    .map(|(label, _)| label.to_string()),
            ),
        );
        drop(state);
        self.with_welcome(|w| {
            w.set_step(step.index());
            w.set_counter(counter.into());
            w.set_can_back(!first);
            w.set_last(last);
            w.set_wallpapers(names);
            w.set_wallpaper(wallpaper);
            w.set_suggestions(suggestions);
        });
        self.show_net();
        self.show_layouts();
        // Only the network step looks at the network, and only while shown.
        if step == Step::Network {
            let weak = Rc::downgrade(self);
            self.timer
                .start(slint::TimerMode::Repeated, NET_EVERY, move || {
                    if let Some(app) = weak.upgrade() {
                        app.check_net();
                    }
                });
        } else {
            self.timer.stop();
        }
    }

    fn check_net(&self) {
        let net = system::read_net(&self.start.net_dir);
        self.state.borrow_mut().net = net;
        self.show_net();
    }

    fn show_net(&self) {
        let net = self.state.borrow().net;
        let (code, title, detail) = match net {
            Net::Wired => (2, "Connected by cable", "You are online."),
            Net::Wifi => (1, "Connected to Wi-Fi", "You are online."),
            Net::Offline => (
                0,
                "Not connected",
                "Choose a Wi-Fi network in Settings, or plug in a cable.",
            ),
        };
        self.with_welcome(|w| {
            w.set_net(code);
            w.set_net_title(title.into());
            w.set_net_detail(detail.into());
        });
    }

    /// The layouts to choose from for the filter, and which is in use.
    fn show_layouts(&self) {
        let state = self.state.borrow();
        let now = state
            .layouts
            .first()
            .map(Layout::render)
            .unwrap_or_else(|| "us".to_string());
        let name_of = |id: &str| {
            let plain = id.split('(').next().unwrap_or(id);
            state
                .all_layouts
                .iter()
                .find(|e| e.id == plain)
                .map_or_else(|| id.to_string(), |e| e.name.clone())
        };
        let items: Vec<Item> = system::filter_layouts(&state.all_layouts, &state.filter, LISTED)
            .into_iter()
            .map(|entry| Item {
                id: entry.id.as_str().into(),
                title: entry.name.as_str().into(),
                action: "Use".into(),
                checked: entry.id == now,
                enabled: true,
                ..Item::default()
            })
            .collect();
        let name = name_of(&now);
        drop(state);
        self.with_welcome(|w| {
            w.set_layouts(ModelRc::new(VecModel::from(items)));
            w.set_layout_now(name.into());
        });
    }

    fn choose_theme(&self, name: &str) {
        if (name == THEME_LIGHT && !LIGHT_THEME_ENABLED)
            || (name == THEME_HIGH_CONTRAST && !HIGH_CONTRAST_ENABLED)
        {
            return;
        }
        if let Some(window) = self.window.upgrade() {
            let switch = window.global::<ThemeSwitch>();
            switch.set_light(name == THEME_LIGHT);
            switch.set_high_contrast(name == THEME_HIGH_CONTRAST);
        }
        self.start.runner.change(system::theme_command(name));
    }

    fn choose_wallpaper(&self, index: usize) {
        let mut state = self.state.borrow_mut();
        if index > state.wallpapers.len() {
            return;
        }
        state.wallpaper = index;
        let path = index
            .checked_sub(1)
            .map(|at| state.wallpapers[at].path.clone());
        drop(state);
        self.with_welcome(|w| w.set_wallpaper(index as i32));
        self.start
            .runner
            .change(system::wallpaper_command(path.as_deref()));
    }

    fn choose_layout(&self, id: &str) {
        let list = {
            let mut state = self.state.borrow_mut();
            let list = system::with_first(&state.layouts, id);
            state.layouts = list.iter().filter_map(|l| Layout::parse(l)).collect();
            list
        };
        self.start.runner.change(system::layouts_command(&list));
        self.show_layouts();
    }

    fn filter(&self, text: &str) {
        self.state.borrow_mut().filter = text.to_string();
        self.show_layouts();
    }

    fn next(self: &Rc<Self>) {
        let moved = self.state.borrow_mut().wizard.advance();
        if moved {
            self.show();
        } else {
            self.finish();
        }
    }

    fn back(self: &Rc<Self>) {
        if self.state.borrow_mut().wizard.back() {
            self.show();
        }
    }

    /// Done or skipped or closed: it is not shown again by itself.
    pub fn finish(&self) {
        let _ = marker::mark_done(&self.start.state_home);
        if let Some(window) = self.window.upgrade() {
            let _ = window.hide();
        }
    }

    pub fn step(&self) -> Step {
        self.state.borrow().wizard.current()
    }

    pub fn wired(&self) -> bool {
        self.state.borrow().net == Net::Wired
    }
}

fn wire(window: &WelcomeWindow, app: &Rc<App>) {
    let welcome = window.global::<Welcome>();
    welcome.on_next({
        let app = app.clone();
        move || app.next()
    });
    welcome.on_back({
        let app = app.clone();
        move || app.back()
    });
    welcome.on_skip({
        let app = app.clone();
        move || app.finish()
    });
    welcome.on_choose_theme({
        let app = app.clone();
        move |name| app.choose_theme(&name)
    });
    welcome.on_choose_wallpaper({
        let app = app.clone();
        move |index| app.choose_wallpaper(index.max(0) as usize)
    });
    welcome.on_filter_layouts({
        let app = app.clone();
        move |text| app.filter(&text)
    });
    welcome.on_choose_layout({
        let app = app.clone();
        move |id| app.choose_layout(&id)
    });
    welcome.on_open_network({
        let app = app.clone();
        move || app.start.runner.open(system::settings_command("network"))
    });
    welcome.on_open_store({
        let app = app.clone();
        move |index| {
            let search = usize::try_from(index)
                .ok()
                .and_then(|at| system::SUGGESTIONS.get(at))
                .map_or("", |(_, search)| *search);
            app.start.runner.open(system::store_command(search));
        }
    });
    welcome.on_open_files({
        let app = app.clone();
        move || app.start.runner.open(system::files_command())
    });
    welcome.on_open_settings({
        let app = app.clone();
        move || app.start.runner.open(system::settings_command(""))
    });
}
