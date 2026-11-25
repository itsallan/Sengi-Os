//! The shadow around a framed window, checked by the pixels of a picture of
//! the screen: it is there under a focused window, lighter under one that
//! is not focused, gone under one that fills the screen, follows a window
//! as it moves, and can be switched off.

mod common;
mod harness;

use common::{apps::Picture, Client};
use std::time::Duration;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;

use harness::{Compositor, SOON};

const GREEN: u32 = 0xff20_b020;
const RED: u32 = 0xffb0_2020;
const OFF: &str = "[appearance]\nwindow-shadows = false\n";

fn rgb(argb: u32) -> u32 {
    argb & 0x00ff_ffff
}

fn desktop() -> u32 {
    let c = sengi_tokens::Tokens::builtin()
        .color("color.desktop")
        .unwrap();
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// How light a pixel is: its three channels added.
fn light(pixel: u32) -> u32 {
    (pixel >> 16 & 0xff) + (pixel >> 8 & 0xff) + (pixel & 0xff)
}

/// The top left and bottom right pixel of the client area of one colour.
fn client(picture: &Picture, colour: u32) -> (u32, u32, u32, u32) {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..picture.height {
        for x in 0..picture.width {
            if picture.at(x, y) == rgb(colour) {
                let b = found.get_or_insert((x, y, x, y));
                *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
    }
    found.expect("the window's colour in the picture")
}

fn shot(picture: &Picture, name: &str) {
    if let Some(dir) = std::env::var_os("SENGI_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        picture.write_ppm(&dir.join(name)).unwrap();
    }
}

type Window = (WlSurface, XdgToplevel);

fn one_window(settings: Option<&str>) -> (Compositor, Client, Window, Picture) {
    let compositor = Compositor::start_with(None, settings, None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let window = app.decorated_window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    let picture = app.capture().unwrap();
    (compositor, app, window, picture)
}

/// How light the picture is `below` pixels under the window's bottom edge,
/// at its middle.
fn under(picture: &Picture, below: u32) -> u32 {
    let (x0, _, x1, y1) = client(picture, GREEN);
    light(picture.at((x0 + x1) / 2, y1 + below))
}

#[test]
fn a_focused_window_has_a_shadow_under_it() {
    let (_compositor, _app, _window, picture) = one_window(None);
    shot(&picture, "shadow-focused.ppm");
    let (x0, _, x1, y1) = client(&picture, GREEN);
    let middle = (x0 + x1) / 2;
    let under = picture.at(middle, y1 + 4);
    let far = picture.at(middle, y1 + 80);
    assert_eq!(far, desktop(), "far away it is the desktop");
    assert!(light(under) < light(desktop()), "{under:06x} is not darker");
    // It gets lighter the further it is from the window.
    let mut last = 0;
    for step in [4, 10, 16, 22, 28] {
        let now = light(picture.at(middle, y1 + step));
        assert!(now >= last, "not lighter at {step}");
        last = now;
    }
}

#[test]
fn the_shadow_is_lighter_when_the_window_is_not_focused() {
    let (compositor, _app, _window, focused) = one_window(None);
    // A second window takes the focus. It cascades down and right, so the
    // first one's left side stays clear.
    let mut other = compositor.connect();
    other.keyboard().unwrap();
    let _second = other.decorated_window(RED).unwrap();
    assert!(other.wait(SOON, |seen| seen.keyboard_enters == 1));
    let unfocused = other.capture().unwrap();
    shot(&unfocused, "shadow-unfocused.ppm");
    let (x0, y0, _, y1) = client(&focused, GREEN);
    let (x1, ..) = client(&unfocused, GREEN);
    assert_eq!(x0, x1, "the first window did not move");
    let side = (x0 - 6, (y0 + y1) / 2);
    let with_focus = light(focused.at(side.0, side.1));
    let without = light(unfocused.at(side.0, side.1));
    assert!(with_focus < light(desktop()), "no shadow at the side");
    assert!(without < light(desktop()), "no shadow when unfocused");
    assert!(
        without > with_focus,
        "unfocused {without} is not lighter than focused {with_focus}"
    );
}

#[test]
fn a_framed_window_that_fills_half_the_screen_keeps_its_shadow() {
    let script = "1500 key super+left\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (surface, _toplevel) = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    assert!(app.wait(Duration::from_secs(4), |seen| seen
        .more
        .toplevel
        .is_some_and(|(w, _, _)| w > 300 && w < 700)));
    let (w, h, _) = app.seen.more.toplevel.unwrap();
    app.paint(&surface, w, h, GREEN).unwrap();
    let picture = app.capture().unwrap();
    shot(&picture, "shadow-snapped.ppm");
    let (_, _, right, _) = client(&picture, GREEN);
    // Past the one pixel border, in the gap to the other half.
    assert!(
        light(picture.at(right + 2, 360)) < light(desktop()),
        "no shadow past the snapped window"
    );
}

#[test]
fn the_shadow_follows_the_window_when_it_is_moved() {
    let (compositor, mut app, _window, before) = one_window(None);
    assert!(under(&before, 4) < light(desktop()));
    let (x0, y0, x1, y1) = client(&before, GREEN);
    // Drag the title bar down and to the left.
    compositor.input(&format!("move {} {}", x0 + 150, y0 - 20));
    compositor.input("press left");
    compositor.input(&format!("move {} {}", x0 + 100, y0 + 40));
    compositor.input(&format!("move {} {}", x0 - 50, y0 + 100));
    compositor.input("release left");
    let after = app.capture().unwrap();
    shot(&after, "shadow-moved.ppm");
    let (nx0, ny0, nx1, ny1) = client(&after, GREEN);
    assert!(nx0 < x0 && ny0 > y0, "the window did not move");
    assert_eq!((nx1 - nx0, ny1 - ny0), (x1 - x0, y1 - y0));
    assert!(
        under(&after, 4) < light(desktop()),
        "no shadow at the new place"
    );
    // Where the window was, the desktop is back, shadow and all.
    let old = (x0 + x1) / 2;
    assert_eq!(after.at(old, y1 + 4), desktop());
    assert_eq!(after.at(old, y1 + 12), desktop());
}

#[test]
fn it_is_gone_when_the_window_is_closed() {
    let (_compositor, mut app, (surface, toplevel), before) = one_window(None);
    assert!(under(&before, 4) < light(desktop()));
    let (x0, _, x1, y1) = client(&before, GREEN);
    toplevel.destroy();
    surface.destroy();
    let after = app.capture().unwrap();
    for (x, y) in [((x0 + x1) / 2, y1 + 4), (x0 - 4, y1 + 2), (x1 + 4, y1 + 2)] {
        assert_eq!(after.at(x, y), desktop(), "left over at {x},{y}");
    }
}

#[test]
fn shadows_can_be_switched_off_in_the_settings() {
    let (compositor, mut app, _window, picture) = one_window(Some(OFF));
    shot(&picture, "shadow-off.ppm");
    for below in [2, 4, 8, 16] {
        assert_eq!(under(&picture, below), light(desktop()), "{below} below");
    }
    // And on again, at the next SIGHUP.
    reload(&compositor, "", 1);
    let on = app.capture().unwrap();
    assert!(under(&on, 4) < light(desktop()), "not back on");
    reload(&compositor, OFF, 2);
    let off = app.capture().unwrap();
    assert_eq!(under(&off, 4), light(desktop()), "not off again");
}

/// Writes the settings and has the compositor read them, the `count`th time.
fn reload(compositor: &Compositor, settings: &str, count: usize) {
    compositor.write_settings(settings);
    compositor.signal("HUP");
    for _ in 0..100 {
        if compositor.said_all("settings read again").len() >= count {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the settings were not read again");
}

#[test]
fn a_window_across_two_screens_has_its_shadow_on_both() {
    let compositor = Compositor::start(None);
    let name = compositor.plug("1280x720");
    assert!(name.starts_with("HEADLESS"), "{name}");
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.decorated_window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    let outputs = app.outputs();
    let before = app.capture_of(&outputs[0], 0).unwrap();
    let (x0, y0, ..) = client(&before, GREEN);
    // Drag it over the seam between the screens.
    compositor.input(&format!("move {} {}", x0 + 150, y0 - 20));
    compositor.input("press left");
    compositor.input(&format!("move {} {}", x0 + 200, y0 - 20));
    compositor.input("move 1200 280");
    compositor.input("release left");
    let left = app.capture_of(&outputs[0], 0).unwrap();
    let right = app.capture_of(&outputs[1], 0).unwrap();
    shot(&left, "shadow-two-left.ppm");
    shot(&right, "shadow-two-right.ppm");
    let (lx0, _, _, ly1) = client(&left, GREEN);
    let (rx0, _, rx1, ry1) = client(&right, GREEN);
    assert_eq!(ly1, ry1, "one window, one bottom edge");
    assert!(lx0 > 1000 && rx0 == 0, "the window is not across the seam");
    // Under the part on each screen it is dark, past the edge of the
    // window on the right screen it is again the desktop.
    assert!(light(left.at(1270, ly1 + 4)) < light(desktop()));
    assert!(light(right.at(rx0 + 10, ry1 + 4)) < light(desktop()));
    assert_eq!(right.at(rx1 + 40, ry1 + 4), desktop());
}

/// The median time of a full frame with one window, drawn from scratch
/// and read back, which is what a picture of the screen is.
fn frame_time(settings: Option<&str>) -> Duration {
    let (_compositor, mut app, _window, _) = one_window(settings);
    let mut times: Vec<Duration> = (0..40)
        .map(|_| {
            let started = std::time::Instant::now();
            app.capture().unwrap();
            started.elapsed()
        })
        .collect();
    times.sort();
    times[times.len() / 2]
}

#[test]
fn shadows_add_little_to_the_cost_of_a_frame() {
    let without = frame_time(Some(OFF));
    let with = frame_time(None);
    eprintln!("a 1280x720 frame with one window: {without:?} without shadows, {with:?} with");
    assert!(
        with < without + Duration::from_millis(20),
        "shadows cost {:?} a frame",
        with.saturating_sub(without)
    );
}
