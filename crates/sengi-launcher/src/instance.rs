//! One panel at a time, through sengi-system's instance socket. Running
//! the same command again closes the open panel, so one key binding
//! toggles it; running the other kind replaces it.

use std::path::Path;

use sengi_system::instance::Wire;
pub use sengi_system::instance::{Claim, Instance};

use crate::cli::Mode;

fn mode_byte(mode: Mode) -> u8 {
    match mode {
        Mode::Apps => b'a',
        Mode::Menu => b'm',
    }
}

/// Become the one launcher, closing any that is already open.
pub fn claim(runtime_dir: Option<&Path>, mode: Mode) -> Claim {
    sengi_system::instance::claim(
        runtime_dir,
        &Wire {
            socket_name: "sengi-launcher.sock",
            lock_name: "sengi-launcher.lock",
            ask: mode_byte(mode),
            answer: mode_byte(mode),
            accepted: b"am",
        },
    )
}
