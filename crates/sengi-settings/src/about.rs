//! The About page: what this is and what it runs on. Everything is read
//! from files the kernel and the system keep: `/etc/os-release`, `/proc`
//! and `/sys`. Their contents are shown, so they are cleaned like any
//! other text from outside.

use std::fs;
use std::path::Path;
use std::rc::Rc;

use slint::ComponentHandle;

use crate::app::{self, App, Controller};
use crate::text;
use crate::ui::{AboutData, InfoRow};

const MAX_FILE: u64 = 4 << 20;

fn read(path: &Path) -> String {
    let fits = fs::metadata(path).is_ok_and(|meta| meta.len() <= MAX_FILE);
    if fits {
        fs::read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    } else {
        String::new()
    }
}

/// One value of `os-release`: `KEY=value`, the value perhaps in quotes.
pub fn os_release_value(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let value = line.trim().strip_prefix(key)?.strip_prefix('=')?.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        let value = text::clean(&value.replace("\\\"", "\"").replace("\\$", "$"), 48);
        (!value.is_empty()).then_some(value)
    })
}

/// The system's name and its version, as `os-release` has them.
pub fn system_name(contents: &str) -> (String, String) {
    let name = os_release_value(contents, "NAME").unwrap_or_else(|| "Sengi OS".to_string());
    let version = os_release_value(contents, "VERSION")
        .or_else(|| os_release_value(contents, "VERSION_ID"))
        .unwrap_or_default();
    (name, version)
}

/// The processor's name and how many threads it runs, from
/// `/proc/cpuinfo`.
pub fn processor(cpuinfo: &str) -> String {
    let field = |name: &str| {
        cpuinfo.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == name).then(|| text::clean(value, 64))
        })
    };
    let threads = cpuinfo
        .lines()
        .filter(|line| {
            line.split(':')
                .next()
                .is_some_and(|key| key.trim() == "processor")
        })
        .count();
    // x86 says "model name"; some ARM boards only say "Hardware".
    let name = field("model name")
        .or_else(|| field("Hardware"))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Unknown".to_string());
    match threads {
        0 | 1 => name,
        threads => format!("{name} ({threads} threads)"),
    }
}

/// A value of `/proc/meminfo`, in kibibytes.
pub fn meminfo(contents: &str, key: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?.strip_prefix(':')?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

/// An amount of memory or disk for people: `15.5 GB`, `412 MB`.
pub fn amount(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let gigabytes = bytes as f64 / GB;
    if gigabytes >= 10.0 {
        format!("{gigabytes:.0} GB")
    } else if gigabytes >= 1.0 {
        format!("{gigabytes:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Installed memory and how much of it is in use now. "In use" is what
/// the kernel could not hand to a new program without swapping: total
/// less `MemAvailable`. It is the figure the 400 MB idle budget is
/// measured in.
pub fn memory(contents: &str) -> (String, String) {
    let total = meminfo(contents, "MemTotal");
    let available = meminfo(contents, "MemAvailable");
    let installed = total.map_or("Unknown".to_string(), |kb| amount(kb * 1024));
    let used = match (total, available) {
        (Some(total), Some(available)) if total >= available => {
            format!("{} MB", (total - available) / 1024)
        }
        _ => "Unknown".to_string(),
    };
    (installed, used)
}

/// The maker of a graphics adapter, from its PCI vendor number. The
/// image carries no PCI database; these are the ones a laptop has.
fn vendor(id: &str) -> Option<&'static str> {
    Some(match id.to_ascii_lowercase().as_str() {
        "8086" => "Intel",
        "10de" => "NVIDIA",
        "1002" | "1022" => "AMD",
        "1af4" => "Virtio",
        "1234" => "QEMU",
        "15ad" => "VMware",
        "80ee" => "VirtualBox",
        _ => return None,
    })
}

/// One graphics adapter from its `uevent` file: maker and driver.
pub fn adapter(uevent: &str) -> Option<String> {
    let field = |name: &str| {
        uevent.lines().find_map(|line| {
            line.strip_prefix(name)?
                .strip_prefix('=')
                .map(|value| text::clean(value, 32))
        })
    };
    let driver = field("DRIVER").filter(|driver| !driver.is_empty())?;
    let maker =
        field("PCI_ID").and_then(|id| id.split_once(':').and_then(|(maker, _)| vendor(maker)));
    Some(match maker {
        Some(maker) => format!("{maker} ({driver})"),
        None => driver,
    })
}

/// The graphics adapters under `/sys/class/drm`, each once.
pub fn graphics(drm: &Path) -> String {
    let mut found: Vec<String> = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(drm)
        .into_iter()
        .flatten()
        .flatten()
        .take(64)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        // card0, card1: not their connectors (card0-eDP-1).
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        if let Some(adapter) = adapter(&read(&entry.path().join("device/uevent"))) {
            if !found.contains(&adapter) && found.len() < 4 {
                found.push(adapter);
            }
        }
    }
    if found.is_empty() {
        "Unknown".to_string()
    } else {
        found.join(", ")
    }
}

/// How full the file system holding `path` is.
// The fields are 64 bits wide here and narrower on other machines.
#[allow(clippy::useless_conversion)]
fn disk(path: &str) -> String {
    let Ok(path) = std::ffi::CString::new(path) else {
        return "Unknown".to_string();
    };
    // SAFETY: statvfs is given a valid C string and a zeroed structure of
    // its own type to fill, which is only read if the call succeeded.
    let stat = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(path.as_ptr(), &mut stat) != 0 {
            return "Unknown".to_string();
        }
        stat
    };
    disk_words(
        u64::from(stat.f_blocks).saturating_mul(u64::from(stat.f_frsize)),
        u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize)),
    )
}

pub fn disk_words(total: u64, free: u64) -> String {
    if total == 0 {
        return "Unknown".to_string();
    }
    format!("{} free of {}", amount(free.min(total)), amount(total))
}

fn row(title: &str, value: String) -> InfoRow {
    InfoRow {
        title: title.into(),
        value: value.into(),
    }
}

pub struct AboutPage;

impl AboutPage {
    pub fn attach(app: &Rc<App>) -> Rc<AboutPage> {
        let data = app.window.global::<AboutData>();
        data.on_refresh(AboutPage::refresh);
        Rc::new(AboutPage)
    }

    fn refresh() {
        std::thread::spawn(|| {
            let (name, version) = system_name(&read(Path::new("/etc/os-release")));
            let kernel = text::clean(&read(Path::new("/proc/sys/kernel/osrelease")), 48);
            let (installed, used) = memory(&read(Path::new("/proc/meminfo")));
            let hardware = vec![
                row("Processor", processor(&read(Path::new("/proc/cpuinfo")))),
                row("Memory", installed),
                row("Graphics", graphics(Path::new("/sys/class/drm"))),
                row("Disk", disk("/")),
            ];
            app::post(move |app| {
                let data = app.window.global::<AboutData>();
                data.set_name(name.into());
                data.set_version(version.into());
                data.set_system(app::model(vec![row("Kernel", kernel)]));
                data.set_hardware(app::model(hardware));
                data.set_memory_used(used.into());
            });
        });
    }
}

impl Controller for AboutPage {
    fn show(&self, _app: &Rc<App>) {
        AboutPage::refresh();
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const OS_RELEASE: &str = "\
PRETTY_NAME=\"Sengi OS 0.5.0\"
NAME=\"Sengi OS\"
VERSION_ID=\"0.5.0\"
VERSION=\"0.5.0 (trixie)\"
ID=sengios
ID_LIKE=debian
HOME_URL='https://example.org/'
";

    #[test]
    fn the_name_and_version_come_from_os_release() {
        assert_eq!(
            system_name(OS_RELEASE),
            ("Sengi OS".to_string(), "0.5.0 (trixie)".to_string())
        );
        assert_eq!(
            os_release_value(OS_RELEASE, "ID").as_deref(),
            Some("sengios")
        );
        assert_eq!(
            os_release_value(OS_RELEASE, "HOME_URL").as_deref(),
            Some("https://example.org/")
        );
        // ID is not ID_LIKE, and a key that is not there is not there.
        assert_eq!(os_release_value("ID_LIKE=debian\n", "ID"), None);
        assert_eq!(os_release_value(OS_RELEASE, "BUILD_ID"), None);
        // No file at all: still Sengi OS, with no version claimed.
        assert_eq!(system_name(""), ("Sengi OS".to_string(), String::new()));
        assert_eq!(system_name("VERSION_ID=7\n").1, "7");
    }

    #[test]
    fn hostile_os_release_is_cleaned() {
        let odd = "NAME=\"Evil\u{202e}\u{1b}[31m OS\"\nVERSION=\"\"\nVERSION_ID=\n";
        let (name, version) = system_name(odd);
        assert_eq!(name, "Evil[31m OS");
        assert_eq!(version, "");
        let long = format!("NAME={}\n", "x".repeat(5000));
        assert_eq!(system_name(&long).0.chars().count(), 48);
    }

    #[test]
    fn the_processor_is_named_with_its_threads() {
        let cpuinfo = "processor\t: 0\nmodel name\t: Intel(R) Core(TM) i5-7200U CPU @ 2.50GHz\n\n\
                       processor\t: 1\nmodel name\t: Intel(R) Core(TM) i5-7200U CPU @ 2.50GHz\n\n\
                       processor\t: 2\nprocessor\t: 3\n";
        assert_eq!(
            processor(cpuinfo),
            "Intel(R) Core(TM) i5-7200U CPU @ 2.50GHz (4 threads)"
        );
        assert_eq!(processor("processor : 0\nHardware : BCM2835\n"), "BCM2835");
        assert_eq!(processor(""), "Unknown");
        assert_eq!(processor("model name\t:\n"), "Unknown");
    }

    #[test]
    fn memory_installed_and_in_use() {
        let sample = "MemTotal:       16282280 kB\nMemFree:         9000000 kB\n\
                      MemAvailable:   15900000 kB\nSwapTotal: 0 kB\n";
        assert_eq!(memory(sample), ("16 GB".to_string(), "373 MB".to_string()));
        assert_eq!(memory(""), ("Unknown".to_string(), "Unknown".to_string()));
        // Available above the total is nonsense, not a negative number.
        let odd = "MemTotal: 100 kB\nMemAvailable: 200 kB\n";
        assert_eq!(memory(odd).1, "Unknown");
        assert_eq!(meminfo("MemTotal: lots kB\n", "MemTotal"), None);
    }

    #[test]
    fn amounts_for_people() {
        assert_eq!(amount(412 * 1024 * 1024), "412 MB");
        assert_eq!(amount(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
        assert_eq!(amount(238 * 1024 * 1024 * 1024), "238 GB");
        assert_eq!(amount(0), "0 MB");
        assert_eq!(disk_words(100 << 30, 40 << 30), "40 GB free of 100 GB");
        assert_eq!(disk_words(0, 0), "Unknown");
        // More free than there is: held to what there is.
        assert_eq!(disk_words(10 << 30, 99 << 30), "10 GB free of 10 GB");
    }

    #[test]
    fn graphics_adapters_are_named_by_maker_and_driver() {
        assert_eq!(
            adapter("DRIVER=i915\nPCI_CLASS=30000\nPCI_ID=8086:5916\n").as_deref(),
            Some("Intel (i915)")
        );
        assert_eq!(
            adapter("DRIVER=nouveau\nPCI_ID=10DE:134D\n").as_deref(),
            Some("NVIDIA (nouveau)")
        );
        assert_eq!(
            adapter("DRIVER=bochs-drm\nPCI_ID=ffff:0001\n").as_deref(),
            Some("bochs-drm")
        );
        assert_eq!(
            adapter("DRIVER=simple-framebuffer\n").as_deref(),
            Some("simple-framebuffer")
        );
        assert_eq!(adapter("PCI_ID=8086:5916\n"), None);
        assert_eq!(adapter(""), None);

        let tmp = TempDir::new();
        tmp.write("card0/device/uevent", "DRIVER=i915\nPCI_ID=8086:5916\n");
        tmp.write("card0-eDP-1/device/uevent", "DRIVER=wrong\n");
        tmp.write("card1/device/uevent", "DRIVER=nouveau\nPCI_ID=10de:134d\n");
        tmp.write(
            "renderD128/device/uevent",
            "DRIVER=i915\nPCI_ID=8086:5916\n",
        );
        tmp.write("version", "drm 1.1.0");
        assert_eq!(graphics(tmp.path()), "Intel (i915), NVIDIA (nouveau)");
        assert_eq!(graphics(&tmp.path().join("missing")), "Unknown");
    }

    #[test]
    fn the_real_machine_can_be_read_without_trouble() {
        assert!(disk("/").contains("free of"));
        assert_eq!(disk("/no/such/place"), "Unknown");
        assert_ne!(memory(&read(Path::new("/proc/meminfo"))).0, "Unknown");
        assert_eq!(read(Path::new("/no/such/file")), "");
    }
}
