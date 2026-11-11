//! sengi-dock, the Sengi OS dock.
//!
//! All the logic lives in the library. This file puts the Slint component
//! on a layer surface through sengi-layer.

use std::cell::RefCell;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::mpsc;

use sengi_dock::app::App;
use sengi_dock::model::Dock;
use sengi_dock::windows::{self, Source, Window};
use sengi_dock::{config, desktop, ui};
use sengi_layer::{Anchor, Config, KeyboardMode, Layer, Margins, Outputs};
use slint::ComponentHandle;

const USAGE: &str = "usage: sengi-dock [--dump]

  --dump   print the open windows and the dock's items, and again whenever
           they change, instead of showing the dock";

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let layer = sengi_layer::App::new()?;
    let handle = layer.handle();

    // The dock itself, once the component exists.
    let dock: Rc<RefCell<Option<App>>> = Rc::default();
    let to_ui = handle.channel({
        let dock = dock.clone();
        move |windows: Vec<Window>| {
            if let Some(dock) = dock.borrow().as_ref() {
                dock.set_windows(&windows);
            }
        }
    })?;

    // One dock, on the output the compositor picks. Anchored to the bottom
    // edge only, which centres it. The real size, margin and exclusive zone
    // come from the component, which does not exist yet.
    let config = Config::new("sengi-dock")
        .layer(Layer::Top)
        .anchor(Anchor::BOTTOM)
        .keyboard(KeyboardMode::None)
        .outputs(Outputs::Primary);

    layer.run(config, move |context| {
        let ui = ui::Dock::new()?;
        // Off until sengi-layer can limit a surface's input region: the room
        // for the label would swallow clicks meant for the window above.
        ui.set_show_labels(false);
        let handle = context.handle();

        let margin = ui.get_bottom_margin().round() as i32;
        handle.set_margins(Margins {
            bottom: margin,
            ..Margins::default()
        });
        // The compositor adds the margin to this itself. The same gap again
        // keeps windows as far from the dock as the dock is from the edge.
        handle.set_exclusive_zone(ui.get_panel_height().round() as i32 + margin);

        let to_ui = to_ui.clone();
        let resize = handle.clone();
        let app = App::start(
            ui.clone_strong(),
            Source::Shared(handle.connection()),
            move |windows| {
                let _ = to_ui.send(windows);
            },
            move |width, height| {
                if let Err(error) = resize.set_size(width, height) {
                    eprintln!("sengi-dock: {error}");
                }
            },
        );
        *dock.borrow_mut() = Some(app);
        Ok(ui)
    })?;
    Ok(())
}

fn print(dock: &mut Dock, windows: &[Window]) {
    println!("windows:");
    for w in windows {
        println!(
            "  #{} app_id={:?} title={:?} activated={} minimized={} maximized={} focus_seq={} parent={:?}",
            w.id, w.app_id, w.title, w.activated, w.minimized, w.maximized, w.focus_seq, w.parent
        );
    }
    println!("dock:");
    for item in dock.items(windows) {
        println!(
            "  {:?} key={:?} name={:?} icon={:?} running={} focused={} windows={}",
            item.kind,
            item.key,
            item.name,
            item.icon,
            item.running(),
            item.focused(),
            item.window_count()
        );
    }
    println!();
}

fn dump() -> Result<(), Box<dyn std::error::Error>> {
    let data_dirs = desktop::data_dirs();
    let mut dock = Dock::load(config::pinned(&config::paths()), data_dirs);
    let (sender, receiver) = mpsc::channel::<Vec<Window>>();
    let _controller = windows::spawn(Source::Own, move |list| {
        let _ = sender.send(list.to_vec());
    })?;
    // Ends when the compositor does: the tracker drops its sender.
    for list in receiver {
        print(&mut dock, &list);
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => run(),
        ["--dump"] => dump(),
        ["--help" | "-h"] => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-dock: {error}");
            ExitCode::FAILURE
        }
    }
}
