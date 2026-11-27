//! sengi-desktop, the Sengi OS desktop.
//!
//! All the logic lives in the library. This file puts the Slint component
//! on a layer surface on every output, between the wallpaper and the windows.

use std::process::ExitCode;

use sengi_apps::Env;
use sengi_desktop::app::Shell;
use sengi_desktop::desk::Desk;
use sengi_desktop::menu::{self, Choice, Request};
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

    // One menu at a time; each answers over this channel when it goes.
    let answers = layer.handle().channel({
        let shell = shell.clone();
        move |choice: Choice| shell.chosen(choice)
    })?;
    let mut open: Option<menu::Open> = None;
    shell.set_menus(move |request| {
        // The one before goes first.
        drop(open.take());
        let Some(request) = request else {
            return Ok(());
        };
        let answers = answers.clone();
        let shown = menu::show(&request, move |choice| {
            if let Some(choice) = choice {
                let _ = answers.send(choice);
            }
        })
        .map_err(|error| format!("cannot show the menu: {error}"))?;
        open = Some(shown);
        Ok(())
    });

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

    layer.run(config, move |context| {
        let window = ui::Desktop::new()?;
        sengi_config::apply_theme!(slint::ComponentHandle::global::<ui::ThemeSwitch>(&window));
        window.set_output(context.output_name().unwrap_or_default().into());
        shell.attach(&window);
        Ok(window)
    })?;
    Ok(())
}

/// `--menu`: one menu over everything, until something on it is chosen or it
/// is put away. The word of the choice goes to standard output.
fn show_menu(request: Request) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::new("sengi-desktop-menu")
        .layer(Layer::Overlay)
        .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
        .size(0, 0)
        .exclusive_zone(-1)
        .keyboard(KeyboardMode::Exclusive)
        .outputs(Outputs::All);
    sengi_layer::run(config, move |context| {
        let window = ui::DesktopMenu::new()?;
        sengi_config::apply_theme!(slint::ComponentHandle::global::<ui::ThemeSwitch>(&window));
        window.set_shown(context.output_name().unwrap_or_default() == request.output);
        window.set_for_item(request.item);
        window.set_menu_x(request.x);
        window.set_menu_y(request.y);
        let handle = context.handle();
        window.on_chosen(move |word| {
            println!("{word}");
            handle.quit();
        });
        let handle = context.handle();
        window.on_dismissed(move || handle.quit());
        Ok(window)
    })?;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Some(request) = Request::parse(&args) {
        return finish(show_menu(request));
    }
    match args[..] {
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
    finish(run())
}

fn finish(result: Result<(), Box<dyn std::error::Error>>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-desktop: {error}");
            ExitCode::FAILURE
        }
    }
}
