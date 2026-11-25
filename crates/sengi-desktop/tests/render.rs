//! The desktop drawn with the software renderer: icons for the folders and
//! files, the menu on a right click, a new folder in rename. With
//! SENGI_RENDER_DIR set the pictures are written there as PNG files, to look
//! at.

use std::fs;
use std::rc::Rc;

use sengi_apps::Env;
use sengi_desktop::app::Shell;
use sengi_desktop::desk::Desk;
use sengi_desktop::ui::Desktop;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
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
fn icons_menus_and_a_new_folder_in_rename() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    window.set_size(PhysicalSize::new(WIDTH as u32, HEIGHT as u32));

    let root = std::env::temp_dir().join(format!("sengi-desktop-render-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let dir = root.join("Desktop");
    fs::create_dir_all(&dir).unwrap();
    for name in ["Projects", "Photos", "A folder with a rather long name"] {
        fs::create_dir(dir.join(name)).unwrap();
    }
    for name in ["notes.txt", "todo.md", "report-final-version-2.pdf"] {
        fs::write(dir.join(name), "").unwrap();
    }
    let env = Env::from_vars(|name| (name == "PATH").then(|| "/nonexistent".into()));
    let shell = Shell::new(Desk::new(dir.clone(), env));

    let desktop = Desktop::new().unwrap();
    shell.attach(&desktop);
    desktop.show().unwrap();

    // The icons stand in a column from the top left, folders first.
    let plain = draw(&window);
    save("desktop.png", &plain);
    assert!(painted(&plain, 0, 0, 120, 120) > 400, "no first icon");
    assert!(painted(&plain, 0, 120, 120, 120) > 400, "no second icon");
    assert_eq!(painted(&plain, 600, 300, 200, 200), 0, "the desktop is not clear");

    // A right click on bare desktop opens the menu where it was clicked.
    click(&window, 600.0, 300.0, PointerEventButton::Right);
    let menu = draw(&window);
    save("menu.png", &menu);
    assert!(painted(&menu, 600, 300, 160, 100) > 2000, "no menu");

    // A left click on bare desktop closes it again.
    click(&window, 900.0, 600.0, PointerEventButton::Left);
    let closed = draw(&window);
    assert_eq!(painted(&closed, 600, 300, 160, 100), 0, "the menu stayed");

    // A click on an icon selects it, and a right click on it opens a menu
    // that is not the desktop's.
    click(&window, 48.0, 40.0, PointerEventButton::Left);
    let selected = draw(&window);
    save("selected.png", &selected);
    assert_ne!(selected, plain, "the click did not show");
    assert_eq!(desktop.get_selected(), 0);
    click(&window, 48.0, 40.0, PointerEventButton::Right);
    let item_menu = draw(&window);
    save("item-menu.png", &item_menu);
    assert_ne!(item_menu, selected);
    assert_ne!(item_menu, menu);

    // "New Folder" is the first row of the desktop's menu: a folder is made,
    // selected and ready for a new name.
    click(&window, 900.0, 600.0, PointerEventButton::Left);
    click(&window, 600.0, 300.0, PointerEventButton::Right);
    click(&window, 640.0, 315.0, PointerEventButton::Left);
    assert!(dir.join("untitled folder").is_dir(), "no new folder");
    let rename = draw(&window);
    save("new-folder.png", &rename);
    assert_ne!(rename, closed);

    // Left clicking away ends the rename and keeps the name.
    click(&window, 900.0, 600.0, PointerEventButton::Left);
    slint::platform::update_timers_and_animations();
    let _ = draw(&window);
    assert!(dir.join("untitled folder").is_dir());
    assert_eq!(desktop.get_selected(), -1);

    fs::remove_dir_all(root).unwrap();
}
