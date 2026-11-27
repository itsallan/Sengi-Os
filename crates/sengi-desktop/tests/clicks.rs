//! The right button on the real desktop, with pointer events only: every
//! bare part of it has New Folder and New File, wherever the icons are and
//! whatever happened before.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use sengi_apps::Env;
use sengi_desktop::app::Shell;
use sengi_desktop::desk::Desk;
use sengi_desktop::menu::{Choice, Request};
use sengi_desktop::ui::{Desktop, DesktopMenu, ThemeSwitch};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, PlatformError};

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 720.0;
/// The desktop's own menu: its padding, four rows of 22 and one line.
const MENU_HEIGHT: f32 = 109.0;
const ITEM_MENU_HEIGHT: f32 = 87.0;

thread_local! {
    static NOW: Cell<u64> = const { Cell::new(0) };
}

/// The windows made so far, in order.
type Windows = Rc<RefCell<Vec<Rc<MinimalSoftwareWindow>>>>;

/// Windows with no display and a clock that moves only when told to, so
/// that two clicks here are never taken for a double click.
struct Headless(Windows);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        window.set_size(PhysicalSize::new(WIDTH as u32, HEIGHT as u32));
        self.0.borrow_mut().push(window.clone());
        Ok(window)
    }

    fn duration_since_start(&self) -> Duration {
        Duration::from_millis(NOW.with(Cell::get))
    }
}

/// A menu that is up: its window, which covers everything, and what it said.
struct Menu {
    window: Rc<MinimalSoftwareWindow>,
    component: DesktopMenu,
    said: Rc<RefCell<Option<String>>>,
}

struct Screen {
    shell: Rc<Shell>,
    window: Rc<MinimalSoftwareWindow>,
    /// The menu on screen, as `sengi-desktop --menu` would show it.
    menu: Rc<RefCell<Option<Menu>>>,
    dir: PathBuf,
}

impl Screen {
    /// Draws, which lays out what is new, and lets time pass.
    fn settle(&self) {
        NOW.with(|now| now.set(now.get() + 1000));
        slint::platform::update_timers_and_animations();
        paint(&self.window, WIDTH as u32, HEIGHT as u32);
        if let Some(menu) = self.menu_window() {
            paint(&menu, WIDTH as u32, HEIGHT as u32);
        }
        slint::platform::update_timers_and_animations();
    }

    fn menu_window(&self) -> Option<Rc<MinimalSoftwareWindow>> {
        self.menu.borrow().as_ref().map(|menu| menu.window.clone())
    }

    /// Where input goes: an open menu is over everything.
    fn target(&self) -> Rc<MinimalSoftwareWindow> {
        self.menu_window().unwrap_or_else(|| self.window.clone())
    }

    /// Takes the menu away if it has answered, and carries the answer out.
    fn answer(&self) {
        let said = self
            .menu
            .borrow()
            .as_ref()
            .and_then(|menu| menu.said.borrow_mut().take());
        let Some(said) = said else { return };
        if let Some(menu) = self.menu.borrow_mut().take() {
            menu.component.hide().unwrap();
        }
        if let Some(choice) = Choice::parse(&said) {
            self.shell.chosen(choice);
        }
    }

    fn button(&self, x: f32, y: f32, button: PointerEventButton) {
        let position = LogicalPosition::new(x, y);
        let target = self.target();
        target.dispatch_event(WindowEvent::PointerMoved { position });
        target.dispatch_event(WindowEvent::PointerPressed { position, button });
        target.dispatch_event(WindowEvent::PointerReleased { position, button });
        self.answer();
        self.settle();
    }

    fn click(&self, x: f32, y: f32) {
        self.button(x, y, PointerEventButton::Left);
    }

    fn right(&self, x: f32, y: f32) {
        self.button(x, y, PointerEventButton::Right);
    }

    fn key(&self, text: &str) {
        let target = self.target();
        target.dispatch_event(WindowEvent::KeyPressed { text: text.into() });
        target.dispatch_event(WindowEvent::KeyReleased { text: text.into() });
        self.answer();
        self.settle();
    }

    /// Clicks the row of an open menu that was asked for at (x, y): `down`
    /// is how far the middle of the row is under the menu's top.
    fn menu_row(&self, (x, y): (f32, f32), height: f32, down: f32) {
        let top = y.min(HEIGHT - height - 4.0).max(4.0);
        self.click((x + 40.0).clamp(30.0, WIDTH - 30.0), top + down);
    }

    /// With SENGI_RENDER_DIR set, writes the desktop at this size with its
    /// menu open, over a flat stand-in for the wallpaper.
    fn picture(&self, width: u32, height: u32, light: bool) {
        let Some(out) = std::env::var_os("SENGI_RENDER_DIR") else {
            return;
        };
        self.window.set_size(PhysicalSize::new(width, height));
        self.settle();
        self.right(width as f32 / 2.0, height as f32 / 2.0);
        let menu = self.menu_window().expect("no menu to take a picture of");
        menu.set_size(PhysicalSize::new(width, height));
        self.settle();
        let mut buffer = paint(&self.window, width, height);
        for (pixel, over) in buffer.iter_mut().zip(paint(&menu, width, height)) {
            let under =
                |channel: u8| (u16::from(channel) * u16::from(255 - over.alpha) / 255) as u8;
            *pixel = PremultipliedRgbaColor {
                red: over.red.saturating_add(under(pixel.red)),
                green: over.green.saturating_add(under(pixel.green)),
                blue: over.blue.saturating_add(under(pixel.blue)),
                alpha: over.alpha.saturating_add(under(pixel.alpha)),
            };
        }
        let behind: [u8; 3] = if light {
            [0xc8, 0xcc, 0xd4]
        } else {
            [0x30, 0x34, 0x3c]
        };
        let data: Vec<u8> = buffer
            .iter()
            .flat_map(|pixel| {
                let under =
                    |at: usize| (u16::from(behind[at]) * u16::from(255 - pixel.alpha) / 255) as u8;
                [
                    pixel.red.saturating_add(under(0)),
                    pixel.green.saturating_add(under(1)),
                    pixel.blue.saturating_add(under(2)),
                ]
            })
            .collect();
        let theme = if light { "-light" } else { "" };
        let name = format!("desktop-menu-{width}x{height}{theme}.png");
        let file = fs::File::create(Path::new(&out).join(name)).unwrap();
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&data)
            .unwrap();
        self.key(&String::from(char::from(Key::Escape)));
    }

    fn count(&self, folders: bool) -> usize {
        fs::read_dir(&self.dir)
            .unwrap()
            .flatten()
            .filter(|item| item.path().is_dir() == folders)
            .count()
    }

    /// A right-click at the spot offers New Folder and New File, and both
    /// work from there.
    fn offers_both(&self, spot: (f32, f32), what: &str) {
        for (folders, down) in [(true, 17.0), (false, 39.0)] {
            let before = self.count(folders);
            self.right(spot.0, spot.1);
            self.menu_row(spot, MENU_HEIGHT, down);
            assert_eq!(self.count(folders), before + 1, "{what} at {spot:?}");
            // The new one is in rename: Escape keeps its name.
            self.key(&String::from(char::from(Key::Escape)));
            for item in fs::read_dir(&self.dir).unwrap().flatten() {
                if item.file_name().to_string_lossy().starts_with("untitled") {
                    let _ = fs::remove_dir(item.path());
                    let _ = fs::remove_file(item.path());
                }
            }
            self.shell.refresh();
            self.settle();
        }
    }
}

fn paint(window: &MinimalSoftwareWindow, width: u32, height: u32) -> Vec<PremultipliedRgbaColor> {
    let mut buffer = vec![PremultipliedRgbaColor::default(); (width * height) as usize];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, width as usize);
    });
    buffer
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|item| item.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The middle of the n-th icon of the first column.
fn icon(n: usize) -> (f32, f32) {
    (64.0, 44.0 + 104.0 * n as f32 + 40.0)
}

#[test]
fn bare_desktop_always_offers_new_folder_and_new_file() {
    let windows: Windows = Rc::default();
    slint::platform::set_platform(Box::new(Headless(windows.clone()))).unwrap();

    let root = std::env::temp_dir().join(format!("sengi-desktop-clicks-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let dir = root.join("Desktop");
    fs::create_dir_all(&dir).unwrap();
    let data = root.join("share");
    let env = Env::from_vars(|name| match name {
        "PATH" => Some("/nonexistent".into()),
        "HOME" => Some(root.clone().into_os_string()),
        "XDG_DATA_HOME" => Some(data.clone().into_os_string()),
        _ => None,
    });
    let shell = Shell::new(Desk::new(dir.clone(), env));
    let desktop = Desktop::new().unwrap();
    shell.attach(&desktop);
    desktop.show().unwrap();
    let window = windows.borrow()[0].clone();
    let menu: Rc<RefCell<Option<Menu>>> = Rc::default();
    shell.set_menus({
        let menu = menu.clone();
        let windows = windows.clone();
        move |request: Option<Request>| {
            *menu.borrow_mut() = request.map(|request| {
                let component = DesktopMenu::new().unwrap();
                component.set_for_item(request.item);
                component.set_menu_x(request.x);
                component.set_menu_y(request.y);
                let said: Rc<RefCell<Option<String>>> = Rc::default();
                component.on_chosen({
                    let said = said.clone();
                    move |word| *said.borrow_mut() = Some(word.into())
                });
                component.on_dismissed({
                    let said = said.clone();
                    move || *said.borrow_mut() = Some("dismissed".into())
                });
                component.show().unwrap();
                let window = windows.borrow().last().expect("a window for it").clone();
                Menu {
                    window,
                    component,
                    said,
                }
            });
            Ok(())
        }
    });
    let screen = Screen {
        shell: shell.clone(),
        window,
        menu,
        dir: dir.clone(),
    };
    screen.settle();

    // An empty desktop first.
    screen.offers_both((600.0, 300.0), "an empty desktop");

    for name in ["b.txt", "c.txt", "d.txt"] {
        fs::write(dir.join(name), "").unwrap();
    }
    shell.refresh();
    screen.settle();

    for (spot, what) in [
        ((64.0, 420.0), "below the icons"),
        ((150.0, 80.0), "right of the first icon"),
        ((150.0, 300.0), "right of the last icon"),
        ((2.0, 200.0), "left of the icons"),
        ((1.0, 1.0), "the top left corner"),
        ((1279.0, 1.0), "the top right corner"),
        ((1.0, 719.0), "the bottom left corner"),
        ((1279.0, 719.0), "the bottom right corner"),
        ((640.0, 12.0), "the strip the bar covers"),
        ((640.0, 700.0), "beside the dock"),
    ] {
        screen.offers_both(spot, what);
    }

    // With a menu already open, a press elsewhere puts it away, and the
    // next right-click has one again.
    screen.right(600.0, 300.0);
    screen.right(900.0, 500.0);
    assert!(screen.menu.borrow().is_none(), "the menu stayed");
    screen.offers_both((900.0, 500.0), "after a press beside a menu");
    // After an icon's menu.
    screen.right(icon(0).0, icon(0).1);
    screen.key(&String::from(char::from(Key::Escape)));
    screen.offers_both((700.0, 400.0), "after an icon's menu");
    // After Escape closed a menu.
    screen.right(600.0, 300.0);
    screen.key(&String::from(char::from(Key::Escape)));
    screen.offers_both((600.0, 300.0), "after Escape");

    // A window was opened over the desktop and closed again: the pointer
    // left and the keyboard with it.
    screen.click(icon(1).0, icon(1).1);
    screen.window.dispatch_event(WindowEvent::PointerExited);
    screen
        .window
        .dispatch_event(WindowEvent::WindowActiveChanged(false));
    screen.settle();
    screen.offers_both((640.0, 360.0), "after a window, still inactive");
    screen
        .window
        .dispatch_event(WindowEvent::WindowActiveChanged(true));
    screen.settle();
    screen.offers_both((640.0, 360.0), "after a window was closed");

    // While a name is being typed: it is kept, the menu comes, and an
    // icon's menu acts on that icon though the new name moved it.
    assert_eq!(names(&dir), ["b.txt", "c.txt", "d.txt"]);
    screen.right(600.0, 300.0);
    screen.menu_row((600.0, 300.0), MENU_HEIGHT, 39.0);
    screen.key("a");
    screen.right(icon(1).0, icon(1).1);
    assert_eq!(names(&dir), ["a", "b.txt", "c.txt", "d.txt"]);
    // Move to Trash: under two rows and a line.
    screen.menu_row(icon(1), ITEM_MENU_HEIGHT, 6.0 + 44.0 + 9.0 + 11.0);
    assert_eq!(names(&dir), ["a", "b.txt", "d.txt"]);
    assert!(data.join("Trash/files/c.txt").is_file());

    screen.right(600.0, 300.0);
    screen.menu_row((600.0, 300.0), MENU_HEIGHT, 39.0);
    screen.key("e");
    screen.right(800.0, 500.0);
    assert!(dir.join("e").is_file(), "{:?}", names(&dir));
    let before = screen.count(true);
    screen.menu_row((800.0, 500.0), MENU_HEIGHT, 17.0);
    assert_eq!(screen.count(true), before + 1);
    screen.key(&String::from(char::from(Key::Escape)));

    for (width, height) in [(888, 600), (720, 480)] {
        for light in [false, true] {
            desktop.global::<ThemeSwitch>().set_light(light);
            screen.picture(width, height, light);
        }
    }

    fs::remove_dir_all(root).unwrap();
}
