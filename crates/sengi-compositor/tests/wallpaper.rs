//! The wallpaper, which is another program's: swaybg puts a surface the size
//! of the output on the background layer. The compositor has to draw it to
//! the corners, under everything else, and a replacement that comes up
//! before the old one goes down must show without a gap.

mod common;
mod harness;

use harness::Compositor;

const RED: u32 = 0xffb0_2020;
const GREEN: u32 = 0xff20_b020;
const BLUE: u32 = 0xff20_20b0;
const GREY: u32 = 0xff60_6060;
const BAR: u32 = 0xff20_b0b0;

fn rgb(argb: u32) -> u32 {
    argb & 0x00ff_ffff
}

/// Four different colours in the four quarters, so that a picture that is
/// the wrong way round or short of an edge shows.
fn quarters(width: u32, height: u32) -> impl Fn(u32, u32) -> u32 {
    move |x, y| match (x < width / 2, y < height / 2) {
        (true, true) => RED,
        (false, true) => GREEN,
        (true, false) => BLUE,
        (false, false) => GREY,
    }
}

#[test]
fn a_wallpaper_reaches_every_corner_of_the_output() {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    let (width, height) = {
        let first = app.capture().unwrap();
        (first.width, first.height)
    };
    let _wallpaper = app.wallpaper(quarters(width, height)).unwrap();
    let picture = app.capture().unwrap();

    assert_eq!((picture.width, picture.height), (width, height));
    assert_eq!(picture.at(0, 0), rgb(RED));
    assert_eq!(picture.at(width - 1, 0), rgb(GREEN));
    assert_eq!(picture.at(0, height - 1), rgb(BLUE));
    assert_eq!(picture.at(width - 1, height - 1), rgb(GREY));
    // The seams of the picture are where they were put.
    assert_eq!(picture.at(width / 2 - 1, height / 2 - 1), rgb(RED));
    assert_eq!(picture.at(width / 2, height / 2), rgb(GREY));
}

#[test]
fn the_bar_is_over_the_wallpaper_and_the_rest_of_it_still_shows() {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    let first = app.capture().unwrap();
    let (width, height) = (first.width, first.height);
    let _wallpaper = app.wallpaper(quarters(width, height)).unwrap();
    let _bar = app.bar(24, BAR).unwrap();
    let picture = app.capture().unwrap();

    assert_eq!(picture.at(0, 0), rgb(BAR));
    assert_eq!(picture.at(width - 1, 23), rgb(BAR));
    assert_eq!(picture.at(0, 24), rgb(RED));
    assert_eq!(picture.at(width - 1, 24), rgb(GREEN));
    assert_eq!(picture.at(0, height - 1), rgb(BLUE));
    assert_eq!(picture.at(width - 1, height - 1), rgb(GREY));
}

#[test]
fn a_new_wallpaper_over_the_old_one_shows_and_stays_when_the_old_one_goes() {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    let first = app.capture().unwrap();
    let (width, height) = (first.width, first.height);

    let (old, old_layer) = app.wallpaper(|_, _| RED).unwrap();
    assert_eq!(app.capture().unwrap().at(0, 0), rgb(RED));

    // As the session's script does it: the new one first, then the old
    // one is ended. At no time is the output without a wallpaper.
    let mut other = compositor.connect();
    let _new = other.wallpaper(quarters(width, height)).unwrap();
    let both = app.capture().unwrap();
    assert!(
        [RED, GREEN, BLUE, GREY]
            .iter()
            .any(|&colour| both.at(width - 1, height - 1) == rgb(colour)),
        "a wallpaper is showing while there are two"
    );
    old_layer.destroy();
    old.destroy();
    assert!(app.roundtrip().is_ok());

    let after = app.capture().unwrap();
    assert_eq!(after.at(0, 0), rgb(RED));
    assert_eq!(after.at(width - 1, 0), rgb(GREEN));
    assert_eq!(after.at(0, height - 1), rgb(BLUE));
    assert_eq!(after.at(width - 1, height - 1), rgb(GREY));
}
