//! sengi-update, the helper behind Settings, Updates.
//!
//!     sengi-update check     list what could be updated, one JSON line each
//!     sengi-update apply     update the system packages (needs root)
//!     sengi-update status    say what would stop an update, as one JSON line
//!
//! No other arguments are accepted. See the README.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(sengi_update::helper::main_with(&args));
}
