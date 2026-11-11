//! sengi-dock, the Sengi OS dock.
//!
//! All the logic lives in the library. This file puts the Slint component
//! on a layer surface through sengi-layer.

use std::cell::RefCell;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};

use sengi_apps::Env;
use sengi_dock::app::{App, Deliver, Message};
use sengi_dock::model::Dock;
use sengi_dock::windows::{self, Source, Window};
use sengi_dock::{config, icons, ui};
use sengi_layer::{Anchor, Config, KeyboardMode, Layer, Margins, Output, Outputs, Rect};
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
        move |message: Message| {
            if let Some(dock) = dock.borrow().as_ref() {
                dock.receive(message);
            }
        }
    })?;
    let to_ui = Mutex::new(to_ui);
    let deliver: Deliver = Arc::new(move |message| {
        let _ = to_ui.lock().unwrap().send(message);
    });

    // One dock, on the output the compositor picks. Anchored to the bottom
    // edge only, which centres it. The real size, margin and exclusive zone
    // come from the component, which does not exist yet.
    let config = Config::new("sengi-dock")
        .layer(Layer::Top)
        .anchor(Anchor::BOTTOM)
        .keyboard(KeyboardMode::None)
        .outputs(Outputs::Auto);

    layer.run(config, move |context| {
        let ui = ui::Dock::new()?;
        // The name of the app under the pointer, shown above the panel. Set
        // this to false for a surface that is exactly the panel.
        ui.set_show_labels(true);
        let handle = context.handle();
        let surface = context.surface();

        let margin = ui.get_bottom_margin().round() as i32;
        handle.set_margins(Margins {
            bottom: margin,
            ..Margins::default()
        });
        // The compositor adds the margin to this itself. The same gap again
        // keeps windows as far from the dock as the dock is from the edge.
        handle.set_exclusive_zone(ui.get_panel_height().round() as i32 + margin);

        let panel_height = ui.get_panel_height().round() as i32;
        let resize = surface.clone();
        let app = App::start(
            ui.clone_strong(),
            Source::Shared(handle.connection()),
            deliver.clone(),
            move |width, height| {
                if let Err(error) = resize.set_size(width, height) {
                    eprintln!("sengi-dock: {error}");
                }
                // The surface is taller than the panel, to have room for
                // the label. Only the panel takes the pointer, or the strip
                // above it would swallow clicks meant for the window there.
                // The region does not follow a resize, so it is set each time.
                let (width, height) = (width as i32, height as i32);
                let panel = Rect::new(0, height - panel_height, width, panel_height);
                resize.set_input_region(Some(&[panel]));
            },
        );

        // The dock may be as wide as its output, less the gap it keeps from
        // the bottom edge on either side. The output is often not known
        // until the surface is on screen.
        let fit_output = {
            let app = app.clone();
            move |output: Output| {
                if let Some((width, _)) = output.size {
                    app.set_max_width(width.saturating_sub(2 * margin.unsigned_abs()));
                }
            }
        };
        if let Some(output) = surface.output() {
            fit_output(output);
        }
        surface.on_output_changed(fit_output);

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
    let env = Env::from_process();
    let mut dock = Dock::load(config::pinned(&config::paths()), env.data_roots(), env);
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
    // The dock decodes icons by running itself; see icons::Decoder.
    if let Some(code) = icons::run_decoder(&args) {
        return code;
    }
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
