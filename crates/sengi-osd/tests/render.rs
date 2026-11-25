//! The panel drawn with the software renderer, at a few levels. With
//! SENGI_RENDER_DIR set the pictures are written there as PNG files, to
//! look at.

use std::rc::Rc;

use sengi_osd::level::{Kind, Level};
use sengi_osd::ui::{self, OsdWindow};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, PlatformError};

const WIDTH: usize = 240;
const HEIGHT: usize = 56;
/// A desktop-ish colour behind the panel, so its see-through corners show.
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

fn save(name: &str, rows: usize, picture: &[[u8; 3]]) {
    let Some(dir) = std::env::var_os("SENGI_RENDER_DIR") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(name);
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(file, WIDTH as u32, rows as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let data: Vec<u8> = picture.iter().flatten().copied().collect();
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&data)
        .unwrap();
}

/// How many pixels of the accent colour a picture has: the filled bar.
fn accent(picture: &[[u8; 3]]) -> usize {
    picture.iter().filter(|p| **p == [0xd9, 0x77, 0x3a]).count()
}

#[test]
fn the_panel_shows_an_icon_and_a_bar_that_follows_the_level() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    window.set_size(PhysicalSize::new(WIDTH as u32, HEIGHT as u32));
    let panel = OsdWindow::new().unwrap();
    panel.show().unwrap();

    let levels = [
        Level::new(Kind::Volume, 65, false),
        Level::new(Kind::Volume, 40, true),
        Level::new(Kind::Brightness, 30, false),
        Level::new(Kind::Keyboard, 100, false),
        Level::new(Kind::Brightness, 5, false),
        Level::new(Kind::Volume, 0, false),
    ];
    let mut sheet = Vec::new();
    let mut fills = Vec::new();
    for level in levels {
        ui::apply(&panel, &level);
        let picture = draw(&window);
        assert!(
            picture.iter().any(|pixel| *pixel != BACKDROP),
            "{level:?} draws nothing"
        );
        fills.push(accent(&picture));
        sheet.extend(picture);
    }
    save("osd.png", HEIGHT * levels.len(), &sheet);

    // 65 percent fills more than 30, and 100 more than both; a muted sound
    // and a level of zero fill nothing.
    assert!(fills[0] > fills[2], "{fills:?}");
    assert!(fills[3] > fills[0], "{fills:?}");
    assert!(fills[4] > 0 && fills[4] < fills[2], "{fills:?}");
    assert_eq!(fills[1], 0, "a muted sound shows an empty bar");
    assert_eq!(fills[5], 0, "{fills:?}");
}
