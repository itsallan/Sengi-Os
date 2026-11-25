//! The light theme: the main screens are drawn with it and searched for
//! pieces still in dark colours. Text is dark on a light surface, so the
//! search is for blocks too big to be a letter, with no light pixel in them.
//! The pictures go to SENGI_RENDER_DIR as `light-*.png` to be looked at.

use sengi_shots::harness::{self, Picture};
use sengi_shots::{bar, desktop, dock, files, greeter, installer, launcher, lock, notify, osd};
use sengi_shots::{quick, settings, store};
use slint::ComponentHandle;

/// The plain colour behind the desktop in the light theme, under the
/// translucent bar and dock.
const BACKDROP: [u8; 3] = [0xdc, 0xdf, 0xe4];

fn luminance(p: [u8; 3]) -> u32 {
    (2126 * u32::from(p[0]) + 7152 * u32::from(p[1]) + 722 * u32::from(p[2])) / 10_000
}

/// The top left corner of a `block` by `block` square of dark pixels.
fn dark_block(picture: &Picture, block: usize) -> Option<(usize, usize)> {
    let dark = |x: usize, y: usize| luminance(picture.at(x, y)) < 110;
    let mut y = 0;
    while y + block <= picture.height {
        let mut x = 0;
        while x + block <= picture.width {
            let flat = (0..block).all(|dy| (0..block).all(|dx| dark(x + dx, y + dy)));
            if flat {
                return Some((x, y));
            }
            x += block / 2;
        }
        y += block / 2;
    }
    None
}

fn look<T: ComponentHandle>(w: &T, width: u32, height: u32, name: &str, block: usize) {
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, width, height);
    harness::advance(400);
    let picture = harness::draw(&win, BACKDROP);
    picture.save(&format!("light-{name}.png"));
    if let Some((x, y)) = dark_block(&picture, block) {
        panic!("{name}: a dark area is left at {x},{y}");
    }
    // The page itself is light: its corner is not a dark fill.
    assert!(luminance(picture.at(width as usize / 2, 1)) > 110, "{name}");
    w.hide().unwrap();
}

#[test]
fn the_main_screens_have_nothing_dark_left() {
    harness::install();

    let w = lock::themed_as(lock::LockWindow::new().unwrap(), true);
    w.set_time("9:41".into());
    w.set_date("Tuesday, 25 November".into());
    w.set_user("Allan Smith".into());
    w.set_placeholder("Password".into());
    w.set_length(6);
    look(&w, 1366, 768, "lock", 32);

    let w = greeter::themed_as(greeter::GreeterWindow::new().unwrap(), true);
    w.set_time("9:41".into());
    w.set_date("Tuesday, 25 November".into());
    w.set_user("Allan Smith".into());
    w.set_placeholder("Password".into());
    look(&w, 1366, 768, "greeter", 32);

    let w = bar::themed_as(bar::Bar::new().unwrap(), true);
    look(&w, 1366, 28, "bar", 12);

    let w = dock::themed_as(dock::Dock::new().unwrap(), true);
    look(&w, 1366, 120, "dock", 16);

    let w = launcher::themed_as(launcher::LauncherWindow::new().unwrap(), true);
    look(&w, 800, 600, "launcher", 32);

    let w = quick::themed_as(quick::QuickWindow::new().unwrap(), true);
    look(&w, 400, 600, "quick", 32);

    let w = notify::themed_as(notify::NotifyWindow::new().unwrap(), true);
    look(&w, 400, 300, "notify", 32);

    let w = osd::themed_as(osd::OsdWindow::new().unwrap(), true);
    look(&w, 240, 56, "osd", 12);

    let w = store::themed_as(store::StoreWindow::new().unwrap(), true);
    look(&w, 888, 600, "store", 32);

    let w = files::themed_as(files::FilesWindow::new().unwrap(), true);
    look(&w, 888, 600, "files", 32);

    let w = desktop::themed_as(desktop::Desktop::new().unwrap(), true);
    look(&w, 800, 600, "desktop", 32);

    let w = installer::themed_as(installer::InstallerWindow::new().unwrap(), true);
    look(&w, 960, 640, "installer", 32);

    for page in ["about", "keyboard", "power", "display"] {
        let w = settings::themed_as(settings::SettingsWindow::new().unwrap(), true);
        w.set_page(page.into());
        look(&w, 888, 600, &format!("settings-{page}"), 32);
    }
}

#[test]
fn the_dark_theme_is_dark_by_the_same_measure() {
    // The check above has teeth: the same screen in the dark theme fails it.
    harness::install();
    let w = lock::themed_as(lock::LockWindow::new().unwrap(), false);
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, 800, 600);
    harness::advance(400);
    let picture = harness::draw(&win, BACKDROP);
    assert!(dark_block(&picture, 32).is_some());
    w.hide().unwrap();
}
