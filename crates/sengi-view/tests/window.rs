//! The real viewer window, drawn without a display and driven with the
//! keyboard and the pointer. It checks zoom, panning and stepping through a
//! folder, and writes pictures of the window when SENGI_RENDER_DIR is set.

use std::fs;
use std::path::{Path, PathBuf};

use sengi_shots::harness::{self, BACKDROP};
use sengi_view::app::{self, App};
use sengi_view::ui::{ThemeSwitch, View, ViewWindow};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};

const QUADRANTS: &[u8] = include_bytes!("fixtures/quadrants.jpg");

struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        let path = std::env::temp_dir().join(format!("sengi-view-window-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Dir(fs::canonicalize(path).unwrap())
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A picture with something to look at: a gradient, a frame and four blocks.
fn pattern_png(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut data = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let frame = x < 6 || y < 6 || x >= width - 6 || y >= height - 6;
            let (bx, by) = (x * 4 / width, y * 4 / height);
            let pixel = if frame {
                [236, 238, 242]
            } else if (bx + by) % 2 == 0 && bx > 0 && bx < 3 && by > 0 && by < 3 {
                [47, 158, 107]
            } else {
                [(x * 255 / width) as u8, (y * 255 / height) as u8, 120]
            };
            data.extend_from_slice(&pixel);
        }
    }
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&data).unwrap();
    writer.finish().unwrap();
    out
}

fn open_window(path: Option<&Path>) -> (ViewWindow, std::rc::Rc<App>) {
    let window = ViewWindow::new().unwrap();
    window.global::<ThemeSwitch>().set_light(harness::light());
    window.show().unwrap();
    harness::resize(&harness::window(), 888, 600);
    let app = app::attach(&window, path.map(Path::to_path_buf), true);
    harness::advance(400);
    (window, app)
}

fn press(key: Key) {
    type_text(&String::from(char::from(key)));
}

fn type_text(text: &str) {
    let window = harness::window();
    for c in text.chars() {
        harness::key(&window, &c.to_string());
    }
    harness::advance(32);
}

fn picture(name: &str) {
    harness::advance(400);
    harness::draw(&harness::window(), BACKDROP).save(name);
}

fn pointer(event: WindowEvent) {
    harness::window().dispatch_event(event);
    harness::advance(32);
}

#[test]
fn viewer_window() {
    harness::install();
    let dir = Dir::new();
    let first = dir.file("a.png", &pattern_png(1200, 800));
    dir.file("b.png", &pattern_png(300, 200));
    dir.file("c.jpg", QUADRANTS);
    dir.file("z.png", b"this is not a picture");
    dir.file("notes.txt", b"not listed");

    let (w, app) = open_window(Some(&first));
    let view = w.global::<View>();
    assert_eq!(app.names(), ["a.png", "b.png", "c.jpg", "z.png"]);
    assert!(view.get_has_picture());
    assert!(
        app.info_text().starts_with("1200 x 800 px, "),
        "{}",
        app.info_text()
    );
    assert_eq!(view.get_position(), "1 of 4");
    assert_eq!(view.get_title(), "a.png \u{2014} Image Viewer");

    // Fitted to the window to begin with: smaller than the picture.
    let fit = view.get_scale();
    assert!(fit > 0.4 && fit < 0.8, "fit {fit}");
    picture("view-fit-888x600.png");

    // + and - step the zoom, 0 is the size of the picture, 9 fits again.
    type_text("+");
    assert!(
        (view.get_zoom() - fit * 1.25).abs() < 0.001,
        "{}",
        view.get_zoom()
    );
    type_text("0");
    assert_eq!(view.get_zoom(), 1.0);
    assert_eq!(view.get_scale(), 1.0);
    picture("view-100-888x600.png");
    type_text("-");
    assert!((view.get_zoom() - 0.8).abs() < 0.001);
    type_text("0");

    // Dragging moves the picture under the window.
    let before = (view.get_pan_x(), view.get_pan_y());
    pointer(WindowEvent::PointerPressed {
        position: LogicalPosition::new(500.0, 300.0),
        button: PointerEventButton::Left,
    });
    for step in 1..=10 {
        pointer(WindowEvent::PointerMoved {
            position: LogicalPosition::new(500.0 - 12.0 * step as f32, 300.0 - 8.0 * step as f32),
        });
    }
    pointer(WindowEvent::PointerReleased {
        position: LogicalPosition::new(380.0, 220.0),
        button: PointerEventButton::Left,
    });
    let after = (view.get_pan_x(), view.get_pan_y());
    assert!(
        after.0 < before.0 && after.1 < before.1,
        "{before:?} -> {after:?}"
    );
    picture("view-panned-888x600.png");
    type_text("9");
    assert_eq!(view.get_zoom(), 0.0);

    // Right and Left step through the folder, round the ends.
    press(Key::RightArrow);
    assert_eq!(app.current().unwrap(), dir.0.join("b.png"));
    assert!(app.info_text().starts_with("300 x 200 px"));
    assert_eq!(view.get_scale(), 1.0);
    picture("view-small-888x600.png");
    press(Key::RightArrow);
    assert_eq!(app.current().unwrap(), dir.0.join("c.jpg"));
    assert!(
        app.info_text().starts_with("64 x 48 px"),
        "{}",
        app.info_text()
    );
    assert_eq!(view.get_position(), "3 of 4");
    press(Key::RightArrow);
    assert!(!view.get_has_picture());
    assert!(
        app.message_text().contains("\"z.png\" is not shown"),
        "{}",
        app.message_text()
    );
    picture("view-error-888x600.png");
    press(Key::RightArrow);
    assert_eq!(app.current().unwrap(), first);
    press(Key::LeftArrow);
    assert_eq!(app.current().unwrap(), dir.0.join("z.png"));
    drop(w);
    drop(app);

    // With no picture the window says so.
    let (_w, app) = open_window(None);
    assert!(app.message_text().contains("No picture is open"));
}
