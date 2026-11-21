//! The plan: everything an installation is told, in one small file in
//! the format of the settings file.
//!
//! ```text
//! [plan]
//! version = 1
//!
//! [disk]
//! device = "/dev/sda"
//! mode = "erase"
//! encryption = "none"
//! size = 256060514304
//! model = "SanDisk SD8SN8U"
//! serial = "162345400999"
//! wwn = "0x5001b448b1234567"
//!
//! [user]
//! name = "allan"
//! full-name = "Allan"
//!
//! [system]
//! host-name = "zenbook"
//! time-zone = "Europe/Paris"
//! keyboard = "fr(azerty)"
//! ```
//!
//! The password is not in it. It comes on standard input, ahead of the
//! plan, and is never written anywhere.
//!
//! `mode` and `encryption` have one value each in this version. They are
//! there so that a plan says what it means, and so that a later version
//! that can install beside another system, or encrypt, adds values and
//! not a new format. A plan this version does not understand in full is
//! refused whole: a key that is not known might be the one that says not
//! to erase.
//!
//! `size`, `model`, `serial` and `wwn` are what the disk was when it was
//! chosen: the last two as far as it has them. If the device of that name
//! is no longer that disk, because a stick was plugged in and the names
//! moved, nothing is touched. Two disks of one model and size differ in
//! their serial numbers.
//!
//! `[source]` is for testing away from the live image: where the system
//! to copy is, where its kernel is, and whether a loop device may be the
//! target. The window never writes it.

use std::path::PathBuf;

use sengi_config::settings::{zone_is_plain, Layout};
use sengi_config::{Document, Value};

use crate::names;

pub const VERSION: i64 = 1;

/// Where the live image keeps the system it runs from, read-only and as
/// it was built, and the kernel it was started with.
pub const LIVE_ROOT: &str = "/run/sengi/rootfs";
pub const LIVE_KERNEL: &str = "/run/sengi/medium/live/vmlinuz";

/// Every key a plan may have.
const KEYS: [(&str, &str); 15] = [
    ("plan", "version"),
    ("disk", "device"),
    ("disk", "mode"),
    ("disk", "encryption"),
    ("disk", "size"),
    ("disk", "model"),
    ("disk", "serial"),
    ("disk", "wwn"),
    ("user", "name"),
    ("user", "full-name"),
    ("system", "host-name"),
    ("system", "time-zone"),
    ("system", "keyboard"),
    ("source", "root"),
    ("source", "kernel"),
];
/// And one more, kept apart because it is not text.
const ALLOW_LOOP: (&str, &str) = ("source", "allow-loop");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The whole disk: `/dev/sda`, `/dev/nvme0n1`.
    pub device: String,
    /// Its size in bytes and its model, as its chooser saw them.
    pub size: u64,
    pub model: String,
    /// Its serial number and world-wide name, empty where it has none.
    pub serial: String,
    pub wwn: String,
    pub user: String,
    pub full_name: String,
    pub host_name: String,
    pub time_zone: String,
    pub keyboard: Layout,
    pub source_root: PathBuf,
    pub kernel: PathBuf,
    pub allow_loop: bool,
}

/// Whether `device` is written as the node of a whole disk is: `/dev/`
/// and one plain name. What it really is gets looked at later.
fn device_is_plain(device: &str) -> bool {
    device.strip_prefix("/dev/").is_some_and(|name| {
        !name.is_empty() && name.len() <= 32 && name.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

impl Plan {
    pub fn parse(text: &str) -> Result<Plan, String> {
        let (document, warnings) = Document::parse(text);
        if let Some(warning) = warnings.first() {
            return Err(format!("the plan cannot be read: {warning}"));
        }
        for (section, key) in document.keys() {
            if !KEYS.contains(&(section, key)) && (section, key) != ALLOW_LOOP {
                return Err(format!(
                    "the plan has a key this version does not know: {section}.{key}"
                ));
            }
        }
        let text_of = |section: &str, key: &str| match document.get(section, key) {
            Some(Value::Text(text)) => Ok(Some(text.clone())),
            Some(_) => Err(format!("{section}.{key} must be text")),
            None => Ok(None),
        };
        let needed = |section: &str, key: &str| {
            text_of(section, key)?.ok_or(format!("the plan has no {section}.{key}"))
        };

        match document.get("plan", "version") {
            Some(Value::Number(VERSION)) => {}
            Some(Value::Number(other)) => {
                return Err(format!(
                    "the plan is of version {other}; this program reads version {VERSION}"
                ))
            }
            _ => return Err("the plan does not say which version it is".to_string()),
        }

        let device = needed("disk", "device")?;
        if !device_is_plain(&device) {
            return Err(format!("{device} is not the name of a disk"));
        }
        let mode = needed("disk", "mode")?;
        if mode != "erase" {
            return Err(format!(
                "disk.mode is \"{mode}\"; this version only erases a whole disk (\"erase\")"
            ));
        }
        let encryption = text_of("disk", "encryption")?.unwrap_or_else(|| "none".to_string());
        if encryption != "none" {
            return Err(format!(
                "disk.encryption is \"{encryption}\"; this version does not encrypt (\"none\")"
            ));
        }
        let size = match document.get("disk", "size") {
            Some(Value::Number(size)) if *size > 0 => *size as u64,
            _ => return Err("the plan does not say how large the disk is".to_string()),
        };
        let model = text_of("disk", "model")?.unwrap_or_default();
        let serial = text_of("disk", "serial")?.unwrap_or_default();
        let wwn = text_of("disk", "wwn")?.unwrap_or_default();

        let user = needed("user", "name")?;
        names::user_name(&user).map_err(|why| format!("user.name: {why}"))?;
        let full_name = text_of("user", "full-name")?.unwrap_or_default();
        names::full_name(&full_name).map_err(|why| format!("user.full-name: {why}"))?;

        let host_name = needed("system", "host-name")?;
        names::host_name(&host_name).map_err(|why| format!("system.host-name: {why}"))?;
        let time_zone = needed("system", "time-zone")?;
        if !zone_is_plain(&time_zone) {
            return Err(format!("{time_zone} is not the name of a time zone"));
        }
        let keyboard = needed("system", "keyboard")?;
        let keyboard = Layout::parse(&keyboard)
            .ok_or(format!("{keyboard} is not the name of a keyboard layout"))?;

        let absolute = |key: &str, default: &str| -> Result<PathBuf, String> {
            let path =
                PathBuf::from(text_of("source", key)?.unwrap_or_else(|| default.to_string()));
            if path.is_absolute() {
                Ok(path)
            } else {
                Err(format!("source.{key} must be a full path"))
            }
        };
        let allow_loop = match document.get(ALLOW_LOOP.0, ALLOW_LOOP.1) {
            None => false,
            Some(Value::Flag(flag)) => *flag,
            Some(_) => return Err("source.allow-loop must be true or false".to_string()),
        };

        Ok(Plan {
            device,
            size,
            model,
            serial,
            wwn,
            user,
            full_name,
            host_name,
            time_zone,
            keyboard,
            source_root: absolute("root", LIVE_ROOT)?,
            kernel: absolute("kernel", LIVE_KERNEL)?,
            allow_loop,
        })
    }

    /// The plan as a file. `[source]` is written only where it differs
    /// from the live image's.
    pub fn render(&self) -> String {
        let text = |text: &str| Value::text(text).render();
        let mut out = format!(
            "[plan]\nversion = {VERSION}\n\n\
             [disk]\ndevice = {}\nmode = \"erase\"\nencryption = \"none\"\nsize = {}\nmodel = {}\nserial = {}\nwwn = {}\n\n\
             [user]\nname = {}\nfull-name = {}\n\n\
             [system]\nhost-name = {}\ntime-zone = {}\nkeyboard = {}\n",
            text(&self.device),
            self.size,
            text(&self.model),
            text(&self.serial),
            text(&self.wwn),
            text(&self.user),
            text(&self.full_name),
            text(&self.host_name),
            text(&self.time_zone),
            text(&self.keyboard.render()),
        );
        let root = self.source_root.to_string_lossy();
        let kernel = self.kernel.to_string_lossy();
        if root != LIVE_ROOT || kernel != LIVE_KERNEL || self.allow_loop {
            out.push_str(&format!(
                "\n[source]\nroot = {}\nkernel = {}\nallow-loop = {}\n",
                text(&root),
                text(&kernel),
                self.allow_loop
            ));
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const PLAN: &str = "\
[plan]
version = 1

[disk]
device = \"/dev/sda\"
mode = \"erase\"
size = 256060514304
model = \"SanDisk SD8SN8U\"

[user]
name = \"allan\"
full-name = \"Allan Smith\"

[system]
host-name = \"zenbook\"
time-zone = \"Europe/Paris\"
keyboard = \"fr(azerty)\"
";

    fn without(line: &str) -> String {
        let kept: Vec<&str> = PLAN.lines().filter(|l| !l.starts_with(line)).collect();
        kept.join("\n")
    }

    fn with(old: &str, new: &str) -> String {
        assert!(PLAN.contains(old));
        PLAN.replace(old, new)
    }

    #[test]
    fn a_plan_is_read() {
        let plan = Plan::parse(PLAN).unwrap();
        assert_eq!(plan.device, "/dev/sda");
        assert_eq!(plan.size, 256_060_514_304);
        assert_eq!(plan.model, "SanDisk SD8SN8U");
        assert_eq!(plan.user, "allan");
        assert_eq!(plan.full_name, "Allan Smith");
        assert_eq!(plan.host_name, "zenbook");
        assert_eq!(plan.time_zone, "Europe/Paris");
        assert_eq!(plan.keyboard.layout, "fr");
        assert_eq!(plan.keyboard.variant, "azerty");
        assert_eq!(plan.source_root, PathBuf::from(LIVE_ROOT));
        assert_eq!(plan.kernel, PathBuf::from(LIVE_KERNEL));
        assert!(!plan.allow_loop);
    }

    #[test]
    fn a_plan_written_out_reads_back_the_same() {
        let plan = Plan::parse(PLAN).unwrap();
        assert_eq!(Plan::parse(&plan.render()), Ok(plan.clone()));
        assert!(!plan.render().contains("[source]"));
        let mut elsewhere = plan;
        elsewhere.source_root = PathBuf::from("/mnt/system");
        elsewhere.allow_loop = true;
        elsewhere.full_name = "Quote \" and \\ slash".to_string();
        assert_eq!(Plan::parse(&elsewhere.render()), Ok(elsewhere));
    }

    #[test]
    fn what_is_missing_is_said() {
        for (line, word) in [
            ("version", "version"),
            ("device", "disk.device"),
            ("mode", "disk.mode"),
            ("size", "how large"),
            ("name =", "user.name"),
            ("host-name", "system.host-name"),
            ("time-zone", "system.time-zone"),
            ("keyboard", "system.keyboard"),
        ] {
            let problem = Plan::parse(&without(line)).unwrap_err();
            assert!(problem.contains(word), "{line}: {problem}");
        }
        // These two may be left out.
        assert!(Plan::parse(&without("model")).is_ok());
        assert!(Plan::parse(&without("full-name")).is_ok());
    }

    #[test]
    fn a_plan_that_is_not_understood_in_full_is_refused() {
        let extra = with("[user]", "[disk]\nkeep-windows = true\n\n[user]");
        assert!(Plan::parse(&extra)
            .unwrap_err()
            .contains("disk.keep-windows"));
        assert!(Plan::parse(&with("version = 1", "version = 2"))
            .unwrap_err()
            .contains("version 2"));
        assert!(Plan::parse(&with("\"erase\"", "\"alongside\""))
            .unwrap_err()
            .contains("only erases"));
        let encrypted = with(
            "mode = \"erase\"",
            "mode = \"erase\"\nencryption = \"luks\"",
        );
        assert!(Plan::parse(&encrypted)
            .unwrap_err()
            .contains("does not encrypt"));
        assert!(Plan::parse(&with("size = 256060514304", "size = lots")).is_err());
        assert!(Plan::parse("").is_err());
        assert!(Plan::parse("\0\0\0").is_err());
    }

    #[test]
    fn only_a_whole_disk_by_its_plain_name_is_a_device() {
        for device in [
            "sda",
            "/dev/",
            "/dev/../etc/passwd",
            "/dev/disk/by-id/x",
            "/dev/sda;reboot",
            "/dev/sda 1",
            "/tmp/disk.img",
            "",
        ] {
            let plan = with("/dev/sda", device);
            assert!(Plan::parse(&plan).is_err(), "{device:?}");
        }
        assert!(Plan::parse(&with("/dev/sda", "/dev/nvme0n1")).is_ok());
    }

    #[test]
    fn names_in_a_plan_are_checked() {
        assert!(Plan::parse(&with("\"allan\"", "\"root\"")).is_err());
        assert!(Plan::parse(&with("\"allan\"", "\"Allan\"")).is_err());
        assert!(Plan::parse(&with("Allan Smith", "Smith, Allan")).is_err());
        assert!(Plan::parse(&with("zenbook", "Zen Book")).is_err());
        assert!(Plan::parse(&with("Europe/Paris", "../../etc/shadow")).is_err());
        assert!(Plan::parse(&with("fr(azerty)", "fr(azerty")).is_err());
        assert!(Plan::parse(&with("fr(azerty)", "fr;rm")).is_err());
    }

    #[test]
    fn the_source_is_the_live_images_unless_a_test_says_otherwise() {
        let plan = format!("{PLAN}\n[source]\nroot = \"/mnt/root\"\nkernel = \"/mnt/vmlinuz\"\nallow-loop = true\n");
        let plan = Plan::parse(&plan).unwrap();
        assert_eq!(plan.source_root, PathBuf::from("/mnt/root"));
        assert_eq!(plan.kernel, PathBuf::from("/mnt/vmlinuz"));
        assert!(plan.allow_loop);
        assert!(Plan::parse(&format!("{PLAN}\n[source]\nroot = \"relative\"\n")).is_err());
        assert!(Plan::parse(&format!("{PLAN}\n[source]\nallow-loop = \"yes\"\n")).is_err());
    }
}
