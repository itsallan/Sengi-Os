//! The accessibility settings as they are drawn: the page itself, the type
//! growing, the high contrast palette and the thicker lines, and reduced
//! motion. The pictures go to SENGI_RENDER_DIR as `a11y-*.png` to be looked
//! at. Each test names its own look, so the environment does not matter.

use sengi_shots::harness::{self, Look, Picture, BACKDROP};
use sengi_shots::{bar, dock, launcher, lock, motion, notify, quick, settings};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

const EXTRA_LARGE: f32 = 1.4;

fn pages() -> ModelRc<settings::PageEntry> {
    let rows = [
        ("network", "Network", "signal"),
        ("wallpaper", "Appearance", "sun"),
        ("mouse", "Mouse & Touchpad", "mouse"),
        ("accessibility", "Accessibility", "person-simple-circle"),
        ("about", "About", "info"),
    ];
    ModelRc::new(VecModel::from(
        rows.iter()
            .map(|(name, title, icon)| settings::PageEntry {
                name: (*name).into(),
                title: (*title).into(),
                icon: (*icon).into(),
            })
            .collect::<Vec<_>>(),
    ))
}

fn accessibility(look: Look, width: u32, height: u32) -> Picture {
    let w = settings::themed_look(settings::SettingsWindow::new().unwrap(), look);
    w.set_pages(pages());
    w.set_page(SharedString::from("accessibility"));
    let d = w.global::<settings::AccessibilityData>();
    d.set_text_size(match look.text_scale {
        s if s > 1.3 => 3,
        s if s > 1.1 => 2,
        s if s < 0.95 => 0,
        _ => 1,
    });
    d.set_high_contrast(look.high_contrast);
    d.set_reduce_motion(look.reduce_motion);
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, width, height);
    harness::advance(400);
    let picture = harness::draw(&win, BACKDROP);
    w.hide().unwrap();
    picture
}

/// How many pixels differ from `fill` in the part of the picture right of
/// the sidebar: a measure of how much is written on the page.
fn ink(picture: &Picture, fill: [u8; 3]) -> usize {
    let mut count = 0;
    for y in 0..picture.height {
        for x in 260..picture.width {
            if picture.at(x, y) != fill {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn the_page_is_drawn_at_every_size_and_look() {
    harness::install();
    let looks = [
        ("default", Look::PLAIN),
        (
            "light",
            Look {
                light: true,
                ..Look::PLAIN
            },
        ),
        ("extra-large", Look::PLAIN.scaled(EXTRA_LARGE)),
        ("small", Look::PLAIN.scaled(0.9)),
        ("high-contrast", Look::HIGH_CONTRAST),
        ("hc-extra-large", Look::HIGH_CONTRAST.scaled(EXTRA_LARGE)),
    ];
    for (name, look) in looks {
        for (width, height) in [(720, 480), (888, 600)] {
            let picture = accessibility(look, width, height);
            picture.save_as(&format!("a11y-settings-{name}-{width}x{height}.png"));
            assert!(picture.width == width as usize);
        }
    }
}

#[test]
fn bigger_text_is_more_ink_on_the_page_and_smaller_text_less() {
    harness::install();
    let fill = Look::PLAIN;
    let page = harness::page_colour_of(fill);
    let plain = ink(&accessibility(fill, 888, 600), page);
    let large = ink(&accessibility(fill.scaled(1.2), 888, 600), page);
    let huge = ink(&accessibility(fill.scaled(EXTRA_LARGE), 888, 600), page);
    let small = ink(&accessibility(fill.scaled(0.9), 888, 600), page);
    assert!(small < plain, "small {small} against plain {plain}");
    assert!(large > plain, "large {large} against plain {plain}");
    assert!(huge > large, "extra large {huge} against large {large}");
}

#[test]
fn high_contrast_is_black_with_white_text_and_two_pixel_lines() {
    harness::install();
    let picture = accessibility(Look::HIGH_CONTRAST, 888, 600);
    // The page and the corner of the window are pure black.
    assert_eq!(picture.at(600, 2), [0, 0, 0]);
    let white = (0..picture.height)
        .flat_map(|y| (260..picture.width).map(move |x| (x, y)))
        .filter(|&(x, y)| picture.at(x, y) == [255, 255, 255])
        .count();
    assert!(white > 200, "only {white} pure white pixels");
    // Down the middle of the column: the first white line is the top
    // edge of the first card, two pixels thick.
    let x = 200 + (888 - 200) / 2;
    let top = (0..picture.height)
        .find(|&y| picture.at(x, y) == [255, 255, 255])
        .expect("a card has an edge");
    let thick = (top..picture.height)
        .take_while(|&y| picture.at(x, y) == [255, 255, 255])
        .count();
    assert_eq!(thick, 2, "the card edge starting at y={top}");
    // The same edge is one pixel in the dark theme.
    let dark = accessibility(Look::PLAIN, 888, 600);
    let border = [0x3a, 0x3d, 0x44];
    let top = (0..dark.height)
        .find(|&y| dark.at(x, y) == border)
        .expect("a card has an edge");
    let thick = (top..dark.height)
        .take_while(|&y| dark.at(x, y) == border)
        .count();
    assert_eq!(thick, 1);
}

#[test]
fn reduced_motion_puts_things_where_they_are_going_at_once() {
    harness::install();
    // A square sent 100 px away, and a patch changing colour, with the
    // durations of the tokens.
    let run = |look: Look| {
        let w = motion::themed_look(motion::MotionWindow::new().unwrap(), look);
        w.show().unwrap();
        harness::advance(100);
        assert_eq!(w.get_position(), 0.0);
        w.set_moved(true);
        // Reading starts the move; two frames in, it is on its way.
        let start = w.get_position();
        harness::advance(32);
        let underway = w.get_position();
        harness::advance(600);
        let end = w.get_position();
        w.hide().unwrap();
        (start, underway, end)
    };
    let (start, underway, end) = run(Look::PLAIN);
    assert_eq!(end, 100.0);
    assert!(start < 100.0 && underway > start && underway < 100.0);
    let reduced = Look {
        reduce_motion: true,
        ..Look::PLAIN
    };
    let (start, underway, end) = run(reduced);
    assert_eq!((start, underway, end), (100.0, 100.0, 100.0));
}

/// The programs of the desktop at the largest text size, drawn so that
/// they can be looked at; each one is checked to have kept inside its
/// window by the size of the picture and by being drawn at all.
#[test]
fn the_desktop_programs_at_extra_large() {
    harness::install();
    let look = Look::PLAIN.scaled(EXTRA_LARGE);
    for (name, look) in [
        ("extra-large", look),
        ("hc-extra-large", Look::HIGH_CONTRAST.scaled(EXTRA_LARGE)),
    ] {
        let w = bar::themed_look(bar::Bar::new().unwrap(), look);
        w.show().unwrap();
        let win = harness::window();
        harness::resize(&win, 1366, 40);
        harness::advance(300);
        let picture = harness::draw(&win, BACKDROP);
        picture.save_as(&format!("a11y-bar-{name}.png"));
        w.hide().unwrap();

        let w = dock::themed_look(dock::Dock::new().unwrap(), look);
        w.show().unwrap();
        let win = harness::window();
        harness::resize(&win, 1366, 120);
        harness::advance(300);
        harness::draw(&win, BACKDROP).save_as(&format!("a11y-dock-{name}.png"));
        w.hide().unwrap();

        let w = launcher::themed_look(launcher::LauncherWindow::new().unwrap(), look);
        w.show().unwrap();
        let win = harness::window();
        harness::resize(&win, 800, 600);
        harness::advance(300);
        harness::draw(&win, BACKDROP).save_as(&format!("a11y-launcher-{name}.png"));
        w.hide().unwrap();

        let w = quick::themed_look(quick::QuickWindow::new().unwrap(), look);
        w.show().unwrap();
        let win = harness::window();
        harness::resize(&win, 420, 640);
        harness::advance(300);
        harness::draw(&win, BACKDROP).save_as(&format!("a11y-quick-{name}.png"));
        w.hide().unwrap();

        let w = notify::themed_look(notify::NotifyWindow::new().unwrap(), look);
        w.show().unwrap();
        let win = harness::window();
        harness::resize(&win, 420, 200);
        harness::advance(300);
        harness::draw(&win, BACKDROP).save_as(&format!("a11y-notify-{name}.png"));
        w.hide().unwrap();

        let w = lock::themed_look(lock::LockWindow::new().unwrap(), look);
        w.set_time("9:41".into());
        w.set_date("Tuesday, 25 November".into());
        w.set_user("Allan Smith".into());
        w.set_placeholder("Password".into());
        w.set_length(6);
        w.show().unwrap();
        let win = harness::window();
        harness::resize(&win, 1366, 768);
        harness::advance(300);
        harness::draw(&win, BACKDROP).save_as(&format!("a11y-lock-{name}.png"));
        w.hide().unwrap();
    }
}
