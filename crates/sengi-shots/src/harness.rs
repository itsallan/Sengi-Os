//! A platform with no display: one software window per component, a clock
//! that only moves when told to, and a picture of what is drawn.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, PlatformError};

thread_local! {
    static LAST: RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(None) };
    static NOW: Cell<u64> = const { Cell::new(0) };
}

struct Headless;

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        LAST.with(|last| *last.borrow_mut() = Some(window.clone()));
        Ok(window)
    }

    fn duration_since_start(&self) -> Duration {
        Duration::from_millis(NOW.with(Cell::get))
    }
}

/// Whether `SENGI_THEME=light` asks for the light palette.
pub fn light() -> bool {
    std::env::var("SENGI_THEME").is_ok_and(|theme| theme == "light")
}

/// How a window is drawn: the palette, the type scale and the motion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub light: bool,
    pub high_contrast: bool,
    pub text_scale: f32,
    pub reduce_motion: bool,
}

impl Look {
    /// The dark theme at the design size, with every animation.
    pub const PLAIN: Look = Look {
        light: false,
        high_contrast: false,
        text_scale: 1.0,
        reduce_motion: false,
    };

    pub const HIGH_CONTRAST: Look = Look {
        high_contrast: true,
        ..Look::PLAIN
    };

    /// The type at a scale: 1.4 is Extra large.
    pub const fn scaled(self, text_scale: f32) -> Look {
        Look { text_scale, ..self }
    }

    /// What `SENGI_THEME` (`light`, `high-contrast`), `SENGI_TEXT_SIZE`
    /// (`small`, `large`, `extra-large`) and `SENGI_REDUCE_MOTION=1` ask for.
    pub fn from_env() -> Look {
        let theme = std::env::var("SENGI_THEME").unwrap_or_default();
        let text_scale = match std::env::var("SENGI_TEXT_SIZE").as_deref() {
            Ok("small") => 0.9,
            Ok("large") => 1.2,
            Ok("extra-large") => 1.4,
            _ => 1.0,
        };
        Look {
            light: theme == "light",
            high_contrast: theme == "high-contrast",
            text_scale,
            reduce_motion: std::env::var("SENGI_REDUCE_MOTION").is_ok_and(|v| v == "1"),
        }
    }
}

/// The colour of a page of Settings, which the layout checks look for
/// around the cards: `color.bg` of the theme in use.
pub fn page_colour() -> [u8; 3] {
    page_colour_of(Look::from_env())
}

/// The same for a look named outright.
pub fn page_colour_of(look: Look) -> [u8; 3] {
    if look.high_contrast {
        [0x00, 0x00, 0x00]
    } else if look.light {
        [0xf3, 0xf4, 0xf6]
    } else {
        [0x16, 0x17, 0x1b]
    }
}

/// Call once per thread, before the first component is made.
pub fn install() {
    // Another test on this thread may have done it already.
    let _ = slint::platform::set_platform(Box::new(Headless));
}

/// The window of the component made last.
pub fn window() -> Rc<MinimalSoftwareWindow> {
    LAST.with(|last| last.borrow().clone().expect("a component was made"))
}

/// Let the animations and timers run for this long.
pub fn advance(millis: u64) {
    for _ in 0..(millis / 16).max(1) {
        NOW.with(|now| now.set(now.get() + 16));
        slint::platform::update_timers_and_animations();
    }
}

pub fn resize(window: &MinimalSoftwareWindow, width: u32, height: u32) {
    window.set_size(PhysicalSize::new(width, height));
}

pub fn pointer(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x, y),
    });
}

pub fn key(window: &MinimalSoftwareWindow, text: &str) {
    window.dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

/// What was drawn: opaque rgb, composited over a backdrop.
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<[u8; 3]>,
}

pub const BACKDROP: [u8; 3] = [0x3a, 0x44, 0x52];

pub fn draw(window: &MinimalSoftwareWindow, backdrop: [u8; 3]) -> Picture {
    advance(0);
    let size = window.size();
    let (width, height) = (size.width as usize, size.height as usize);
    let mut buffer = vec![PremultipliedRgbaColor::default(); width * height];
    // Twice: the first draw may only lay things out.
    for _ in 0..2 {
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut buffer, width);
        });
    }
    let rgb = buffer
        .iter()
        .map(|pixel| {
            let behind =
                |channel: u8| (u16::from(channel) * u16::from(255 - pixel.alpha) / 255) as u8;
            [
                pixel.red.saturating_add(behind(backdrop[0])),
                pixel.green.saturating_add(behind(backdrop[1])),
                pixel.blue.saturating_add(behind(backdrop[2])),
            ]
        })
        .collect();
    Picture { width, height, rgb }
}

impl Picture {
    pub fn at(&self, x: usize, y: usize) -> [u8; 3] {
        self.rgb[y * self.width + x]
    }

    /// Writes a PNG into SENGI_RENDER_DIR, when that is set. In the light
    /// theme the name gets `-light` before the extension, in high contrast
    /// `-hc`, and at another text size `-large` and so on.
    pub fn save(&self, name: &str) {
        let look = Look::from_env();
        let mut suffix = String::new();
        if look.light {
            suffix.push_str("-light");
        }
        if look.high_contrast {
            suffix.push_str("-hc");
        }
        if let Ok(size) = std::env::var("SENGI_TEXT_SIZE") {
            suffix.push('-');
            suffix.push_str(&size);
        }
        match name.strip_suffix(".png") {
            Some(stem) => self.save_as(&format!("{stem}{suffix}.png")),
            None => self.save_as(name),
        }
    }

    /// Writes a PNG under exactly this name into SENGI_RENDER_DIR, when
    /// that is set.
    pub fn save_as(&self, name: &str) {
        let Some(dir) = std::env::var_os("SENGI_RENDER_DIR") else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        let file = std::fs::File::create(std::path::Path::new(&dir).join(name)).unwrap();
        let mut encoder = png::Encoder::new(file, self.width as u32, self.height as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let data: Vec<u8> = self.rgb.iter().flatten().copied().collect();
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&data)
            .unwrap();
    }
}

impl Picture {
    /// Whether every pixel of the box is this colour. The box is clipped to
    /// the picture.
    pub fn is_flat(&self, x0: usize, x1: usize, y0: usize, y1: usize, colour: [u8; 3]) -> bool {
        self.first_other(x0, x1, y0, y1, colour).is_none()
    }

    /// The first pixel in the box that is not this colour.
    pub fn first_other(
        &self,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
        colour: [u8; 3],
    ) -> Option<(usize, usize)> {
        for y in y0..y1.min(self.height) {
            for x in x0..x1.min(self.width) {
                if self.at(x, y) != colour {
                    return Some((x, y));
                }
            }
        }
        None
    }
}

/// The tests measure text, so they need the typeface the desktop uses. A
/// machine without Inter draws something else and skips them.
pub fn has_inter() -> bool {
    fn find(dir: &std::path::Path, depth: u32) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        entries.flatten().any(|entry| {
            let path = entry.path();
            if path.is_dir() {
                depth > 0 && find(&path, depth - 1)
            } else {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("Inter") && !name.starts_with("Interp"))
            }
        })
    }
    ["/usr/share/fonts", "/usr/local/share/fonts"]
        .iter()
        .any(|dir| find(std::path::Path::new(dir), 4))
}
