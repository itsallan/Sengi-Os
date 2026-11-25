//! The snap hint, checked by the pixels of a picture of the screen: while a
//! window is dragged to the edge of a screen an outline shows where it
//! will land, it follows the pointer away again, and it is gone when the
//! window is let go.

mod common;
mod harness;

use common::{apps::Picture, Client};
use harness::{Compositor, SOON};
use std::time::Duration;

const GREEN: u32 = 0xff20_b020;

fn rgb(argb: u32) -> u32 {
    argb & 0x00ff_ffff
}

fn token(name: &str) -> u32 {
    let c = sengi_tokens::Tokens::builtin().color(name).unwrap();
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

fn green_of(pixel: u32) -> u32 {
    pixel >> 8 & 0xff
}

fn shot(picture: &Picture, name: &str) {
    if let Some(dir) = std::env::var_os("SENGI_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        picture.write_ppm(&dir.join(name)).unwrap();
    }
}

/// The top left and bottom right pixel of the window's client area.
fn client_area(picture: &Picture) -> (u32, u32, u32, u32) {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..picture.height {
        for x in 0..picture.width {
            if picture.at(x, y) == rgb(GREEN) {
                let b = found.get_or_insert((x, y, x, y));
                *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
    }
    found.expect("the window in the picture")
}

/// A compositor with one framed window, a picture of the screen, and the
/// place on the window's title bar to take hold of.
fn framed() -> (Compositor, Client, Picture, (u32, u32)) {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.decorated_window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    let picture = app.capture().unwrap();
    let (x0, y0, x1, _) = client_area(&picture);
    // The title bar is the strip above the client area.
    let grab = ((x0 + x1) / 2, y0 - 10);
    (compositor, app, picture, grab)
}

/// Takes hold of the title bar and drags to `to`, in steps.
fn drag(compositor: &Compositor, from: (u32, u32), to: &[(u32, u32)]) {
    compositor.input(&format!("move {} {}", from.0, from.1));
    compositor.input("press left");
    for (x, y) in to {
        compositor.input(&format!("move {x} {y}"));
    }
}

#[test]
fn an_outline_shows_where_a_window_dragged_to_the_left_edge_will_land() {
    let (compositor, mut app, before, grab) = framed();
    assert!(compositor.status_line("snap hint:").contains("none"));

    drag(&compositor, grab, &[(640, 300), (200, 300), (0, 300)]);
    let said = compositor.status_line("snap hint:");
    assert!(!said.contains("none"), "no outline: {said}");
    let during = app.capture().unwrap();
    shot(&during, "snap-hint-left.ppm");

    // Where the left half of the screen will be, clear of the window being
    // dragged, the desktop is tinted with the accent.
    let (x, y) = (300, 600);
    let (was, is) = (before.at(x, y), during.at(x, y));
    assert_eq!(was, token("color.desktop"), "the desktop was not plain");
    assert_ne!(is, was, "no outline over the left half");
    assert!(
        green_of(is) > green_of(was) + 15,
        "the outline is not the accent: {was:06x} became {is:06x}"
    );
    // It is a fill, not a solid: the desktop shows through.
    assert!(green_of(is) < 0x80, "{is:06x} is not see-through");
    // The right half is left alone.
    assert_eq!(
        during.at(900, 600),
        was,
        "the outline reaches the right half"
    );

    // Let go: the outline is gone, and the window is where it showed.
    compositor.input("release left");
    assert!(compositor.status_line("snap hint:").contains("none"));
    assert!(
        app.wait(Duration::from_secs(3), |seen| seen
            .more
            .toplevel
            .is_some_and(|(w, _, _)| w > 300 && w < 700)),
        "the window was not snapped to the left half: {:?}",
        app.seen.more.toplevel
    );
}

#[test]
fn the_outline_follows_the_pointer_and_goes_when_it_leaves_the_edge() {
    let (compositor, mut app, _, grab) = framed();
    drag(&compositor, grab, &[(640, 300), (1279, 300)]);
    let right = compositor.status_line("snap hint:");
    assert!(!right.contains("none"), "{right}");
    // On the right half: it starts past the middle.
    let x: i32 = right
        .split(" at ")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .and_then(|x| x.parse().ok())
        .expect("a place in the report");
    assert!(x > 500, "the outline is on the left: {right}");
    let picture = app.capture().unwrap();
    shot(&picture, "snap-hint-right.ppm");

    // Away from the edge again: nothing is shown, and letting go there
    // snaps nothing.
    compositor.input("move 640 300");
    assert!(compositor.status_line("snap hint:").contains("none"));
    compositor.input("release left");
    app.wait(Duration::from_millis(500), |_| false);
    assert!(
        app.seen
            .more
            .toplevel
            .is_none_or(|(w, _, _)| w == 0 || w == 300),
        "the window was snapped from the middle of the screen: {:?}",
        app.seen.more.toplevel
    );
}

#[test]
fn the_top_edge_shows_the_whole_screen_and_a_corner_the_top_wins() {
    let (compositor, mut app, _, grab) = framed();
    drag(&compositor, grab, &[(640, 300), (640, 0)]);
    let top = compositor.status_line("snap hint:");
    assert!(top.contains("1280x"), "not the width of the screen: {top}");
    let picture = app.capture().unwrap();
    shot(&picture, "snap-hint-top.ppm");

    // The top left corner is the top edge as well.
    compositor.input("move 0 0");
    let corner = compositor.status_line("snap hint:");
    assert!(corner.contains("1280x"), "{corner}");
    compositor.input("release left");
    assert!(compositor.status_line("snap hint:").contains("none"));
}

#[test]
fn a_window_that_is_not_dragged_by_its_frame_shows_no_outline() {
    // Pressing on the client area and moving does not move the window.
    let (compositor, _app, _, _) = framed();
    compositor.input("move 640 300");
    compositor.input("press left");
    compositor.input("move 0 300");
    assert!(compositor.status_line("snap hint:").contains("none"));
    compositor.input("release left");
}
