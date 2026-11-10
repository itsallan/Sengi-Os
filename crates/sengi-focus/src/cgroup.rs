//! Reads the user's app scopes from the cgroup tree and sets their weights
//! through the user service manager.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::policy::Scope;

/// Weight of the focused app. Unfocused apps sit at the default of 100, and
/// the compositor's session scope at 400, so a focused app gets most of what
/// the desktop itself is not using.
pub const FOCUSED_WEIGHT: u32 = 300;
pub const DEFAULT_WEIGHT: u32 = 100;

/// `app.slice` of the user this process runs as.
pub fn app_slice() -> io::Result<PathBuf> {
    let uid = fs::metadata("/proc/self")?.uid();
    Ok(PathBuf::from(format!(
        "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice"
    )))
}

pub fn scopes(app_slice: &Path) -> Vec<Scope> {
    let Ok(entries) = fs::read_dir(app_slice) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let unit = entry.file_name().into_string().ok()?;
            if !unit.ends_with(".scope") {
                return None;
            }
            let procs = fs::read_to_string(entry.path().join("cgroup.procs")).unwrap_or_default();
            let processes = procs
                .lines()
                .filter_map(|pid| fs::read_to_string(format!("/proc/{pid}/comm")).ok())
                .map(|comm| comm.trim_end().to_string())
                .collect();
            Some(Scope { unit, processes })
        })
        .collect()
}

pub fn set_weight(unit: &str, weight: u32) {
    let status = Command::new("systemctl")
        .args(["--user", "--runtime", "set-property", unit])
        .arg(format!("CPUWeight={weight}"))
        .arg(format!("IOWeight={weight}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // A scope that exited between the scan and now is not worth reporting.
    if let Err(error) = status {
        eprintln!("sengi-focus: could not run systemctl: {error}");
    }
}
