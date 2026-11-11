//! sengi-compositor: the Wayland compositor for Sengi OS.
//!
//! Still a skeleton. See README.md for what works and what is missing.

mod cli;
mod cursor;
mod decoration;
#[cfg(any(feature = "winit", feature = "drm"))]
mod device_input;
#[cfg(feature = "drm")]
mod drm;
mod foreign_toplevel;
mod frame;
#[cfg(feature = "drm")]
mod gpu;
mod headless;
mod input;
mod keys;
mod layer;
mod layout;
mod lock;
mod log;
mod png;
mod render;
mod script;
mod shell;
mod state;
mod text;
mod window;
#[cfg(feature = "winit")]
mod winit;

use std::process::ExitCode;

use cli::{Backend, Command, Options};
use state::State;

fn main() -> ExitCode {
    // Not for users: the drm backend runs this in a child to learn what GL
    // would render with, without loading Mesa into the compositor itself.
    #[cfg(feature = "drm")]
    {
        let mut args = std::env::args().skip(1);
        if args.next().as_deref() == Some("--probe-gl") {
            return match args.next().map(|node| drm::probe(&node)) {
                Some(Ok(renderer)) => {
                    println!("{renderer}");
                    ExitCode::SUCCESS
                }
                Some(Err(err)) => {
                    eprintln!("sengi-compositor: no GL on that device: {err}");
                    ExitCode::FAILURE
                }
                None => ExitCode::from(2),
            };
        }
    }

    let options = match cli::parse(std::env::args().skip(1)) {
        Ok(Command::Run(options)) => options,
        Ok(Command::Help) => {
            println!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Command::Version) => {
            println!("sengi-compositor {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(Command::Exit) => return ask_to_exit(),
        Err(err) => {
            eprintln!("sengi-compositor: {err}\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };

    log::init();

    let has_display = ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    // greetd and a login on a console both give the session a terminal.
    let on_terminal = ["XDG_VTNR", "XDG_SEAT"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    let on_terminal = on_terminal && cfg!(feature = "drm");
    let result = match options.resolve_backend(has_display, on_terminal) {
        #[cfg(feature = "drm")]
        Backend::Drm => drm::run(options),
        #[cfg(not(feature = "drm"))]
        Backend::Drm => Err("this build has no drm backend".into()),
        #[cfg(feature = "winit")]
        Backend::Winit => winit::run(options),
        #[cfg(not(feature = "winit"))]
        Backend::Winit => Err("this build has no winit backend".into()),
        Backend::Headless | Backend::Auto => headless::run(options),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("sengi-compositor: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Prints the socket name and launches the `--startup` commands against it.
fn start_clients(state: &mut State, options: &Options) {
    println!("{}", state.socket_name.to_string_lossy());

    if let Some(locker) = &options.locker {
        state.locker_command = locker.clone();
    }
    for command in &options.startup {
        state.spawn(command);
    }
}

/// `--exit`: tells the compositor this was started under to quit, the way
/// `labwc --exit` does. It finds it through the pid it gave its children.
fn ask_to_exit() -> ExitCode {
    let pid = std::env::var(state::PID_VARIABLE)
        .ok()
        .filter(|pid| pid.parse::<u32>().is_ok());
    let Some(pid) = pid else {
        eprintln!(
            "sengi-compositor: {} is not set; not started from a session?",
            state::PID_VARIABLE
        );
        return ExitCode::FAILURE;
    };
    match std::process::Command::new("kill")
        .args(["-TERM", &pid])
        .status()
    {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        _ => {
            eprintln!("sengi-compositor: could not signal process {pid}");
            ExitCode::FAILURE
        }
    }
}
