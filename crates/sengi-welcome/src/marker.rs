//! Once only: a file in the user's state directory says the welcome has
//! been seen, and the live image never shows it at all.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The kernel command line word the live image is started with.
const LIVE_WORD: &str = "boot=sengi";

/// The marker under the state directory (`~/.local/state`).
pub fn path(state_home: &Path) -> PathBuf {
    state_home.join("sengi-os").join("welcome-done")
}

pub fn is_done(state_home: &Path) -> bool {
    path(state_home).exists()
}

pub fn mark_done(state_home: &Path) -> io::Result<()> {
    let file = path(state_home);
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(file, "")
}

/// Whether the kernel command line is the live image's.
pub fn is_live(cmdline: &str) -> bool {
    cmdline.split_whitespace().any(|word| word == LIVE_WORD)
}

pub fn running_live() -> bool {
    fs::read_to_string("/proc/cmdline").is_ok_and(|cmdline| is_live(&cmdline))
}

/// Whether a first login should show the welcome: on an installed system,
/// and not yet seen.
pub fn should_show(live: bool, done: bool) -> bool {
    !live && !done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sengi-welcome-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_marker_is_made_once_and_found() {
        let state = temp("marker");
        assert!(!is_done(&state));
        mark_done(&state).unwrap();
        assert!(is_done(&state));
        // A second time does no harm.
        mark_done(&state).unwrap();
        assert_eq!(path(&state), state.join("sengi-os/welcome-done"));
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn the_live_image_is_told_by_its_boot_word() {
        assert!(is_live("BOOT_IMAGE=/vmlinuz boot=sengi quiet splash"));
        assert!(!is_live("BOOT_IMAGE=/vmlinuz root=UUID=1234 ro quiet"));
        assert!(!is_live("root=/dev/sda1 notboot=sengi"));
        assert!(!is_live(""));
    }

    #[test]
    fn it_shows_only_on_an_installed_system_and_only_once() {
        assert!(should_show(false, false));
        assert!(!should_show(true, false));
        assert!(!should_show(false, true));
        assert!(!should_show(true, true));
    }
}
