//! sengi-desktop, the Sengi OS desktop.
//!
//! All the logic lives in the library. This file puts the Slint component
//! on a layer surface on every output, between the wallpaper and the windows.

use std::process::ExitCode;

use sengi_apps::Env;
use sengi_desktop::app::Shell;
use sengi_desktop::desk::Desk;
use sengi_desktop::{files, ui, watch};
use sengi_layer::{Anchor, Config, KeyboardMode, Layer, Outputs};

const USAGE: &str = "usage: sengi-desktop";

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::from_process();
    let dir = files::desktop_dir(&env.home);
    files::ensure(&dir).map_err(|error| format!("cannot make {}: {error}", dir.display()))?;

    let layer = sengi_layer::App::new()?;
    let shell = Shell::new(Desk::new(dir.clone(), env));

    // The folder is read again when something in it changes, and not
    // otherwise.
    let changed = layer.handle().channel({
        let shell = shell.clone();
        move |(): ()| shell.refresh()
    })?;
    if let Err(error) = watch::spawn(dir, move || {
        let _ = changed.send(());
    }) {
        eprintln!("sengi-desktop: changes in the folder will not show: {error}");
    }

    // Over the whole output, below the windows, whatever the bar and the
    // dock reserve (-1): compositors differ in whether they shrink such a
    // surface, so the component keeps its icons clear of them itself.
    // Windows get the keyboard back when they are clicked.
    let config = Config::new("sengi-desktop")
        .layer(Layer::Bottom)
        .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
        .size(0, 0)
        .exclusive_zone(-1)
        .keyboard(KeyboardMode::OnDemand)
        .outputs(Outputs::All);

    layer.run(config, move |_context| {
        let window = ui::Desktop::new()?;
        sengi_config::apply_theme!(slint::ComponentHandle::global::<ui::ThemeSwitch>(&window));
        shell.attach(&window);
        Ok(window)
    })?;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => {}
        ["--help" | "-h"] => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-desktop: {error}");
            ExitCode::FAILURE
        }
    }
}
