//! Every icon, in every state the interface draws it in, is rendered with
//! the software renderer and has to paint a real shape: the name having a
//! file is not enough, a path can be a lone dot or empty.

use std::path::Path;
use std::rc::Rc;

use sengi_ui::IconProbe;
use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, PlatformError};

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

const SIDE: usize = 64;

/// A picture of the probe as it is now: premultiplied pixels, SIDE square.
fn draw(window: &MinimalSoftwareWindow) -> Vec<PremultipliedRgbaColor> {
    let mut buffer = vec![PremultipliedRgbaColor::default(); SIDE * SIDE];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, SIDE);
    });
    buffer
}

/// How an icon came out: the pixels that are not the background, and the
/// size of the box around them.
#[derive(Debug, Clone, PartialEq)]
struct Shape {
    painted: usize,
    width: usize,
    height: usize,
    picture: Vec<[u8; 3]>,
}

fn shape(picture: &[PremultipliedRgbaColor], background: [u8; 3]) -> Shape {
    let rgb: Vec<[u8; 3]> = picture.iter().map(|p| [p.red, p.green, p.blue]).collect();
    let (mut x0, mut y0, mut x1, mut y1) = (SIDE, SIDE, 0, 0);
    let mut painted = 0;
    for (i, pixel) in rgb.iter().enumerate() {
        if pixel
            .iter()
            .zip(background)
            .any(|(a, b)| a.abs_diff(b) > 24)
        {
            let (x, y) = (i % SIDE, i / SIDE);
            painted += 1;
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    let (width, height) = if painted == 0 {
        (0, 0)
    } else {
        (x1 - x0 + 1, y1 - y0 + 1)
    };
    Shape {
        painted,
        width,
        height,
        picture: rgb,
    }
}

fn icon_names() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/icons/phosphor/bold");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix("-bold.svg").map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

/// Icons that are meant to be small: Phosphor's lone dot, which the Wi-Fi
/// icon only ever draws over its three faint bars.
const PARTS: &[&str] = &["wifi-none"];

/// Whether a shape is a proper icon: something to see, and not a dot.
fn is_an_icon(shape: &Shape) -> bool {
    shape.painted >= 100 && shape.width.max(shape.height) >= SIDE * 2 / 5
}

fn add(label: String, shape: Shape, sheet: &mut Vec<(String, Shape)>) {
    if !label.starts_with("part ") {
        let info = (shape.painted, shape.width, shape.height);
        assert!(is_an_icon(&shape), "{label} paints {info:?}");
    }
    sheet.push((label, shape));
}

#[test]
fn every_icon_and_state_paints_a_real_shape() {
    let window = MinimalSoftwareWindow::new(Default::default());
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    window.set_size(PhysicalSize::new(SIDE as u32, SIDE as u32));
    let probe = IconProbe::new().unwrap();
    probe.show().unwrap();

    // The background is what the probe paints with nothing in it.
    probe.set_kind("name".into());
    probe.set_name("no-such-icon".into());
    let first = draw(&window);
    let background = [first[0].red, first[0].green, first[0].blue];
    assert_eq!(
        shape(&first, background).painted,
        0,
        "an unknown name draws nothing"
    );

    let mut sheet: Vec<(String, Shape)> = Vec::new();
    let check = add;

    for name in icon_names() {
        probe.set_kind("name".into());
        probe.set_name(name.clone().into());
        let s = shape(&draw(&window), background);
        let label = if PARTS.contains(&name.as_str()) {
            format!("part {name}")
        } else {
            format!("name {name}")
        };
        check(label, s, &mut sheet);
    }
    // The lone dot really is one, and so is why it has an exception.
    probe.set_name("wifi-none".into());
    assert!(!is_an_icon(&shape(&draw(&window), background)));

    probe.set_kind("signal".into());
    for level in 0..=4 {
        probe.set_level(level);
        check(
            format!("signal {level}"),
            shape(&draw(&window), background),
            &mut sheet,
        );
    }
    probe.set_kind("speaker".into());
    for muted in [false, true] {
        probe.set_flag(muted);
        check(
            format!("speaker muted={muted}"),
            shape(&draw(&window), background),
            &mut sheet,
        );
    }
    probe.set_kind("battery".into());
    for (percent, charging) in [
        (100, false),
        (80, false),
        (50, false),
        (25, false),
        (5, false),
        (50, true),
    ] {
        probe.set_level(percent);
        probe.set_flag(charging);
        check(
            format!("battery {percent} charging={charging}"),
            shape(&draw(&window), background),
            &mut sheet,
        );
    }

    // Each state of one icon looks different from the others.
    let states: Vec<&(String, Shape)> = sheet
        .iter()
        .filter(|(l, _)| !l.starts_with("name ") && !l.starts_with("part "))
        .collect();
    for (i, (a, sa)) in states.iter().enumerate() {
        for (b, sb) in &states[i + 1..] {
            if a.split(' ').next() == b.split(' ').next() {
                assert_ne!(sa.picture, sb.picture, "{a} and {b} look the same");
            }
        }
    }

    if let Some(dir) = std::env::var_os("SENGI_SHOTS") {
        write_sheet(Path::new(&dir), &sheet);
    }
}

/// The contact sheet as a PPM, the labels in a text file beside it.
fn write_sheet(dir: &Path, sheet: &[(String, Shape)]) {
    std::fs::create_dir_all(dir).unwrap();
    let columns = 12;
    let rows = sheet.len().div_ceil(columns);
    let (w, h) = (columns * SIDE, rows * SIDE);
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for y in 0..h {
        for x in 0..w {
            let index = (y / SIDE) * columns + x / SIDE;
            let pixel = sheet
                .get(index)
                .map_or([0, 0, 0], |(_, s)| s.picture[(y % SIDE) * SIDE + x % SIDE]);
            out.extend_from_slice(&pixel);
        }
    }
    std::fs::write(dir.join("icons-sheet.ppm"), out).unwrap();
    let labels: Vec<String> = sheet
        .iter()
        .enumerate()
        .map(|(i, (l, _))| format!("{} (row {}, column {}): {l}", i, i / columns, i % columns))
        .collect();
    std::fs::write(dir.join("icons-sheet.txt"), labels.join("\n")).unwrap();
}
