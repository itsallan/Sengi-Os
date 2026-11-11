//! sengi-compositor: the Wayland compositor for Sengi OS.
//!
//! Still a skeleton. See README.md for what works and what is missing.

mod cli;
mod headless;
mod input;
mod layer;
mod layout;
mod png;
mod render;
mod shell;
mod state;
#[cfg(feature = "winit")]
mod winit;

use std::{error::Error, process::ExitCode};

use cli::{Backend, Command, Options};
use state::State;

fn main() -> ExitCode {
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
        Err(err) => {
            eprintln!("sengi-compositor: {err}\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };

    let has_display = ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    let result = match options.resolve_backend(has_display) {
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
fn start_clients(state: &State, options: &Options) -> Result<(), Box<dyn Error>> {
    println!("{}", state.socket_name.to_string_lossy());

    for command in &options.startup {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("WAYLAND_DISPLAY", &state.socket_name)
            .spawn()
            .map_err(|err| format!("cannot start `{command}`: {err}"))?;
    }
    Ok(())
}
