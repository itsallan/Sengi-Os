//! The panel drawn with the software renderer, as a grid of icons and names
//! before anything is typed and as a list after. With SENGI_RENDER_DIR set
//! the two pictures are written there as PNG files, to look at.

use std::rc::Rc;

use sengi_apps::decoder::Pixels;
use sengi_launcher::cli;
use sengi_launcher::matcher::{Candidate, TieBreak};
use sengi_launcher::session::Session;
use sengi_launcher::ui::{self, LauncherWindow};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, PlatformError};

const WIDTH: usize = 1280;
const HEIGHT: usize = 720;
/// Behind the panel, so that its see-through parts show in the pictures.
const BACKDROP: [u8; 3] = [0x30, 0x34, 0x3c];

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn draw(window: &MinimalSoftwareWindow) -> Vec<[u8; 3]> {
    let mut buffer = vec![PremultipliedRgbaColor::default(); WIDTH * HEIGHT];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, WIDTH);
    });
    buffer
        .iter()
        .map(|pixel| {
            let behind =
                |channel: u8| (u16::from(channel) * u16::from(255 - pixel.alpha) / 255) as u8;
            [
                pixel.red.saturating_add(behind(BACKDROP[0])),
                pixel.green.saturating_add(behind(BACKDROP[1])),
                pixel.blue.saturating_add(behind(BACKDROP[2])),
            ]
        })
        .collect()
}

fn save(name: &str, picture: &[[u8; 3]]) {
    let Some(dir) = std::env::var_os("SENGI_RENDER_DIR") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(name);
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(file, WIDTH as u32, HEIGHT as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let data: Vec<u8> = picture.iter().flatten().copied().collect();
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&data)
        .unwrap();
}

/// The first and last rows that hold anything but the backdrop.
fn extent(picture: &[[u8; 3]]) -> Option<(usize, usize)> {
    let painted = |row: usize| {
        picture[row * WIDTH..(row + 1) * WIDTH]
            .iter()
            .any(|pixel| *pixel != BACKDROP)
    };
    let first = (0..HEIGHT).find(|row| painted(*row))?;
    let last = (0..HEIGHT).rev().find(|row| painted(*row))?;
    Some((first, last))
}

/// A square of `size` pixels in one colour, as a decoded icon is.
fn icon(size: u32, rgb: [u8; 3]) -> Pixels {
    let mut data = Vec::new();
    for _ in 0..size * size {
        data.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    Pixels { size, data }
}

#[test]
fn the_panel_is_a_grid_until_something_is_typed_and_then_a_list() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    window.set_size(PhysicalSize::new(WIDTH as u32, HEIGHT as u32));

    let candidates: Vec<Candidate> = (0..40)
        .map(|n| Candidate::new(format!("App {n:02}")).icons([format!("icon-{n}")]))
        .collect();
    let session = Session::new(candidates, TieBreak::Alphabetical, 8);
    let invocation = cli::parse(Vec::<String>::new()).unwrap();

    let panel = LauncherWindow::new().unwrap();
    panel.show().unwrap();
    let sink = ui::attach(&panel, session, &invocation, |_| Ok(()), |_| {});

    // Some icons have arrived, some have not: those get a letter tile.
    for (n, rgb) in [[200, 80, 60], [60, 160, 90], [70, 100, 200], [220, 180, 60]]
        .into_iter()
        .enumerate()
    {
        sink(n, Some(icon(48, rgb)));
    }
    assert!(panel.get_grid(), "no grid before anything is typed");
    assert_eq!(panel.get_columns(), 6);
    let grid = draw(&window);
    save("grid.png", &grid);
    let (grid_top, grid_bottom) = extent(&grid).expect("the grid draws nothing");

    // Typing narrows the results to a list, which is shorter.
    panel.invoke_query_edited("app 1".into());
    assert!(!panel.get_grid(), "still a grid after typing");
    let list = draw(&window);
    save("list.png", &list);
    let (list_top, list_bottom) = extent(&list).expect("the list draws nothing");

    // The search field stays where it was, and what is under it shrinks.
    assert_eq!(
        grid_top, list_top,
        "the panel moved when the grid became a list"
    );
    assert!(
        list_bottom < grid_bottom,
        "the list ({list_bottom}) is not shorter than the grid ({grid_bottom})"
    );
    // And clearing the field brings the grid back.
    panel.invoke_query_edited("".into());
    assert!(panel.get_grid());
}
