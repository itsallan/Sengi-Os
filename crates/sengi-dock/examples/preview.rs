//! Shows the dock in an ordinary window, for looking at it without a layer
//! surface. Run it inside a compositor:
//!
//!     preview STATE [--hover N] [--press N] [--no-labels] [--snapshot FILE]
//!
//! STATE is one of:
//!   empty       only the search button
//!   pinned      pinned apps, nothing running
//!   running     some apps running, none focused
//!   focused     as running, with one app focused
//!   transition  an app appears and goes again; with --snapshot, FILE gets
//!               -in, -shown, -out and -gone frames
//!   live        the real thing: your config, your apps, your windows
//!
//! --hover and --press put the pointer on item N. --snapshot saves what the
//! renderer drew, over the desktop colour, prints the memory in use and exits.

use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sengi_apps::icons::{self, IconDirs};
use sengi_apps::{AppIndex, DesktopEntry, Env};
use sengi_dock::app::{App, Deliver, Message};
use sengi_dock::model::Dock;
use sengi_dock::ui;
use sengi_dock::windows::{Source, Window};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model};

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn entry(id: &str, name: &str, icon: &str) -> DesktopEntry {
    let mut entry = DesktopEntry::named(id, name, "true");
    entry.icon = icon.into();
    entry
}

/// Stand-ins for icons that apps install: a large PNG that has to be shrunk
/// and an SVG.
fn write_icons(root: &Path) {
    let dir = root.join("icons/hicolor/256x256/apps");
    fs::create_dir_all(&dir).unwrap();
    let side = 256u32;
    let mut data = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let (dx, dy) = (x as f32 - 127.5, y as f32 - 127.5);
            let inside = (dx * dx + dy * dy).sqrt() < 120.0;
            let shade = (y * 90 / side) as u8;
            data.extend_from_slice(&if inside {
                [70 + shade, 130, 220 - shade, 255]
            } else {
                [0, 0, 0, 0]
            });
        }
    }
    let file = fs::File::create(dir.join("web.png")).unwrap();
    let mut encoder = png::Encoder::new(file, side, side);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&data).unwrap();

    let dir = root.join("icons/hicolor/scalable/apps");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("files.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 48 48\">\
         <path d=\"M4 10h16l4 5h20v25H4z\" fill=\"#4cae6b\"/>\
         <path d=\"M4 19h40v21H4z\" fill=\"#6fcf8d\"/></svg>",
    )
    .unwrap();
}

fn window(id: u32, app_id: &str, focus_seq: u64, activated: bool) -> Window {
    Window {
        id,
        app_id: app_id.into(),
        focus_seq,
        activated,
        ..Window::default()
    }
}

fn windows_for(state: &str) -> Vec<Window> {
    match state {
        "running" => vec![
            window(1, "foot", 1, false),
            window(2, "notes", 2, false),
            window(3, "foot", 3, false),
        ],
        "focused" => vec![
            window(1, "foot", 1, false),
            window(2, "notes", 2, false),
            window(3, "files", 3, true),
        ],
        _ => Vec::new(),
    }
}

fn save(ui: &ui::Dock, path: &str) {
    let shot = ui.window().take_snapshot().expect("snapshot");
    // The dock's window is see-through. Lay it over the desktop colour so
    // the picture looks like the dock does on screen.
    let desktop = [0x12u32, 0x13, 0x16];
    let mut data = Vec::with_capacity(shot.as_bytes().len());
    for p in shot.as_bytes().as_chunks::<4>().0 {
        let alpha = p[3] as u32;
        for channel in 0..3 {
            let over = p[channel] as u32 * alpha + desktop[channel] * (255 - alpha);
            data.push((over / 255) as u8);
        }
        data.push(255);
    }
    let file = fs::File::create(path).expect("snapshot file");
    let mut encoder = png::Encoder::new(file, shot.width(), shot.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&data).unwrap();
    println!("saved {path} ({}x{})", shot.width(), shot.height());
}

fn report_memory() {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if line.starts_with("VmRSS") || line.starts_with("RssAnon") {
            println!("{line}");
        }
    }
}

fn after(ms: u64, f: impl FnOnce() + 'static) {
    slint::Timer::single_shot(Duration::from_millis(ms), f);
}

fn main() -> std::process::ExitCode {
    // Icons are decoded by running this program again.
    let all: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = icons::run_decoder(&all) {
        return code;
    }
    let mut state = String::from("pinned");
    let mut hover = None;
    let mut press = None;
    let mut snapshot = None;
    let mut labels = true;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--hover" => hover = args.next().and_then(|n| n.parse::<f32>().ok()),
            "--press" => press = args.next().and_then(|n| n.parse::<f32>().ok()),
            "--snapshot" => snapshot = args.next(),
            "--no-labels" => labels = false,
            other => state = other.to_string(),
        }
    }

    let dock_ui = ui::Dock::new().expect("the dock component");
    dock_ui.set_show_labels(labels);
    let resize = {
        let weak = dock_ui.as_weak();
        move |width: u32, height: u32| {
            if let Some(ui) = weak.upgrade() {
                ui.window()
                    .set_size(slint::LogicalSize::new(width as f32, height as f32));
            }
        }
    };

    let deliver: Deliver = Arc::new(|message: Message| {
        let _ = slint::invoke_from_event_loop(move || {
            APP.with_borrow(|app| {
                if let Some(app) = app {
                    app.receive(message);
                }
            });
        });
    });

    let app = if state == "live" {
        App::start(dock_ui.clone_strong(), Source::Own, deliver, resize)
    } else {
        let root = std::env::temp_dir().join(format!("sengi-dock-preview-{}", std::process::id()));
        write_icons(&root);
        let icon_dirs = IconDirs {
            themes: vec![root.join("icons")],
            pixmaps: Vec::new(),
        };
        let apps = AppIndex::from_entries(vec![
            entry("files", "Files", "files"),
            entry("foot", "Foot", "foot"),
            entry("mail", "Mail", "no-such-icon"),
            entry("notes", "Notes and Lists", ""),
            entry("web", "Web Browser", "web"),
        ]);
        let pinned: &[&str] = if state == "empty" {
            &[]
        } else {
            &["foot", "web", "mail"]
        };
        let dock = Dock::new(
            apps,
            pinned.iter().map(|s| s.to_string()).collect(),
            Vec::new(),
            Env::from_process(),
        );
        let app = App::new(dock_ui.clone_strong(), dock, icon_dirs, deliver, resize);
        app.set_windows(&windows_for(&state));
        app
    };
    APP.set(Some(app.clone()));

    let item_centre = |index: f32| {
        // Slots are all one width, so the pointer position follows from it.
        let (width, height) = app.size();
        let count = app.ui().get_items().iter().count() as f32;
        let slot = (width as f32 - 8.0) / count.max(1.0);
        LogicalPosition::new(4.0 + slot * (index + 0.5), height as f32 - 28.0)
    };

    if let Some(index) = hover.or(press) {
        let position = item_centre(index);
        let pressed = press.is_some();
        let weak = dock_ui.as_weak();
        after(200, move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.window()
                .dispatch_event(WindowEvent::PointerMoved { position });
            if pressed {
                ui.window().dispatch_event(WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Left,
                });
            }
        });
    }

    if state == "transition" {
        let with_app = vec![window(1, "files", 1, true)];
        let app_in = app.clone();
        after(600, move || app_in.set_windows(&with_app));
        let app_out = app.clone();
        after(1600, move || app_out.set_windows(&[]));
        if let Some(path) = snapshot.take() {
            let stem = path.trim_end_matches(".png").to_string();
            for (ms, tag) in [(680, "in"), (1300, "shown"), (1680, "out"), (2300, "gone")] {
                let weak = dock_ui.as_weak();
                let file = format!("{stem}-{tag}.png");
                after(ms, move || {
                    if let Some(ui) = weak.upgrade() {
                        println!("panel width {}", ui.get_panel_width());
                        save(&ui, &file);
                    }
                });
            }
            after(2400, || slint::quit_event_loop().unwrap());
        }
    } else if let Some(path) = snapshot {
        let weak = dock_ui.as_weak();
        after(900, move || {
            if let Some(ui) = weak.upgrade() {
                save(&ui, &path);
                report_memory();
            }
            slint::quit_event_loop().unwrap();
        });
    }

    dock_ui.run().expect("the event loop");
    std::process::ExitCode::SUCCESS
}
