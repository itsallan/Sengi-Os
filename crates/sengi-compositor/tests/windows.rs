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

/// Not run by a plain `cargo test`: the compositor starts this test binary
/// as the screen locker, with `--include-ignored`, see `harness::hand_over`.
#[test]
#[ignore = "started by the compositor as the locker"]
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
fn a_window_too_large_for_a_small_screen_is_shown_once_it_has_shrunk() {
    let compositor = Compositor::start_args(&["--outputs", "800x600"], None);
    let mut app = compositor.connect();
    let (surface, _xdg, _toplevel) = bare_window(&mut app).unwrap();
    assert!(app.wait(SOON, |seen| seen.window_configures >= 1));
    // Wider than the whole screen, as Settings and the terminal are.
    let buffer = app.buffer(890, 630, GREEN).unwrap();
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    assert!(
        app.wait(SOON, |seen| seen
            .more
            .toplevel
            .is_some_and(|(w, _, _)| w > 0)),
        "it was never told a size"
    );
    let (w, h) = told(&app).unwrap();
    // The client does as it was told and draws that size.
    app.paint(&surface, w, h, GREEN).unwrap();
    let picture = app.capture().unwrap();
    assert_eq!(
        picture.at(400, 300),
        rgb(GREEN),
        "the window was told {w}x{h} and is not on the screen"
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

/// The pixels of a window dragged with Super held, released at the top
/// edge, which maximises it.
#[test]
fn a_window_let_go_at_the_top_edge_is_maximised() {
    let script = "1500 keydown super\n1600 move 640 360\n1700 press left\n2000 move 640 0\n2200 release left\n2400 keyup super\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.window(GREEN).unwrap();
    assert!(
        app.wait(Duration::from_secs(5), |seen| seen
            .more
            .toplevel
            .is_some_and(|(w, _, _)| w > 1000)),
        "not maximised by letting go at the top: {:?}",
        app.seen.more.toplevel
    );
}

#[test]
fn a_window_let_go_at_the_left_edge_takes_the_left_half() {
    let script = "1500 keydown super\n1600 move 640 360\n1700 press left\n2000 move 0 360\n2200 release left\n2400 keyup super\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (surface, _toplevel) = app.window(GREEN).unwrap();
    assert!(app.wait(Duration::from_secs(5), |seen| seen
        .more
        .toplevel
        .is_some_and(|(w, _, _)| w > 300 && w < 700)));
    let (w, h, _) = app.seen.more.toplevel.unwrap();
    app.paint(&surface, w, h, GREEN).unwrap();
    let picture = app.capture().unwrap();
    assert_eq!(picture.at(5, 360), rgb(GREEN), "not on the left");
    assert_ne!(picture.at(1270, 360), rgb(GREEN), "it covers the right");
}

/// The rows of title bar, border included, in a picture of a window at `scale`, read down a
/// column in its right half, inside the window and clear of the buttons and
/// the title.
fn title_bar_rows(scale: &str) -> u32 {
    let colour = sengi_tokens::Tokens::builtin()
        .color("color.surface")
        .expect("the tokens define the title bar colour");
    let bar = u32::from(colour.r) << 16 | u32::from(colour.g) << 8 | u32::from(colour.b);
    let edge = sengi_tokens::Tokens::builtin()
        .color("color.border")
        .expect("the tokens define the border colour");
    let edge = u32::from(edge.r) << 16 | u32::from(edge.g) << 8 | u32::from(edge.b);
    let compositor = Compositor::start_args(&["--scale", scale], None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.decorated_window(GREEN).unwrap();
    // Focused, so that the bar is the active colour.
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    let picture = app.capture().unwrap();
    let (mut longest, mut run) = (0, 0);
    for y in 0..picture.height {
        // The bar is drawn with its border around it, on the top edge.
        let pixel = rgb(picture.at(740, y));
        if pixel == bar || pixel == edge {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

/// The title bar is as tall as the tokens say, at a scale of one.
#[test]
fn the_frame_is_the_expected_size_at_scale_one() {
    let height = sengi_tokens::Tokens::builtin()
        .px("size.titlebar-height")
        .expect("the tokens define the title bar height");
    assert_eq!(title_bar_rows("1"), height);
}

/// Twice the rows at a scale of two. The run stops at a blended row, so a
/// bar with a soft edge at either end comes out short.
#[test]
fn the_frame_is_the_expected_size_at_scale_two() {
    let height = sengi_tokens::Tokens::builtin()
        .px("size.titlebar-height")
        .expect("the tokens define the title bar height");
    assert_eq!(title_bar_rows("2"), 2 * height);
}

/// Snapping and moving to an edge leave a fullscreen window where it is, and
/// taking it out of fullscreen gives back the size it had.
#[test]
fn edge_keys_leave_a_fullscreen_window_alone() {
    let script = "3000 key super+left\n3300 key alt+left\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (_surface, toplevel) = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    toplevel.set_fullscreen(None);
    assert!(
        app.wait(SOON, |seen| seen.more.toplevel == Some((1280, 720, true))),
        "not told to fill the screen: {:?}",
        app.seen.more.toplevel
    );
    // Both keys come after the window has gone fullscreen.
    app.wait(Duration::from_secs(4), |_| false);
    assert_eq!(
        app.seen.more.toplevel,
        Some((1280, 720, true)),
        "an edge key changed a fullscreen window"
    );
    toplevel.unset_fullscreen();
    assert!(
        app.wait(SOON, |seen| seen.more.toplevel == Some((300, 200, false))),
        "not given back its size: {:?}",
        app.seen.more.toplevel
    );
}

/// A window wider than the screen, at a scale of two, has its title bar
/// across the whole screen: the bar is cut to the screen and the cut part is
/// not left unpainted.
#[test]
fn a_window_wider_than_the_screen_has_its_bar_across_it_at_scale_two() {
    let compositor = Compositor::start_args(&["--scale", "2"], None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (surface, _toplevel) = app.decorated_window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    app.paint(&surface, 900, 200, GREEN).unwrap();
    let picture = app.capture().unwrap();
    // The bar is physical rows 128 to 187; its fill is not the desktop.
    let desktop = rgb(picture.at(0, 0));
    for x in [400, 640, 1000, 1277] {
        let pixel = rgb(picture.at(x, 140));
        assert_ne!(pixel, desktop, "no bar at x {x}");
        assert_ne!(pixel, rgb(GREEN), "the client is over the bar at x {x}");
    }
}

/// A window wider than the screen, at a scale of two, has its bottom border
/// across the whole screen, not only up to where its bar is cut.
#[test]
fn a_wide_window_has_its_bottom_border_across_the_screen_at_scale_two() {
    let border = sengi_tokens::Tokens::builtin()
        .color("color.border")
        .expect("the tokens define the border colour");
    let border = u32::from(border.r) << 16 | u32::from(border.g) << 8 | u32::from(border.b);
    let compositor = Compositor::start_args(&["--scale", "2"], None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (surface, _toplevel) = app.decorated_window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    app.paint(&surface, 900, 200, GREEN).unwrap();
    let picture = app.capture().unwrap();
    for x in [400, 640, 1000, 1277] {
        let last_client = (0..picture.height)
            .rev()
            .find(|&y| rgb(picture.at(x, y)) == rgb(GREEN))
            .expect("the client is on screen");
        let below: Vec<u32> = (last_client + 1..last_client + 6)
            .map(|y| rgb(picture.at(x, y)))
            .collect();
        assert!(
            below.contains(&border),
            "no bottom border at x {x}: {below:x?}"
        );
    }
}

/// At a scale of one half, a client that grows by one logical pixel where
/// the physical width does not change still has its bar as wide as the
/// frame: the bar's size is refreshed with the frame, not kept from before.
#[test]
#[ignore = "known: at a scale below one the bar is partly unpainted"]
fn a_bar_grows_with_its_window_at_half_scale() {
    let compositor = Compositor::start_args(&["--scale", "0.5"], None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (surface, _toplevel) = app.decorated_window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    app.paint(&surface, 301, 200, GREEN).unwrap();
    app.paint(&surface, 302, 200, GREEN).unwrap();
    let picture = app.capture().unwrap();
    let mid = picture.width / 2;
    let client_top = (0..picture.height)
        .find(|&y| rgb(picture.at(mid, y)) == rgb(GREEN))
        .expect("the client is on screen");
    let client_right = (0..picture.width)
        .rev()
        .find(|&x| rgb(picture.at(x, client_top + 3)) == rgb(GREEN))
        .expect("the client is on screen");
    let bar_row = client_top - 4;
    let bar_colour = sengi_tokens::Tokens::builtin()
        .color("color.surface")
        .expect("the tokens define the title bar colour");
    let bar_colour =
        u32::from(bar_colour.r) << 16 | u32::from(bar_colour.g) << 8 | u32::from(bar_colour.b);
    let bar_right = (0..picture.width)
        .rev()
        .find(|&x| rgb(picture.at(x, bar_row)) == bar_colour)
        .expect("the bar is on screen");
    assert!(
        bar_right > client_right,
        "the bar ends at {bar_right}, the client at {client_right}"
    );
}

/// The same key that leaves a fullscreen window alone does snap one that is
/// not fullscreen, so the test above cannot pass just because the key was
/// dropped.
#[test]
fn an_edge_key_snaps_a_window_that_is_not_fullscreen() {
    let script = "3000 key super+left\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (_surface, _toplevel) = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    assert!(
        app.wait(Duration::from_secs(6), |seen| seen
            .more
            .toplevel
            .is_some_and(|(w, _, _)| w > 300 && w < 700)),
        "the key did not snap the window: {:?}",
        app.seen.more.toplevel
    );
}

fn token(name: &str) -> u32 {
    let c = sengi_tokens::Tokens::builtin().color(name).unwrap();
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// The top left and bottom right of the pixels of one colour.
fn bounds(picture: &common::apps::Picture, colour: u32) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..picture.height {
        for x in 0..picture.width {
            if picture.at(x, y) == colour {
                let b = found.get_or_insert((x, y, x, y));
                *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
    }
    found
}

/// Runs of pixels on row `y`, from `from` for `len` pixels, that are neither
/// the bar nor its border: the three buttons.
fn button_runs(
    picture: &common::apps::Picture,
    y: u32,
    from: u32,
    len: u32,
    bar: u32,
) -> Vec<(u32, u32)> {
    let border = token("color.border");
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for x in from..from + len {
        let pixel = picture.at(x, y);
        if pixel == bar || pixel == border {
            continue;
        }
        match runs.last_mut() {
            Some(run) if run.1 + 1 == x => run.1 = x,
            _ => runs.push((x, x)),
        }
    }
    runs
}

/// What the buttons measure in a picture: the left inset from the frame's
/// edge, then the diameter of each circle and the gaps between them.
fn measure_buttons(
    picture: &common::apps::Picture,
    anchor: u32,
    bar: u32,
    left_of_anchor: u32,
) -> Vec<u32> {
    let (x0, y0, _, y1) = bounds(picture, anchor).expect("a button of the anchor colour");
    let frame_left = x0 - left_of_anchor;
    let y = y0 + (y1 - y0).div_ceil(2);
    assert_eq!(
        picture.at(frame_left, y),
        token("color.border"),
        "frame edge"
    );
    let runs = button_runs(picture, y, frame_left + 1, 68, bar);
    assert_eq!(runs.len(), 3, "three buttons: {runs:?}");
    let mut numbers = vec![runs[0].0 - frame_left];
    for (i, run) in runs.iter().enumerate() {
        numbers.push(run.1 - run.0 + 1);
        if let Some(next) = runs.get(i + 1) {
            numbers.push(next.0 - run.1 - 1);
        }
    }
    numbers
}

/// Inset, circle, gap, circle, gap, circle, as the spec gives them: 12 from
/// the frame's edge, 12 across, 8 apart.
const BUTTON_NUMBERS: [u32; 6] = [12, 12, 8, 12, 8, 12];

#[test]
fn the_buttons_of_a_focused_window_measure_as_specified() {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.decorated_window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    let picture = app.capture().unwrap();
    // Pixels fully inside the close circle are the fill colour.
    let numbers = measure_buttons(
        &picture,
        token("color.window-close"),
        token("color.surface"),
        13,
    );
    eprintln!("focused buttons: inset, d, gap, d, gap, d = {numbers:?}");
    assert_eq!(numbers, BUTTON_NUMBERS);
    if let Some(dir) = std::env::var_os("SENGI_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        picture.write_ppm(&dir.join("window-focused.ppm")).unwrap();
    }
}

#[test]
fn the_buttons_of_a_window_that_is_not_focused_are_grey() {
    let compositor = Compositor::start(None);
    let mut first = compositor.connect();
    first.keyboard().unwrap();
    let _a = first.decorated_window(GREEN).unwrap();
    assert!(first.wait(SOON, |seen| seen.keyboard_enters == 1));
    let mut second = compositor.connect();
    second.keyboard().unwrap();
    let _b = second.decorated_window(RED).unwrap();
    assert!(second.wait(SOON, |seen| seen.keyboard_enters == 1));
    let picture = second.capture().unwrap();
    let grey = token("color.window-inactive");
    let numbers = measure_buttons(&picture, grey, token("color.bg"), 13);
    eprintln!("unfocused buttons: {numbers:?}");
    assert_eq!(numbers, BUTTON_NUMBERS);
    if let Some(dir) = std::env::var_os("SENGI_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        picture
            .write_ppm(&dir.join("window-unfocused.ppm"))
            .unwrap();
    }
}

/// With the pointer on a button every button shows its glyph.
#[test]
fn the_pointer_over_the_buttons_shows_the_glyphs() {
    // Where the first window's close button is, found from a picture
    // without the pointer, then the same run with the pointer there.
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.decorated_window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    let plain = app.capture().unwrap();
    let (x0, y0, x1, y1) = bounds(&plain, token("color.window-close")).unwrap();
    drop(app);
    drop(compositor);

    let script = format!("1500 move {} {}\n", (x0 + x1) / 2, (y0 + y1) / 2);
    let compositor = Compositor::start_with(None, None, Some(&script));
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.decorated_window(GREEN).unwrap();
    app.wait(Duration::from_secs(4), |_| false);
    let hovered = app.capture().unwrap();
    // The glyph is the centre of each circle: the close glyph is dark red.
    let centre = ((x0 + x1) / 2, (y0 + y1) / 2);
    assert_ne!(plain.at(centre.0, centre.1), hovered.at(centre.0, centre.1));
    // The maximise button, two steps to the right, has its glyph too.
    let at_max = (centre.0 + 40, centre.1);
    assert_ne!(plain.at(at_max.0, at_max.1), hovered.at(at_max.0, at_max.1));
    if let Some(dir) = std::env::var_os("SENGI_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        hovered.write_ppm(&dir.join("window-hovered.ppm")).unwrap();
    }
}

/// Pressing a button the pointer already rests on changes nothing but the
/// press, and the button is still drawn held down.
#[test]
fn a_button_is_drawn_darker_while_it_is_held() {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.decorated_window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    let plain = app.capture().unwrap();
    let (x0, y0, x1, y1) = bounds(&plain, token("color.window-close")).unwrap();
    drop(app);
    drop(compositor);

    let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
    // On the button at 1.5 s, held from 2 s on, never let go.
    let script = format!("1500 move {cx} {cy}\n2000 press left\n");
    let compositor = Compositor::start_with(None, None, Some(&script));
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.decorated_window(GREEN).unwrap();
    app.wait(Duration::from_secs(4), |_| false);
    let held = app.capture().unwrap();
    // Left of the glyph, still inside the circle.
    let (x, y) = (x0, cy);
    let fill = sengi_tokens::Tokens::builtin()
        .color("color.window-close")
        .unwrap();
    let darker = |v: u8| u32::from(v) * 85 / 100;
    let want = darker(fill.r) << 16 | darker(fill.g) << 8 | darker(fill.b);
    let got = held.at(x, y);
    for shift in [16, 8, 0] {
        let (a, b) = ((got >> shift) & 0xff, (want >> shift) & 0xff);
        assert!(
            a.abs_diff(b) <= 3,
            "held button {got:06x}, wanted {want:06x}"
        );
    }
    assert_ne!(held.at(x, y), plain.at(x, y));
    // The next button, left of its glyph, is as it was.
    let (mx, my) = (x0 + 20, cy);
    assert_eq!(held.at(mx, my), plain.at(mx, my));
    if let Some(dir) = std::env::var_os("SENGI_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        held.write_ppm(&dir.join("window-pressed.ppm")).unwrap();
    }
}
