//! One panel at a time, through sengi-system's instance socket. A second
//! invocation asks the open panel to close and does not open one of its
//! own, so the same click or key opens and closes it.

use std::path::Path;

use sengi_system::instance::Wire;
pub use sengi_system::instance::{Claim, Instance};

/// Become the one panel, or close the one that is open.
pub fn claim(runtime_dir: Option<&Path>) -> Claim {
    sengi_system::instance::claim(
        runtime_dir,
        &Wire {
            socket_name: "sengi-quick.sock",
            lock_name: "sengi-quick.lock",
            ask: b'c',
            answer: b'k',
            accepted: b"c",
        },
    )
}
