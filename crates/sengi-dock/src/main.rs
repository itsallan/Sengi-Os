//! sengi-dock, the Sengi OS dock.
//!
//! All the logic lives in the library. This file puts the Slint component
//! on a layer surface, which is sengi-layer's job.

use std::process::ExitCode;
use std::sync::mpsc;

use sengi_dock::model::Dock;
use sengi_dock::windows::{self, Source, Window};
use sengi_dock::{config, desktop};

const USAGE: &str = "usage: sengi-dock [--dump]

  --dump   print the open windows and the dock's items, and again whenever
           they change, instead of showing the dock";

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // ------------------------------------------------------------------
    // sengi-layer goes here.
    //
    // The dock is a layer surface in the top layer, anchored to the bottom
    // edge and centred, sized to its content, with an exclusive zone so
    // windows do not cover it and no keyboard focus. Roughly:
    //
    //     let (to_ui, from_tracker) = <sengi-layer's cross-thread channel>;
    //     sengi_layer::run(config, move |ctx| {
    //         let ui = sengi_dock::ui::Dock::new()?;
    //         let resize = ctx.resizer();
    //         let app = App::start(
    //             ui,
    //             ctx.scale_factor(),
    //             Source::Shared(ctx.connection()),   // or Source::Own
    //             move |windows| to_ui.send(windows),
    //             move |width, height| resize.set_size(width, height),
    //         );
    //         ctx.on_message(from_tracker, {
    //             let app = app.clone();
    //             move |windows: Vec<Window>| app.set_windows(&windows)
    //         });
    //         ctx.on_scale_changed({
    //             let app = app.clone();
    //             move |scale| app.set_scale(scale)
    //         });
    //         Ok(app.ui().clone_strong())
    //     })
    //
    // with, for the surface config, read from the component before the
    // surface is made:
    //
    //     size            app.size()
    //     margin bottom   ui.get_bottom_margin()
    //     exclusive zone  ui.get_panel_height() + ui.get_bottom_margin()
    //     namespace       "sengi-dock"
    // ------------------------------------------------------------------
    Err(
        "the dock cannot be shown yet: this build has no layer-shell backend. \
         Try --dump, or the preview example."
            .into(),
    )
}

fn print(dock: &mut Dock, windows: &[Window]) {
    println!("windows:");
    for w in windows {
        println!(
            "  #{} app_id={:?} title={:?} activated={} minimized={} maximized={} focus_seq={}",
            w.id, w.app_id, w.title, w.activated, w.minimized, w.maximized, w.focus_seq
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
