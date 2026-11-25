//! Connects the viewer window to the files: decoding on a thread, stepping
//! through the folder, and what the window says about the picture.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};

use crate::decode::{self, Decoded};
use crate::folder;
use crate::ui::{View, ViewWindow};

pub struct App {
    window: slint::Weak<ViewWindow>,
    files: RefCell<Vec<PathBuf>>,
    at: Cell<usize>,
    /// Counts up with each request, so that a slow decode that is no longer
    /// wanted is dropped when it arrives.
    generation: Cell<u64>,
    /// Decode on this thread, for tests that have no event loop.
    sync: bool,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// What was decoded, as it crosses from the thread to the window.
struct Ready {
    generation: u64,
    result: Result<(u32, u32, SharedPixelBuffer<Rgba8Pixel>), String>,
}

pub fn run(window: ViewWindow, path: Option<PathBuf>) -> Result<(), slint::PlatformError> {
    let _app = attach(&window, path, false);
    let result = window.run();
    APP.with(|slot| *slot.borrow_mut() = None);
    result
}

/// Connects the program to the window and starts the first picture. With
/// `sync` the picture is decoded at once on this thread. The app lives as
/// long as the value returned.
pub fn attach(window: &ViewWindow, path: Option<PathBuf>, sync: bool) -> Rc<App> {
    let app = Rc::new(App {
        window: window.as_weak(),
        files: RefCell::new(Vec::new()),
        at: Cell::new(0),
        generation: Cell::new(0),
        sync,
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    wire(window, &app);
    match path {
        Some(path) => {
            let (files, at) = folder::siblings(&path);
            *app.files.borrow_mut() = files;
            app.at.set(at);
            app.show();
        }
        None => app.empty(),
    }
    app
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// A size in bytes the way people read it.
pub fn size_text(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

impl App {
    fn with_view<T>(&self, f: impl FnOnce(View<'_>) -> T) -> Option<T> {
        self.window
            .upgrade()
            .map(|window| f(window.global::<View>()))
    }

    fn empty(&self) {
        self.with_view(|view| {
            view.set_message("No picture is open. Open one from Files.".into());
            view.set_message_is_error(false);
        });
    }

    /// The path of the picture shown.
    pub fn current(&self) -> Option<PathBuf> {
        self.files.borrow().get(self.at.get()).cloned()
    }

    /// The names in the folder, in the order they are stepped through.
    pub fn names(&self) -> Vec<String> {
        self.files.borrow().iter().map(|p| name_of(p)).collect()
    }

    /// What the window says about the picture, for tests.
    pub fn info_text(&self) -> String {
        self.with_view(|view| view.get_info().to_string())
            .unwrap_or_default()
    }

    pub fn message_text(&self) -> String {
        self.with_view(|view| view.get_message().to_string())
            .unwrap_or_default()
    }

    fn show(&self) {
        let Some(path) = self.current() else { return };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let total = self.files.borrow().len();
        let name = name_of(&path);
        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
        self.with_view(|view| {
            view.set_name(name.clone().into());
            view.set_title(format!("{name} \u{2014} Image Viewer").into());
            view.set_position(if total > 1 {
                format!("{} of {total}", self.at.get() + 1).into()
            } else {
                "".into()
            });
            view.set_can_browse(total > 1);
            view.set_info(size_text(bytes).into());
            view.set_has_picture(false);
            view.set_message("Opening...".into());
            view.set_message_is_error(false);
        });
        let work = move || {
            let result = decode::decode(&path).map(to_buffer);
            Ready { generation, result }
        };
        if self.sync {
            deliver(self, work(), name, bytes);
        } else {
            std::thread::spawn(move || {
                let ready = work();
                let _ = slint::invoke_from_event_loop(move || {
                    APP.with(|slot| {
                        if let Some(app) = slot.borrow().as_ref() {
                            deliver(app, ready, name, bytes);
                        }
                    });
                });
            });
        }
    }

    /// Steps through the folder, round the ends.
    fn step(&self, by: isize) {
        let count = self.files.borrow().len();
        if count < 2 {
            return;
        }
        let at = (self.at.get() as isize + by).rem_euclid(count as isize);
        self.at.set(at as usize);
        self.show();
    }
}

fn to_buffer(d: Decoded) -> (u32, u32, SharedPixelBuffer<Rgba8Pixel>) {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(d.shown_width, d.shown_height);
    buffer.make_mut_bytes().copy_from_slice(&d.rgba);
    (d.width, d.height, buffer)
}

fn deliver(app: &App, ready: Ready, name: String, bytes: u64) {
    if ready.generation != app.generation.get() {
        return;
    }
    app.with_view(|view| match ready.result {
        Ok((width, height, buffer)) => {
            view.set_picture(Image::from_rgba8(buffer));
            view.set_pixels_wide(width as i32);
            view.set_pixels_high(height as i32);
            view.set_zoom(0.0);
            view.set_serial(view.get_serial() + 1);
            view.set_info(format!("{width} x {height} px, {}", size_text(bytes)).into());
            view.set_message("".into());
            view.set_has_picture(true);
        }
        Err(why) => {
            view.set_picture(Image::default());
            view.set_pixels_wide(0);
            view.set_pixels_high(0);
            view.set_has_picture(false);
            view.set_message(format!("\"{name}\" is not shown. {why}").into());
            view.set_message_is_error(true);
        }
    });
}

fn wire(window: &ViewWindow, app: &Rc<App>) {
    let view = window.global::<View>();
    let weak = Rc::downgrade(app);
    view.on_next(move || {
        if let Some(app) = weak.upgrade() {
            app.step(1);
        }
    });
    let weak = Rc::downgrade(app);
    view.on_previous(move || {
        if let Some(app) = weak.upgrade() {
            app.step(-1);
        }
    });
    let weak = Rc::downgrade(app);
    view.on_close(move || {
        if let Some(app) = weak.upgrade() {
            if let Some(window) = app.window.upgrade() {
                let _ = window.hide();
            }
        }
    });
}

/// What a command line argument names: a path, or a `file://` address.
pub fn start_for(arg: Option<&str>, cwd: &Path) -> Result<Option<PathBuf>, String> {
    arg.map(|arg| sengi_open::resolve(arg, cwd)).transpose()
}
