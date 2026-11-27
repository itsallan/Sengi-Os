//! The desktop's menu drawn with the software renderer: its rows say what
//! was chosen, and a press beside it or Escape puts it away. With
//! SENGI_RENDER_DIR set the pictures are written there.

use std::cell::RefCell;
use std::fs;
use std::rc::Rc;

use sengi_desktop::ui::DesktopMenu;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, PlatformError};

const WIDTH: usize = 1280;
const HEIGHT: usize = 720;
/// The wallpaper, as far as the pictures go: see-through parts show it.
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
    let file = fs::File::create(std::path::Path::new(&dir).join(name)).unwrap();
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

/// How many pixels of the picture are not the backdrop, in a rectangle.
fn painted(picture: &[[u8; 3]], x: usize, y: usize, w: usize, h: usize) -> usize {
    (y..y + h)
        .flat_map(|row| (x..x + w).map(move |col| (col, row)))
        .filter(|&(col, row)| picture[row * WIDTH + col] != BACKDROP)
        .count()
}

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32, button: PointerEventButton) {
    let position = LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button });
    window.dispatch_event(WindowEvent::PointerReleased { position, button });
}

#[test]
fn the_menu_answers_and_goes_away() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    window.set_size(PhysicalSize::new(WIDTH as u32, HEIGHT as u32));

    let menu = DesktopMenu::new().unwrap();
    menu.set_menu_x(600.0);
    menu.set_menu_y(300.0);
    let said: Rc<RefCell<Vec<String>>> = Rc::default();
    menu.on_chosen({
        let said = said.clone();
        move |word| said.borrow_mut().push(word.into())
    });
    menu.on_dismissed({
        let said = said.clone();
        move || said.borrow_mut().push("dismissed".into())
    });
    menu.show().unwrap();

    let picture = draw(&window);
    save("menu.png", &picture);
    assert!(painted(&picture, 600, 300, 160, 100) > 2000, "no menu");
    assert_eq!(painted(&picture, 0, 0, 300, 200), 0, "it covers the screen");

    // The first two rows, a press beside the menu, and Escape.
    click(&window, 640.0, 315.0, PointerEventButton::Left);
    click(&window, 640.0, 339.0, PointerEventButton::Left);
    click(&window, 100.0, 600.0, PointerEventButton::Left);
    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Escape.into(),
    });
    assert_eq!(
        *said.borrow(),
        ["new-folder", "new-file", "dismissed", "dismissed"]
    );

    // The menu of an icon has its own rows.
    menu.set_for_item(true);
    save("item-menu.png", &draw(&window));
    said.borrow_mut().clear();
    click(&window, 640.0, 315.0, PointerEventButton::Left);
    assert_eq!(*said.borrow(), ["open"]);

    // On another output nothing is drawn.
    menu.set_shown(false);
    assert_eq!(painted(&draw(&window), 600, 300, 160, 100), 0);
}
