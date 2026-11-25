//! Every ordinary window gets the Sengi OS frame with its three buttons,
//! also one that asks for client-side decorations and one that never talks
//! to xdg-decoration at all (what Chromium, Firefox and GTK4 may do).

mod common;
mod harness;

use std::time::Duration;

use common::{apps::Picture, Client};
use harness::{Compositor, SOON};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;

const GREEN: u32 = 0xff20_b020;

type Opener = fn(&mut Client) -> Result<(WlSurface, XdgToplevel), common::Failure>;

fn token(name: &str) -> u32 {
    let c = sengi_tokens::Tokens::builtin().color(name).unwrap();
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// The middle of the box around the pixels of one token colour.
fn centre_of(picture: &Picture, name: &str) -> Option<(u32, u32)> {
    let colour = token(name);
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..picture.height {
        for x in 0..picture.width {
            if picture.at(x, y) == colour {
                let b = found.get_or_insert((x, y, x, y));
                *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
    }
    found.map(|(x0, y0, x1, y1)| ((x0 + x1) / 2, (y0 + y1) / 2))
}

fn count(picture: &Picture, argb: u32) -> usize {
    let mut n = 0;
    for y in 0..picture.height {
        for x in 0..picture.width {
            if picture.at(x, y) == argb & 0x00ff_ffff {
                n += 1;
            }
        }
    }
    n
}

fn shot(picture: &Picture, name: &str) {
    if let Some(dir) = std::env::var_os("SENGI_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        picture.write_ppm(&dir.join(name)).unwrap();
    }
}

fn open_client_side(app: &mut Client) -> Result<(WlSurface, XdgToplevel), common::Failure> {
    app.client_side_window(GREEN)
}

fn open_silent(app: &mut Client) -> Result<(WlSurface, XdgToplevel), common::Failure> {
    app.window(GREEN)
}

fn click_at(compositor: &Compositor, at: (u32, u32)) {
    compositor.input(&format!("move {} {}", at.0, at.1));
    compositor.input("click left");
}

fn has_the_buttons(open: Opener, name: &str) {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = open(&mut app).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    let picture = app.capture().unwrap();
    shot(&picture, name);
    for button in [
        "color.window-close",
        "color.window-minimise",
        "color.window-maximise",
    ] {
        assert!(centre_of(&picture, button).is_some(), "no {button}");
    }
    // The window's own pixels are all there below the frame, less the few
    // the pointer may cover.
    assert!(count(&picture, GREEN) >= 300 * 200 - 100);
}

#[test]
fn a_client_side_window_is_told_server_side_and_gets_the_buttons() {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.client_side_window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.more.decoration_mode.is_some()));
    assert_eq!(app.seen.more.decoration_mode, Some(2), "not server side");
    drop(app);
    drop(compositor);
    has_the_buttons(open_client_side, "csd-client-side.ppm");
}

#[test]
fn a_window_that_never_negotiates_gets_the_buttons() {
    has_the_buttons(open_silent, "csd-silent.ppm");
}

fn buttons_work(open: Opener) {
    // Close.
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = open(&mut app).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    let picture = app.capture().unwrap();
    let close = centre_of(&picture, "color.window-close").unwrap();
    let minimise = centre_of(&picture, "color.window-minimise").unwrap();
    let maximise = centre_of(&picture, "color.window-maximise").unwrap();
    click_at(&compositor, close);
    assert!(
        app.wait(SOON, |seen| seen.more.toplevel_closed == 1),
        "close was not sent"
    );
    drop(app);
    drop(compositor);

    // Minimise.
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = open(&mut app).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    click_at(&compositor, minimise);
    app.wait(Duration::from_millis(500), |_| false);
    let after = app.capture().unwrap();
    assert_eq!(count(&after, GREEN), 0, "still on the screen");
    drop(app);
    drop(compositor);

    // Maximise.
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (surface, _toplevel) = open(&mut app).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    click_at(&compositor, maximise);
    assert!(
        app.wait(SOON, |seen| seen
            .more
            .toplevel
            .is_some_and(|(w, _, _)| w > 1000)),
        "not maximised: {:?}",
        app.seen.more.toplevel
    );
    let (w, h, _) = app.seen.more.toplevel.unwrap();
    app.paint(&surface, w, h, GREEN).unwrap();
    shot(&app.capture().unwrap(), "csd-maximised.ppm");
}

#[test]
fn the_buttons_of_a_client_side_window_close_minimise_and_maximise() {
    buttons_work(open_client_side);
}

#[test]
fn the_buttons_of_a_window_that_never_negotiates_close_minimise_and_maximise() {
    buttons_work(open_silent);
}

#[test]
fn a_silent_window_fits_a_small_screen_with_its_frame() {
    let compositor = Compositor::start_args(&["--outputs", "800x600"], None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    let picture = app.capture().unwrap();
    shot(&picture, "csd-small-screen.ppm");
    for button in [
        "color.window-close",
        "color.window-minimise",
        "color.window-maximise",
    ] {
        let (x, y) = centre_of(&picture, button).unwrap_or_else(|| panic!("no {button}"));
        assert!(x < 800 && y < 600);
    }
}
