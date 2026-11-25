//! Which programs are run, and from where.
//!
//! The helper runs as root, so it names apt-get by absolute path and
//! never looks at `PATH`. Settings runs as the user and finds `flatpak`
//! and `pkexec` the ordinary way, which is also how the tests put fakes
//! in their place.

use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Tools {
    pub apt_get: PathBuf,
    pub flatpak: OsString,
    pub pkexec: OsString,
    /// What pkexec is asked to run. The polkit action is for this path.
    pub helper: PathBuf,
}

/// Where the helper is installed, and what the polkit action names.
pub const HELPER: &str = "/usr/bin/sengi-update";

impl Tools {
    /// The programs of an installed system.
    pub fn system() -> Tools {
        Tools {
            apt_get: PathBuf::from("/usr/bin/apt-get"),
            flatpak: OsString::from("flatpak"),
            pkexec: OsString::from("pkexec"),
            helper: PathBuf::from(HELPER),
        }
    }

    /// For the program that runs as the user: the system's tools, and a
    /// helper somewhere else when `SENGI_UPDATE_HELPER` says so (the
    /// tests and a build tree; pkexec will not authorize another path
    /// for this action, so it only ever asks for more).
    pub fn for_user() -> Tools {
        let mut tools = Tools::system();
        if let Some(path) = std::env::var_os("SENGI_UPDATE_HELPER") {
            tools.helper = PathBuf::from(path);
        }
        tools
    }
}
