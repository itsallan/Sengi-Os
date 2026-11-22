//! Window management against the running compositor: placement, snapping
//! and maximising by the keys the image's labwc has, checked by the sizes
//! the window is told and by the pixels of a picture of the screen.

mod common;
mod harness;

use std::time::Duration;

use common::Client;
use harness::{Compositor, SOON};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};

const GREEN: u32 = 0xff20_b020;
const RED: u32 = 0xffb0_2020;

fn rgb(argb: u32) -> u32 {
    argb & 0x00ff_ffff
}

/// Not a test of its own: the compositor starts this as the screen locker,
/// see `harness::hand_over`.
#[test]
fn locker_stand_in() {
    harness::hand_over();
}

/// The size the window was last told, once the compositor has told it.
fn told(app: &Client) -> Option<(i32, i32)> {
    app.seen.more.toplevel.map(|(w, h, _)| (w, h))
}

#[test]
fn super_left_fills_the_left_half_of_the_screen() {
    let script = "1500 key super+left\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (surface, _toplevel) = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    // The key comes at 1.5 s; the snap is then told to the window.
    assert!(
        app.wait(Duration::from_secs(4), |seen| seen
            .more
            .toplevel
            .is_some_and(|(w, _, _)| w > 300 && w < 700)),
        "not snapped to half the screen: {:?}",
        app.seen.more.toplevel
    );
    let (w, h) = told(&app).unwrap();
    assert!(h > 400 && h < 720, "height {h}");
    app.paint(&surface, w, h, GREEN).unwrap();
    let picture = app.capture().unwrap();
    assert_eq!(picture.at(5, 360), rgb(GREEN), "not on the left");
    assert_ne!(picture.at(1270, 360), rgb(GREEN), "it covers the right");
}

#[test]
fn super_a_maximises_and_again_gives_the_size_back() {
    let script = "1500 key super+a\n2800 key super+a\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    assert!(
        app.wait(Duration::from_secs(4), |seen| seen
            .more
            .toplevel
            .is_some_and(|(w, _, _)| w > 1000)),
        "not maximised: {:?}",
        app.seen.more.toplevel
    );
    assert!(
        app.wait(Duration::from_secs(4), |seen| seen
            .more
            .toplevel
            .is_some_and(|(w, _, _)| w == 300)),
        "not back to its own size: {:?}",
        app.seen.more.toplevel
    );
}

/// A window whose surface is created here, not by `Client::window`, so
/// that its first buffer can be of any size.
fn bare_window(app: &mut Client) -> Result<(WlSurface, XdgSurface, XdgToplevel), common::Failure> {
    let wm_base: XdgWmBase = app.globals.bind(&app.handle, 1..=2, ())?;
    let surface = app.compositor.create_surface(&app.handle, ());
    let xdg = wm_base.get_xdg_surface(&surface, &app.handle, ());
    let toplevel = xdg.get_toplevel(&app.handle, ());
    surface.commit();
    Ok((surface, xdg, toplevel))
}

#[test]
fn a_window_too_large_for_the_screen_is_asked_to_fit_it() {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    let (surface, _xdg, _toplevel) = bare_window(&mut app).unwrap();
    assert!(app.wait(SOON, |seen| seen.window_configures >= 1));
    // It draws bigger than the screen, the way some clients do.
    let buffer = app.buffer(1400, 900, GREEN).unwrap();
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    assert!(
        app.wait(SOON, |seen| seen
            .more
            .toplevel
            .is_some_and(|(w, _, _)| w > 0)),
        "it was never told a size: {:?}",
        app.seen.more.toplevel
    );
    let (w, h) = told(&app).unwrap();
    assert!(
        w <= 1280 && h <= 720,
        "told to be {w}x{h}, which does not fit the screen"
    );
}

#[test]
fn a_window_that_asks_for_fullscreen_before_it_is_shown_is_not_fitted() {
    let compositor = Compositor::start(None);
    let mut shell = compositor.connect();
    let _bar = shell.bar(30, RED).unwrap();
    let mut app = compositor.connect();
    let (surface, _xdg, toplevel) = bare_window(&mut app).unwrap();
    // Fullscreen before its first buffer: the whole screen, bar and all.
    toplevel.set_fullscreen(None);
    assert!(app.wait(SOON, |seen| seen
        .more
        .toplevel
        .is_some_and(|(_, _, full)| full)));
    let buffer = app.buffer(1280, 720, GREEN).unwrap();
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    let picture = app.capture().unwrap();
    assert_eq!(
        picture.at(5, 5),
        rgb(GREEN),
        "the bar is over the fullscreen window"
    );
    assert_eq!(picture.at(640, 360), rgb(GREEN));
}
