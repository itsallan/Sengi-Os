//! When not to update. These are checked by the helper, as root, before
//! it does anything, and by Settings before it asks for a password, so
//! that a refusal is said at once and in plain words. The helper's check
//! is the one that counts.

use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// Below this much charge, on battery, nothing is installed: a cut-off
/// half way through unpacking is the worst thing an update can do.
pub const MIN_BATTERY: u8 = 20;

/// Where the facts are read from. Tests point it at a made-up tree.
#[derive(Debug, Clone)]
pub struct Paths {
    pub power_supply: PathBuf,
    /// Files apt and dpkg hold a lock on while they work.
    pub locks: Vec<PathBuf>,
    pub reboot_required: PathBuf,
    pub mounts: PathBuf,
}

impl Paths {
    pub fn system() -> Paths {
        Paths {
            power_supply: PathBuf::from("/sys/class/power_supply"),
            locks: [
                "/var/lib/dpkg/lock-frontend",
                "/var/lib/dpkg/lock",
                "/var/lib/apt/lists/lock",
                "/var/cache/apt/archives/lock",
            ]
            .iter()
            .map(PathBuf::from)
            .collect(),
            reboot_required: PathBuf::from("/run/reboot-required"),
            mounts: PathBuf::from("/proc/mounts"),
        }
    }

    /// The same facts under `root`, for the tests.
    pub fn under(root: &Path) -> Paths {
        Paths {
            power_supply: root.join("sys/class/power_supply"),
            locks: vec![root.join("var/lib/dpkg/lock-frontend")],
            reboot_required: root.join("run/reboot-required"),
            mounts: root.join("proc/mounts"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The live system: its root is in memory and has no package lists.
    Live,
    /// On battery, with this much left.
    Battery(u8),
    /// Another apt or dpkg is running.
    Busy,
    /// Not root, and the helper is not being tested.
    NotRoot,
}

impl Refusal {
    /// A short word for the JSON line.
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::Live => "live",
            Refusal::Battery(_) => "battery",
            Refusal::Busy => "busy",
            Refusal::NotRoot => "root",
        }
    }

    /// What to tell the person.
    pub fn message(&self) -> String {
        match self {
            Refusal::Live => "Updates are available after Sengi OS is installed.".to_string(),
            Refusal::Battery(percent) => format!(
                "The battery is at {percent}%. Plug in the charger before updating, \
                 so that the power cannot run out in the middle of an update."
            ),
            Refusal::Busy => "Another program is installing or updating software. \
                              Wait for it to finish and try again."
                .to_string(),
            Refusal::NotRoot => "The update helper must run as root.".to_string(),
        }
    }

    /// The refusal a JSON line named, with the words it carried.
    pub fn from_code(code: &str, message: &str) -> Option<(Refusal, String)> {
        let refusal = match code {
            "live" => Refusal::Live,
            "battery" => Refusal::Battery(0),
            "busy" => Refusal::Busy,
            "root" => Refusal::NotRoot,
            _ => return None,
        };
        Some((refusal, sengi_text::line(message, 300)))
    }
}

/// Whether the root file system is held in memory: the live system, an
/// overlay or tmpfs on top of the read-only image. This is the check
/// Settings' About page makes too.
pub fn is_live(mounts: &str) -> bool {
    mounts.lines().any(|line| {
        let mut fields = line.split_whitespace().skip(1);
        fields.next() == Some("/") && matches!(fields.next(), Some("overlay" | "tmpfs" | "ramfs"))
    })
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// Charge of the laptop's battery, if it is running on it. Batteries of
/// devices (a mouse, a headset) are not the computer's.
pub fn battery_discharging(power_supply: &Path) -> Option<u8> {
    let mut lowest: Option<u8> = None;
    let entries = fs::read_dir(power_supply).ok()?;
    for entry in entries.flatten().take(32) {
        let dir = entry.path();
        if read(&dir.join("type")).trim() != "Battery" {
            continue;
        }
        if read(&dir.join("scope")).trim() == "Device" {
            continue;
        }
        if read(&dir.join("status")).trim() != "Discharging" {
            continue;
        }
        let Ok(percent) = read(&dir.join("capacity")).trim().parse::<u32>() else {
            continue;
        };
        let percent = percent.min(100) as u8;
        lowest = Some(lowest.map_or(percent, |old| old.min(percent)));
    }
    lowest
}

/// Whether another process holds a write lock on one of these files.
/// apt and dpkg lock with `fcntl`, so that is asked: `F_GETLK` with a
/// read lock reports a conflicting writer, and works on a file that is
/// only open for reading. A file that is not there, or that this user may
/// not open, says nothing.
pub fn locked(files: &[PathBuf]) -> bool {
    files.iter().any(|path| {
        let Ok(file) = fs::File::open(path) else {
            return false;
        };
        // SAFETY: flock is plain data, zero is a valid value of it, and
        // fcntl only reads and writes the structure it is given.
        unsafe {
            let mut probe: libc::flock = std::mem::zeroed();
            probe.l_type = libc::F_RDLCK as libc::c_short;
            probe.l_whence = libc::SEEK_SET as libc::c_short;
            let result = libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut probe);
            result == 0 && probe.l_type != libc::F_UNLCK as libc::c_short
        }
    })
}

pub fn reboot_required(path: &Path) -> bool {
    path.exists()
}

/// Whether an update may start now.
pub fn allow_apply(paths: &Paths) -> Result<(), Refusal> {
    if is_live(&read(&paths.mounts)) {
        return Err(Refusal::Live);
    }
    if let Some(percent) = battery_discharging(&paths.power_supply) {
        if percent < MIN_BATTERY {
            return Err(Refusal::Battery(percent));
        }
    }
    if locked(&paths.locks) {
        return Err(Refusal::Busy);
    }
    Ok(())
}

/// Whether looking for updates makes sense: not on the live system. A
/// check neither needs the locks nor minds a low battery.
pub fn allow_check(paths: &Paths) -> Result<(), Refusal> {
    if is_live(&read(&paths.mounts)) {
        return Err(Refusal::Live);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    fn battery(root: &Path, name: &str, status: &str, capacity: &str, scope: Option<&str>) {
        let dir = root.join("sys/class/power_supply").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("type"), "Battery\n").unwrap();
        fs::write(dir.join("status"), format!("{status}\n")).unwrap();
        fs::write(dir.join("capacity"), format!("{capacity}\n")).unwrap();
        if let Some(scope) = scope {
            fs::write(dir.join("scope"), format!("{scope}\n")).unwrap();
        }
    }

    #[test]
    fn a_live_root_is_refused() {
        let live = "overlay / overlay rw,relatime,lowerdir=/ro 0 0\nproc /proc proc rw 0 0\n";
        assert!(is_live(live));
        assert!(is_live("tmpfs / tmpfs rw 0 0\n"));
        assert!(!is_live(
            "/dev/sda2 / ext4 rw,relatime 0 0\ntmpfs /tmp tmpfs rw 0 0\n"
        ));
        assert!(!is_live(""));

        let root = scratch("live");
        fs::create_dir_all(root.join("proc")).unwrap();
        fs::write(root.join("proc/mounts"), live).unwrap();
        let paths = Paths::under(&root);
        assert_eq!(allow_apply(&paths), Err(Refusal::Live));
        assert_eq!(allow_check(&paths), Err(Refusal::Live));
    }

    #[test]
    fn a_low_battery_that_is_discharging_is_refused() {
        let root = scratch("battery");
        battery(&root, "BAT0", "Discharging", "19", None);
        let paths = Paths::under(&root);
        assert_eq!(allow_apply(&paths), Err(Refusal::Battery(19)));
        // A check does not mind.
        assert_eq!(allow_check(&paths), Ok(()));
        // 20 is enough.
        battery(&root, "BAT0", "Discharging", "20", None);
        assert_eq!(allow_apply(&paths), Ok(()));
        // On the charger, any level is.
        battery(&root, "BAT0", "Charging", "3", None);
        assert_eq!(allow_apply(&paths), Ok(()));
        battery(&root, "BAT0", "Not charging", "3", None);
        assert_eq!(allow_apply(&paths), Ok(()));
        // The mouse's battery is not the computer's.
        battery(&root, "BAT0", "Full", "100", None);
        battery(&root, "hid-1", "Discharging", "5", Some("Device"));
        assert_eq!(allow_apply(&paths), Ok(()));
        // A computer with no battery has nothing to run out.
        let bare = scratch("bare");
        assert_eq!(allow_apply(&Paths::under(&bare)), Ok(()));
    }

    #[test]
    fn the_lowest_of_two_batteries_counts() {
        let root = scratch("two");
        battery(&root, "BAT0", "Discharging", "60", None);
        battery(&root, "BAT1", "Discharging", "10", None);
        assert_eq!(
            battery_discharging(&root.join("sys/class/power_supply")),
            Some(10)
        );
    }

    #[test]
    fn a_file_somebody_else_has_locked_is_busy() {
        let root = scratch("lock");
        let lock = root.join("var/lib/dpkg/lock-frontend");
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        fs::write(&lock, "").unwrap();
        let files = vec![lock.clone()];
        assert!(!locked(&files));
        assert!(!locked(&[root.join("not/there")]));

        // fcntl locks belong to a process, and a shell cannot take one,
        // so the holder is a forked copy of this process.
        let path = std::ffi::CString::new(lock.to_str().unwrap()).unwrap();
        // SAFETY: the child does nothing but call fcntl and pause.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: only async-signal-safe calls between fork and _exit.
            unsafe {
                let fd = libc::open(path.as_ptr(), libc::O_RDWR);
                let mut want: libc::flock = std::mem::zeroed();
                want.l_type = libc::F_WRLCK as libc::c_short;
                want.l_whence = libc::SEEK_SET as libc::c_short;
                libc::fcntl(fd, libc::F_SETLK, &want);
                libc::pause();
                libc::_exit(0);
            }
        }
        assert!(pid > 0);
        // Give the child a moment to take the lock.
        let mut seen = false;
        for _ in 0..100 {
            if locked(&files) {
                seen = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // SAFETY: ends and reaps the child made above.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
        assert!(seen, "a lock held by another process is seen");
        assert!(!locked(&files), "and the lock is gone with its holder");
    }

    #[test]
    fn refusals_say_what_is_wrong() {
        for refusal in [
            Refusal::Live,
            Refusal::Battery(12),
            Refusal::Busy,
            Refusal::NotRoot,
        ] {
            assert!(!refusal.message().is_empty());
            let (back, words) =
                Refusal::from_code(refusal.code(), &refusal.message()).expect("a known code");
            assert_eq!(back.code(), refusal.code());
            assert_eq!(words, refusal.message());
        }
        assert!(Refusal::Battery(12).message().contains("12%"));
        assert!(Refusal::from_code("nonsense", "x").is_none());
    }
}
