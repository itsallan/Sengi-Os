//! Which drives Files offers: the USB sticks, SD cards and external disks,
//! and what is on them. Read from the kernel's own files, with no D-Bus and
//! no program started:
//!
//! - `/sys/block` for the disks, their partitions, sizes and whether they
//!   are removable or hang off a USB bus;
//! - the udev database in `/run/udev/data` for the file system type and
//!   label of each (`/dev/disk/by-label` is the fallback for the label);
//! - `/proc/self/mountinfo` for where each is mounted.
//!
//! What is never offered: a disk that is not removable and not on USB or an
//! SD reader (the computer's own disks), and any disk with a partition that
//! holds a part of the running system or the Sengi OS live medium, however
//! it is plugged in.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use crate::fsops;

/// Where the facts are read from. The real ones, or a made-up tree in a
/// test.
#[derive(Debug, Clone)]
pub struct Sources {
    pub sys_block: PathBuf,
    pub mountinfo: PathBuf,
    pub udev_data: PathBuf,
    pub by_label: PathBuf,
}

impl Sources {
    pub fn system() -> Sources {
        Sources {
            sys_block: PathBuf::from("/sys/block"),
            mountinfo: PathBuf::from("/proc/self/mountinfo"),
            udev_data: PathBuf::from("/run/udev/data"),
            by_label: PathBuf::from("/dev/disk/by-label"),
        }
    }
}

/// The label the Sengi OS live medium has. The boot script finds the
/// medium by it, and a stick that has it is never offered.
const LIVE_LABEL: &str = "SENGIOS";

/// Kinds of disk that are never a stick or a card: loop and RAM disks,
/// compressed memory, device-mapper and RAID volumes, network block devices,
/// floppies and optical drives.
const NOT_DRIVES: &[&str] = &["loop", "ram", "zram", "dm-", "md", "nbd", "fd", "sr"];

/// What a partition can hold that cannot be opened as files.
const NOT_FILES: &[&str] = &[
    "swap",
    "crypto_LUKS",
    "BitLocker",
    "LVM2_member",
    "linux_raid_member",
    "zfs_member",
    "ddf_raid_member",
    "isw_raid_member",
];

/// Places that mean a disk belongs to the running system.
const SYSTEM_MOUNTS: &[&str] = &[
    "/",
    "/boot",
    "/boot/efi",
    "/usr",
    "/var",
    "/home",
    "/etc",
    "/opt",
    "/srv",
    "/cdrom",
    "/run/live/medium",
    "/run/sengi/medium",
];

const SYSTEM_TREES: &[&str] = &[
    "/boot/",
    "/usr/",
    "/var/",
    "/etc/",
    "/lib/",
    "/run/live/",
    "/run/sengi/",
];

/// One volume that can be opened: a partition, or a whole disk that has a
/// file system and no partitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The kernel's name for it: "sdb1".
    pub name: String,
    /// The disk it is on: "sdb". The same as `name` for a whole disk.
    pub disk: String,
    pub label: String,
    /// The file system, as udev found it: "vfat", "exfat", "ext4". Empty
    /// when only a label is known.
    pub fs: String,
    /// In bytes.
    pub size: u64,
    /// Where it is mounted, if it is.
    pub mount: Option<PathBuf>,
    pub read_only: bool,
}

impl Device {
    pub fn node(&self) -> PathBuf {
        PathBuf::from("/dev").join(&self.name)
    }

    pub fn is_mounted(&self) -> bool {
        self.mount.is_some()
    }

    /// What it is called in the sidebar: its label, or its size.
    pub fn title(&self) -> String {
        if self.label.is_empty() {
            format!("{} drive", fsops::size_text(self.size))
        } else {
            self.label.clone()
        }
    }

    /// The second line: how big it is and whether it is open.
    pub fn detail(&self) -> String {
        let mut text = fsops::size_text(self.size);
        if self.read_only {
            text.push_str(", read-only");
        }
        text.push_str(if self.is_mounted() {
            " \u{b7} Mounted"
        } else {
            " \u{b7} Not mounted"
        });
        text
    }
}

/// One line of `/proc/self/mountinfo`, as far as it matters here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// "8:17": the device it is on, as major and minor numbers.
    pub numbers: String,
    /// What part of the file system is mounted; "/" for the whole of it.
    pub root: String,
    pub point: PathBuf,
    pub read_only: bool,
}

/// The characters mountinfo writes as `\040` (a space) and the like.
fn unescape(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\\' && at + 3 < bytes.len() {
            let digits = &bytes[at + 1..at + 4];
            if digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) {
                let value = digits
                    .iter()
                    .fold(0u32, |sum, digit| sum * 8 + u32::from(digit - b'0'));
                out.push(value as u8);
                at += 4;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    out
}

pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    let mut mounts = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        if fields.len() < 6 {
            continue;
        }
        mounts.push(Mount {
            numbers: fields[2].to_string(),
            root: String::from_utf8_lossy(&unescape(fields[3])).into_owned(),
            point: PathBuf::from(OsString::from_vec(unescape(fields[4]))),
            read_only: fields[5].split(',').any(|option| option == "ro"),
        });
    }
    mounts
}

/// A label as udev and `/dev/disk/by-label` write it, with `\x20` for a
/// space and so on, as the text it stands for.
pub fn decode_label(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\\' && bytes.get(at + 1) == Some(&b'x') && at + 3 < bytes.len() {
            let high = (bytes[at + 2] as char).to_digit(16);
            let low = (bytes[at + 3] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                at += 4;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn is_system_mount(point: &Path) -> bool {
    let Some(text) = point.to_str() else {
        return false;
    };
    SYSTEM_MOUNTS.contains(&text) || SYSTEM_TREES.iter().any(|tree| text.starts_with(tree))
}

fn is_usb_bus(name: &str) -> bool {
    name.strip_prefix("usb")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// Whether a disk is one that gets unplugged: it says so itself, it is an
/// SD card in a reader, or it hangs off a USB bus.
fn is_external(dir: &Path) -> bool {
    if read(&dir.join("removable")).trim() == "1" {
        return true;
    }
    if read(&dir.join("device/type")).trim() == "SD" {
        return true;
    }
    fs::canonicalize(dir).is_ok_and(|real| {
        real.components()
            .any(|part| is_usb_bus(&part.as_os_str().to_string_lossy()))
    })
}

fn numbers_of(dir: &Path) -> Option<String> {
    let text = read(&dir.join("dev"));
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The partitions of the disk in `dir`, by number.
fn partitions(dir: &Path, disk: &str) -> Vec<(String, PathBuf)> {
    let Ok(items) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(u32, String, PathBuf)> = Vec::new();
    for item in items.flatten() {
        let Ok(name) = item.file_name().into_string() else {
            continue;
        };
        if !name.starts_with(disk) {
            continue;
        }
        let path = item.path();
        let number = read(&path.join("partition")).trim().parse::<u32>();
        if let Ok(number) = number {
            found.push((number, name, path));
        }
    }
    found.sort();
    found
        .into_iter()
        .map(|(_, name, path)| (name, path))
        .collect()
}

/// What udev knows of what is on a volume.
struct Probe {
    fs: String,
    label: String,
}

fn probe(src: &Sources, numbers: &str, name: &str) -> Probe {
    let text = read(&src.udev_data.join(format!("b{numbers}")));
    let mut fs_type = String::new();
    let mut plain = String::new();
    let mut encoded = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("E:ID_FS_TYPE=") {
            fs_type = value.to_string();
        } else if let Some(value) = line.strip_prefix("E:ID_FS_LABEL_ENC=") {
            encoded = Some(decode_label(value));
        } else if let Some(value) = line.strip_prefix("E:ID_FS_LABEL=") {
            plain = value.to_string();
        }
    }
    let mut label = encoded.unwrap_or(plain);
    if label.is_empty() {
        label = label_from_links(src, name).unwrap_or_default();
    }
    Probe { fs: fs_type, label }
}

/// The label that `/dev/disk/by-label` has for the volume called `name`.
fn label_from_links(src: &Sources, name: &str) -> Option<String> {
    for link in fs::read_dir(&src.by_label).ok()?.flatten() {
        let target = fs::read_link(link.path()).ok()?;
        if target.file_name().is_some_and(|last| last == name) {
            return Some(decode_label(&link.file_name().to_string_lossy()));
        }
    }
    None
}

/// Whether the volume in `dir` is a part of the running system or the
/// medium Sengi OS was started from.
fn is_system(src: &Sources, mounts: &[Mount], name: &str, dir: &Path) -> bool {
    let Some(numbers) = numbers_of(dir) else {
        return false;
    };
    mounts
        .iter()
        .any(|mount| mount.numbers == numbers && is_system_mount(&mount.point))
        || probe(src, &numbers, name).label == LIVE_LABEL
}

fn describe(src: &Sources, mounts: &[Mount], disk: &str, name: &str, dir: &Path) -> Option<Device> {
    let numbers = numbers_of(dir)?;
    let found = probe(src, &numbers, name);
    // Nothing that can be recognised: an empty slot, a table with no
    // partitions yet, an extended partition.
    if found.fs.is_empty() && found.label.is_empty() {
        return None;
    }
    if NOT_FILES.contains(&found.fs.as_str()) {
        return None;
    }
    let sectors: u64 = read(&dir.join("size")).trim().parse().unwrap_or(0);
    let mount = mounts.iter().find(|mount| {
        mount.numbers == numbers && mount.root == "/" && !is_system_mount(&mount.point)
    });
    Some(Device {
        name: name.to_string(),
        disk: disk.to_string(),
        label: found.label,
        fs: found.fs,
        size: sectors.saturating_mul(512),
        mount: mount.map(|mount| mount.point.clone()),
        read_only: mount.is_some_and(|mount| mount.read_only),
    })
}

/// Every volume on a removable or external disk that can be opened as files,
/// by disk and partition number.
pub fn discover(src: &Sources) -> Vec<Device> {
    let mounts = parse_mountinfo(&read(&src.mountinfo));
    let Ok(listing) = fs::read_dir(&src.sys_block) else {
        return Vec::new();
    };
    let mut disks: Vec<String> = listing
        .flatten()
        .filter_map(|item| item.file_name().into_string().ok())
        .collect();
    disks.sort();
    let mut found = Vec::new();
    for disk in disks {
        if NOT_DRIVES.iter().any(|kind| disk.starts_with(kind)) {
            continue;
        }
        let dir = src.sys_block.join(&disk);
        if !is_external(&dir) {
            continue;
        }
        let parts = partitions(&dir, &disk);
        let whole = [(disk.clone(), dir.clone())];
        // One partition of the system, or of the live medium, and the whole
        // disk stays out of the list.
        if whole
            .iter()
            .chain(parts.iter())
            .any(|(name, path)| is_system(src, &mounts, name, path))
        {
            continue;
        }
        let offered: &[(String, PathBuf)] = if parts.is_empty() { &whole } else { &parts };
        for (name, path) in offered {
            if let Some(device) = describe(src, &mounts, &disk, name, path) {
                found.push(device);
            }
        }
    }
    found
}

/// The drives that are mounted: the places their Trash may be found.
pub fn mounted_in(devices: &[Device], media: Option<&Path>) -> Vec<PathBuf> {
    let Some(media) = media else {
        return Vec::new();
    };
    devices
        .iter()
        .filter_map(|device| device.mount.clone())
        .filter(|point| point.starts_with(media))
        .collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::fsops::tests::Temp;

    /// A made-up /sys, /run/udev and /proc under a temporary folder.
    pub struct Fake {
        pub t: Temp,
    }

    impl Fake {
        pub fn new() -> Fake {
            let t = Temp::new();
            for dir in ["sys/block", "run/udev/data", "dev/disk/by-label"] {
                fs::create_dir_all(t.path().join(dir)).unwrap();
            }
            fs::write(t.path().join("mountinfo"), "").unwrap();
            Fake { t }
        }

        pub fn sources(&self) -> Sources {
            Sources {
                sys_block: self.t.path().join("sys/block"),
                mountinfo: self.t.path().join("mountinfo"),
                udev_data: self.t.path().join("run/udev/data"),
                by_label: self.t.path().join("dev/disk/by-label"),
            }
        }

        /// A disk, as /sys/block has it: a link into the device tree, where
        /// a USB disk sits under a bus called usbN.
        pub fn disk(&self, name: &str, usb: bool, removable: bool, numbers: &str, sectors: u64) {
            let bus = if usb {
                "pci0000:00/0000:00:14.0/usb1/1-2/host6/target6:0:0/6:0:0:0"
            } else {
                "pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0"
            };
            let real = self
                .t
                .path()
                .join("sys/devices")
                .join(bus)
                .join("block")
                .join(name);
            fs::create_dir_all(&real).unwrap();
            fs::write(
                real.join("removable"),
                if removable { "1\n" } else { "0\n" },
            )
            .unwrap();
            fs::write(real.join("dev"), format!("{numbers}\n")).unwrap();
            fs::write(real.join("size"), format!("{sectors}\n")).unwrap();
            std::os::unix::fs::symlink(&real, self.t.path().join("sys/block").join(name)).unwrap();
        }

        pub fn partition(&self, disk: &str, name: &str, number: u32, numbers: &str, sectors: u64) {
            let dir = self.t.path().join("sys/block").join(disk).join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("partition"), format!("{number}\n")).unwrap();
            fs::write(dir.join("dev"), format!("{numbers}\n")).unwrap();
            fs::write(dir.join("size"), format!("{sectors}\n")).unwrap();
        }

        /// What udev's database says of the volume with these numbers.
        pub fn udev(&self, numbers: &str, props: &[(&str, &str)]) {
            let text: String = props
                .iter()
                .map(|(key, value)| format!("E:{key}={value}\n"))
                .collect();
            fs::write(
                self.t
                    .path()
                    .join("run/udev/data")
                    .join(format!("b{numbers}")),
                text,
            )
            .unwrap();
        }

        pub fn mountinfo(&self, lines: &[&str]) {
            fs::write(self.t.path().join("mountinfo"), lines.join("\n") + "\n").unwrap();
        }
    }

    fn names(devices: &[Device]) -> Vec<&str> {
        devices.iter().map(|d| d.name.as_str()).collect()
    }

    /// A laptop: an internal disk with the system on it, the live stick it
    /// booted from, and a USB stick with a label that has a space in it.
    fn laptop() -> Fake {
        let fake = Fake::new();
        // The computer's own SATA disk: not removable, not USB.
        fake.disk("sda", false, false, "8:0", 500_000_000);
        fake.partition("sda", "sda1", 1, "8:1", 1_000_000);
        fake.partition("sda", "sda2", 2, "8:2", 499_000_000);
        fake.udev("8:1", &[("ID_FS_TYPE", "vfat"), ("ID_FS_LABEL", "ESP")]);
        fake.udev("8:2", &[("ID_FS_TYPE", "ext4")]);
        // The live stick: USB and removable, the ISO on the whole disk.
        fake.disk("sdb", true, true, "8:16", 4_000_000);
        fake.udev(
            "8:16",
            &[("ID_FS_TYPE", "iso9660"), ("ID_FS_LABEL", "SENGIOS")],
        );
        // A stick with a label that has a space and a colon in it.
        fake.disk("sdc", true, true, "8:32", 31_250_000);
        fake.partition("sdc", "sdc1", 1, "8:33", 31_000_000);
        fake.udev(
            "8:33",
            &[
                ("ID_FS_TYPE", "exfat"),
                ("ID_FS_LABEL", "My_Stick:1"),
                ("ID_FS_LABEL_ENC", "My\\x20Stick:1"),
            ],
        );
        fake.mountinfo(&[
            "25 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw",
            "26 25 8:1 / /boot/efi rw,relatime shared:2 - vfat /dev/sda1 rw",
            "27 25 8:16 / /run/sengi/medium ro,relatime - iso9660 /dev/sdb ro",
            "28 25 0:30 / /run ro - tmpfs tmpfs rw",
        ]);
        fake
    }

    #[test]
    fn the_internal_disk_and_the_live_stick_are_never_offered() {
        let fake = laptop();
        let found = discover(&fake.sources());
        assert_eq!(names(&found), ["sdc1"]);
    }

    #[test]
    fn a_stick_is_listed_with_its_label_size_and_state() {
        let fake = laptop();
        let found = discover(&fake.sources());
        let stick = &found[0];
        assert_eq!(stick.label, "My Stick:1");
        assert_eq!(stick.title(), "My Stick:1");
        assert_eq!(stick.fs, "exfat");
        assert_eq!(stick.disk, "sdc");
        assert_eq!(stick.size, 31_000_000 * 512);
        assert!(!stick.is_mounted());
        assert_eq!(stick.detail(), "15.9 GB \u{b7} Not mounted");
        assert_eq!(stick.node(), Path::new("/dev/sdc1"));
    }

    #[test]
    fn a_mounted_stick_says_where_with_spaces_decoded() {
        let fake = laptop();
        fake.mountinfo(&[
            "25 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw",
            "27 25 8:16 / /run/sengi/medium ro,relatime - iso9660 /dev/sdb ro",
            "40 25 8:33 / /run/media/live/My\\040Stick:1 rw,nosuid,nodev - exfat /dev/sdc1 rw",
        ]);
        let found = discover(&fake.sources());
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].mount.as_deref(),
            Some(Path::new("/run/media/live/My Stick:1"))
        );
        assert!(found[0].detail().ends_with("Mounted"));
        assert!(!found[0].read_only);
        fake.mountinfo(&["40 25 8:33 / /run/media/live/X ro,nosuid - exfat /dev/sdc1 ro"]);
        let found = discover(&fake.sources());
        assert!(found[0].read_only);
        assert!(found[0].detail().contains("read-only"));
    }

    #[test]
    fn a_stick_with_a_partition_of_the_system_on_it_is_left_out() {
        let fake = laptop();
        // Linux installed on a USB disk: its root is a partition of sdd.
        fake.disk("sdd", true, true, "8:48", 100_000_000);
        fake.partition("sdd", "sdd1", 1, "8:49", 1_000_000);
        fake.partition("sdd", "sdd2", 2, "8:50", 99_000_000);
        fake.udev("8:49", &[("ID_FS_TYPE", "vfat"), ("ID_FS_LABEL", "EFI")]);
        fake.udev("8:50", &[("ID_FS_TYPE", "ext4")]);
        fake.mountinfo(&["25 1 8:50 / / rw - ext4 /dev/sdd2 rw"]);
        let found = discover(&fake.sources());
        assert!(!names(&found).contains(&"sdd1"), "{:?}", names(&found));
        assert!(!names(&found).contains(&"sdd2"), "{:?}", names(&found));
        assert_eq!(names(&found), ["sdc1"]);
    }

    #[test]
    fn the_live_label_alone_keeps_a_stick_out_even_when_it_is_not_mounted() {
        let fake = Fake::new();
        fake.disk("sdb", true, true, "8:16", 4_000_000);
        fake.partition("sdb", "sdb1", 1, "8:17", 3_000_000);
        fake.udev(
            "8:17",
            &[("ID_FS_TYPE", "iso9660"), ("ID_FS_LABEL", "SENGIOS")],
        );
        assert!(discover(&fake.sources()).is_empty());
    }

    #[test]
    fn a_disk_without_partitions_is_offered_whole() {
        let fake = Fake::new();
        fake.disk("sdb", true, true, "8:16", 2_000_000);
        fake.udev("8:16", &[("ID_FS_TYPE", "vfat"), ("ID_FS_LABEL", "CAMERA")]);
        let found = discover(&fake.sources());
        assert_eq!(names(&found), ["sdb"]);
        assert_eq!(found[0].disk, "sdb");
        assert_eq!(found[0].title(), "CAMERA");
    }

    #[test]
    fn what_cannot_be_opened_as_files_is_not_listed() {
        let fake = Fake::new();
        fake.disk("sdb", true, true, "8:16", 10_000_000);
        for (n, kind) in ["swap", "crypto_LUKS", "ext4"].iter().enumerate() {
            let number = n as u32 + 1;
            let numbers = format!("8:{}", 16 + number);
            fake.partition("sdb", &format!("sdb{number}"), number, &numbers, 1_000_000);
            fake.udev(&numbers, &[("ID_FS_TYPE", kind)]);
        }
        // An extended partition: nothing udev recognises.
        fake.partition("sdb", "sdb4", 4, "8:20", 2);
        let found = discover(&fake.sources());
        assert_eq!(names(&found), ["sdb3"]);
        assert_eq!(found[0].title(), "512.0 MB drive");
    }

    #[test]
    fn disks_that_are_not_sticks_are_ignored() {
        let fake = Fake::new();
        // Not removable and not on USB: a SATA disk, even with files on it.
        fake.disk("sda", false, false, "8:0", 1_000_000);
        fake.udev("8:0", &[("ID_FS_TYPE", "ext4")]);
        // A loop device and an optical drive claim to be removable.
        fake.disk("loop0", false, true, "7:0", 1_000);
        fake.udev("7:0", &[("ID_FS_TYPE", "squashfs")]);
        fake.disk("sr0", false, true, "11:0", 1_000);
        fake.udev("11:0", &[("ID_FS_TYPE", "iso9660")]);
        assert!(discover(&fake.sources()).is_empty());
    }

    #[test]
    fn a_usb_disk_that_says_it_is_fixed_is_still_external() {
        let fake = Fake::new();
        // Many USB hard disks report removable=0.
        fake.disk("sdb", true, false, "8:16", 2_000_000);
        fake.udev("8:16", &[("ID_FS_TYPE", "ntfs"), ("ID_FS_LABEL", "Backup")]);
        assert_eq!(names(&discover(&fake.sources())), ["sdb"]);
    }

    #[test]
    fn an_sd_card_in_the_built_in_reader_is_offered() {
        let fake = Fake::new();
        fake.disk("mmcblk0", false, false, "179:0", 2_000_000);
        let real = fs::canonicalize(fake.t.path().join("sys/block/mmcblk0")).unwrap();
        fs::create_dir_all(real.join("device")).unwrap();
        fs::write(real.join("device/type"), "SD\n").unwrap();
        fake.partition("mmcblk0", "mmcblk0p1", 1, "179:1", 1_900_000);
        fake.udev(
            "179:1",
            &[("ID_FS_TYPE", "vfat"), ("ID_FS_LABEL", "PHOTOS")],
        );
        assert_eq!(names(&discover(&fake.sources())), ["mmcblk0p1"]);
    }

    #[test]
    fn the_label_comes_from_by_label_when_udev_has_none() {
        let fake = Fake::new();
        fake.disk("sdb", true, true, "8:16", 2_000_000);
        fake.partition("sdb", "sdb1", 1, "8:17", 1_000_000);
        fake.udev("8:17", &[("ID_FS_TYPE", "vfat")]);
        std::os::unix::fs::symlink(
            "../../sdb1",
            fake.t.path().join("dev/disk/by-label/Holiday\\x20Photos"),
        )
        .unwrap();
        let found = discover(&fake.sources());
        assert_eq!(found[0].label, "Holiday Photos");
    }

    #[test]
    fn mountinfo_escapes_and_bind_mounts_are_read() {
        let mounts = parse_mountinfo(
            "36 35 98:0 /mnt1 /mnt/a\\040b rw,noatime master:1 - ext3 /dev/root rw\n\
             37 35 8:1 / /tab\\011bed ro - vfat /dev/sda1 ro\n\
             garbage\n",
        );
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].numbers, "98:0");
        assert_eq!(mounts[0].root, "/mnt1");
        assert_eq!(mounts[0].point, Path::new("/mnt/a b"));
        assert!(!mounts[0].read_only);
        assert_eq!(mounts[1].point, Path::new("/tab\tbed"));
        assert!(mounts[1].read_only);
    }

    #[test]
    fn labels_are_decoded() {
        assert_eq!(decode_label("My\\x20Stick"), "My Stick");
        assert_eq!(decode_label("caf\\xc3\\xa9"), "caf\u{e9}");
        assert_eq!(decode_label("100%\\x"), "100%\\x");
        assert_eq!(decode_label("plain"), "plain");
    }

    #[test]
    fn a_mount_that_is_only_a_folder_of_a_volume_is_not_where_it_is_mounted() {
        let fake = laptop();
        fake.mountinfo(&["40 25 8:33 /photos /mnt/photos rw - exfat /dev/sdc1 rw"]);
        let found = discover(&fake.sources());
        assert!(!found[0].is_mounted());
    }

    #[test]
    fn only_drives_under_the_media_folder_have_a_trash_to_look_in() {
        let mut stick = Device {
            name: "sdc1".into(),
            disk: "sdc".into(),
            label: "S".into(),
            fs: "vfat".into(),
            size: 1,
            mount: Some(PathBuf::from("/run/media/live/S")),
            read_only: false,
        };
        let media = Path::new("/run/media/live");
        assert_eq!(
            mounted_in(std::slice::from_ref(&stick), Some(media)),
            [PathBuf::from("/run/media/live/S")]
        );
        stick.mount = Some(PathBuf::from("/mnt/x"));
        assert!(mounted_in(std::slice::from_ref(&stick), Some(media)).is_empty());
        assert!(mounted_in(&[], None).is_empty());
    }
}
