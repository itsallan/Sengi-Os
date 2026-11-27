//! The logo of the interface is the one in docs/media/logo.svg, and it comes
//! out whole, with its eye, at every size it is drawn at.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use sengi_ui::LogoProbe;
use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, PlatformError};

fn read(path: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path),
    )
    .unwrap()
}

/// The `name="value"` of one tag.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let (_, rest) = tag.split_once(&format!(" {name}=\""))?;
    rest.split('"').next()
}

fn number(tag: &str, name: &str) -> f32 {
    attribute(tag, name)
        .unwrap_or_else(|| panic!("{tag} has no {name}"))
        .parse()
        .unwrap()
}

/// The values of the properties `ui/logo.slint` keeps the drawing in.
fn shape_properties() -> BTreeMap<String, String> {
    read("ui/logo.slint")
        .lines()
        .filter_map(|line| line.trim().strip_prefix("out property <"))
        .filter_map(|line| {
            let (_, rest) = line.split_once("> ")?;
            let (name, value) = rest.split_once(": ")?;
            let value = value.strip_suffix(';')?.trim_matches('"');
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

/// A circle as the two arcs a path draws it with.
fn circle(tag: &str) -> String {
    let (x, y, r) = (number(tag, "cx"), number(tag, "cy"), number(tag, "r"));
    format!(
        "M{left} {y} A{r} {r} 0 1 1 {right} {y} A{r} {r} 0 1 1 {left} {y} Z",
        left = x - r,
        right = x + r,
    )
}

#[test]
fn the_geometry_is_that_of_the_svg() {
    let svg = read("docs/media/logo.svg");
    let shape = shape_properties();
    let property = |name: &str| {
        shape
            .get(name)
            .unwrap_or_else(|| panic!("ui/logo.slint has no {name}"))
            .as_str()
    };
    let measure = |name: &str| property(name).parse::<f32>().unwrap();
    let tags: Vec<&str> = svg.split('<').collect();
    let all = |start: &str| -> Vec<&str> {
        tags.iter()
            .copied()
            .filter(|tag| tag.starts_with(start))
            .collect()
    };

    let paths = all("path ");
    assert_eq!(paths.len(), 4, "the speed lines, tail, legs and body");
    for (path, name) in paths.iter().zip(["speed", "tail", "legs", "body"]) {
        assert_eq!(attribute(path, "d"), Some(property(name)), "{name}");
    }
    assert_eq!(number(paths[0], "stroke-width"), measure("thin"));
    assert_eq!(number(paths[1], "stroke-width"), measure("thin"));
    assert_eq!(number(paths[2], "stroke-width"), measure("thick"));
    assert_eq!(number(paths[0], "opacity"), measure("speed-opacity"));

    let circles = all("circle ");
    assert_eq!(circles.len(), 2, "the ear and the eye");
    assert_eq!(property("ear"), circle(circles[0]));
    assert_eq!(property("eye"), circle(circles[1]));

    let root = all("svg ")[0];
    let view: Vec<f32> = attribute(root, "viewBox")
        .unwrap()
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    let group = all("g ")[0];
    let up: f32 = attribute(group, "transform")
        .and_then(|t| t.strip_prefix("translate(0 ")?.strip_suffix(')'))
        .expect("the drawing is moved straight up or down")
        .parse()
        .unwrap();
    assert_eq!(
        ["left", "top", "width", "height"].map(measure),
        [view[0], view[1] - up, view[2], view[3]]
    );
}

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

/// The probe's window, `logo.page` twice over.
const SIDE: usize = 192;
/// Where the probe puts the logo, `space.lg` from the corner.
const AT: usize = 16;

#[test]
fn it_is_drawn_whole_at_every_size() {
    let window = MinimalSoftwareWindow::new(Default::default());
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    window.set_size(PhysicalSize::new(SIDE as u32, SIDE as u32));
    let probe = LogoProbe::new().unwrap();
    probe.show().unwrap();
    let shape = shape_properties();
    let measure = |name: &str| shape[name].parse::<f32>().unwrap();

    for width in [16, 20, 32, 64, 128] {
        probe.set_logo_width(width as f32);
        slint::platform::update_timers_and_animations();
        let mut buffer = vec![PremultipliedRgbaColor::default(); SIDE * SIDE];
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut buffer, SIDE);
        });
        let picture: Vec<[u8; 3]> = buffer.iter().map(|p| [p.red, p.green, p.blue]).collect();
        if let Some(dir) = std::env::var_os("SENGI_SHOTS") {
            let mut out = format!("P6\n{SIDE} {SIDE}\n255\n").into_bytes();
            out.extend(picture.iter().flatten());
            fs::create_dir_all(&dir).unwrap();
            fs::write(Path::new(&dir).join(format!("logo-{width}.ppm")), out).unwrap();
        }

        let small = width < 32;
        assert_eq!(probe.get_small(), small, "at {width} px");
        let height = probe.get_logo_height();
        let background = picture[0];
        let painted: Vec<(usize, usize)> = (0..picture.len())
            .filter(|&i| picture[i] != background)
            .map(|i| (i % SIDE, i / SIDE))
            .collect();
        let x0 = painted.iter().map(|p| p.0).min().unwrap();
        let x1 = painted.iter().map(|p| p.0).max().unwrap() + 1;
        let y0 = painted.iter().map(|p| p.1).min().unwrap();
        let y1 = painted.iter().map(|p| p.1).max().unwrap() + 1;
        let right = AT + width;
        let bottom = AT + height.ceil() as usize;
        assert!(
            x0 >= AT && x1 <= right && y0 >= AT && y1 <= bottom,
            "at {width} px it spills out of its box: {x0}..{x1} by {y0}..{y1}"
        );
        // Whole, and near the middle of its box.
        let slack = width / 16 + 1;
        let (left, spare) = (x0 - AT, right - x1);
        assert!(
            left.abs_diff(spare) <= slack,
            "at {width} px: {left} px to the left of it, {spare} to the right"
        );
        let (above, below) = (y0 - AT, bottom - y1);
        assert!(
            above.abs_diff(below) <= slack,
            "at {width} px: {above} px above it, {below} below"
        );
        assert!((x1 - x0) * 10 >= width * 8, "at {width} px it is cut off");

        // The eye, a hole of the background's colour in the body.
        let prefix = if small { "mark-" } else { "" };
        let [view_left, view_top, view_width] =
            ["left", "top", "width"].map(|n| measure(&format!("{prefix}{n}")));
        let scale = width as f32 / view_width;
        let (at_x, at_y) = if small { (69.0, 50.5) } else { (70.0, 49.0) };
        let eye_x = AT + ((at_x - view_left) * scale) as usize;
        let eye_y = AT + ((at_y - view_top) * scale) as usize;
        let body = picture[(eye_y * SIDE) + eye_x - (12.0 * scale).round() as usize];
        let eye = picture[eye_y * SIDE + eye_x];
        assert!(
            body[1] > 120,
            "at {width} px the body is the accent: {body:?}"
        );
        assert!(
            eye[1] + 40 < body[1],
            "at {width} px the eye does not show: {eye:?} in {body:?}"
        );
    }
}
