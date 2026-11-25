//! The sheet of keyboard shortcuts, against the running compositor: Super
//! and / opens it, Escape or a click closes it, it is not there on the lock
//! screen, and what it lists is what the compositor binds (that is checked
//! in `keys.rs` and `card.rs`, from the same table).

mod common;
mod harness;

use common::apps::Picture;
use harness::{Compositor, SOON};

/// Not a test of its own: the compositor starts this test binary as the
/// screen locker when a test asks for one, see `harness::hand_over`.
#[test]
#[ignore = "started by the compositor as the locker"]
fn locker_stand_in() {
    harness::hand_over();
}

fn token(name: &str) -> u32 {
    let c = sengi_tokens::Tokens::builtin().color(name).unwrap();
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

fn shot(picture: &Picture, name: &str) {
    if let Some(dir) = std::env::var_os("SENGI_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        picture.write_ppm(&dir.join(name)).unwrap();
    }
}

/// How many pixels of the picture, in a block at its middle, are the
/// colour of a card's surface.
fn card_pixels(picture: &Picture) -> usize {
    let surface = token("color.surface");
    let (cx, cy) = (picture.width / 2, picture.height / 2);
    let mut count = 0;
    for y in cy - 100..cy + 100 {
        for x in cx - 150..cx + 150 {
            if picture.at(x, y) == surface {
                count += 1;
            }
        }
    }
    count
}

fn overlay(compositor: &Compositor) -> String {
    compositor.status_line("overlay:")
}

#[test]
fn super_slash_shows_the_sheet_and_escape_takes_it_away() {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    assert!(overlay(&compositor).contains("none"));
    let plain = app.capture().unwrap();
    assert_eq!(card_pixels(&plain), 0, "a card on a plain desktop");

    compositor.input("key super+/");
    assert!(overlay(&compositor).contains("shortcuts"), "not open");
    let open = app.capture().unwrap();
    shot(&open, "shortcuts-sheet.ppm");
    assert!(
        card_pixels(&open) > 20_000,
        "the sheet is not in the middle of the screen: {} pixels",
        card_pixels(&open)
    );

    // Said again, it closes.
    compositor.input("key super+/");
    assert!(
        overlay(&compositor).contains("none"),
        "not closed by the key"
    );

    compositor.input("key super+/");
    assert!(overlay(&compositor).contains("shortcuts"));
    compositor.input("key esc");
    assert!(
        overlay(&compositor).contains("none"),
        "Escape did not close it"
    );
    let gone = app.capture().unwrap();
    assert_eq!(card_pixels(&gone), 0, "the sheet is still drawn");
}

#[test]
fn a_click_closes_the_sheet_and_goes_no_further() {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _pointer = app.pointer().unwrap();
    let _window = app.window(0xff20_b020).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    compositor.input("key super+/");
    assert!(overlay(&compositor).contains("shortcuts"));
    compositor.input("move 640 360");
    compositor.input("press left");
    compositor.input("release left");
    assert!(overlay(&compositor).contains("none"));
    app.roundtrip().unwrap();
    assert_eq!(
        app.seen.more.button_serial, None,
        "the click that closed the sheet reached the window under it"
    );
}

#[test]
fn the_settings_button_asks_for_it_and_asking_twice_leaves_it_open() {
    let compositor = Compositor::start(None);
    let status = compositor.ask_with("--shortcuts").wait().unwrap();
    assert!(status.success(), "{status}");
    assert!(overlay(&compositor).contains("shortcuts"));
    let status = compositor.ask_with("--shortcuts").wait().unwrap();
    assert!(status.success(), "{status}");
    assert!(
        overlay(&compositor).contains("shortcuts"),
        "the second request closed it"
    );
}

#[test]
fn there_is_no_sheet_on_the_lock_screen() {
    let compositor = Compositor::start(None);
    let mut locker = compositor.locker();
    locker.keyboard().unwrap();
    let lock = locker.lock().unwrap();
    let output = locker.outputs().remove(0);
    let _surface = locker.lock_surface(&lock, &output, 0xffb0_2020).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1));
    compositor.input("key super+/");
    assert!(overlay(&compositor).contains("none"));
    let status = compositor.ask_with("--shortcuts").wait().unwrap();
    assert!(status.success());
    assert!(
        overlay(&compositor).contains("none"),
        "opened over the lock"
    );
}
