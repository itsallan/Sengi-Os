//! The disks of this machine, what is on them, and which of them the
//! installer will not touch.
//!
//! Everything is read from `/sys/block`, the mount table and the swap
//! table; what the partitions hold comes from `lsblk`, which asks udev
//! and so needs no root. The window shows this list, and the program
//! that installs reads it again for itself before it erases anything.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

/// Where the live image mounts the medium it was started from.
pub const MEDIUM: &str = "/run/sengi/medium";

/// The smallest disk Sengi OS is installed on. The system is about a
/// gigabyte and a half; this leaves room to use it.
pub const MIN_BYTES: u64 = 8_000_000_000;

/// Kinds of block device that are never a disk to install on, by the
/// start of their kernel names.
const NOT_DISKS: [&str; 8] = ["ram", "zram", "sr", "fd", "dm-", "md", "nbd", "mtd"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    /// The number in the table, and where it starts, in sectors of 512 bytes.
    pub number: u32,
    pub start: u64,
    pub size: u64,
    /// The partition type and the partition identifier of a GPT, as udev
    /// gives them, in small letters. Empty where there are none.
    pub kind: String,
    pub uuid: String,
    /// The kind of file system, as blkid names it, or empty.
    pub fstype: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// The kernel's name, `sda`, and the node, `/dev/sda`.
    pub name: String,
    pub path: String,
    pub size: u64,
    /// As the device reports it. Not cleaned: a stranger chose it.
    pub model: String,
    /// Its serial number and its world-wide name, where it has them.
    /// With the model and the size, these are what tells this disk from
    /// another that might come to have its name.
    pub serial: String,
    pub wwn: String,
    /// How it is attached, as lsblk names it: `sata`, `nvme`, `usb`.
    pub tran: String,
    pub read_only: bool,
    /// The kind of partition table, as lsblk names it: `gpt`, `dos`, or
    /// nothing. And the identifier the table has, in small letters.
    pub pttype: String,
    pub ptuuid: String,
    pub parts: Vec<Part>,
    /// A file system straight on the disk, with no partitions.
    pub fstype: String,
    pub label: String,
    /// Where it, or a partition of it, is mounted.
    pub mounted: Vec<String>,
    /// It or a partition of it is swap space in use.
    pub swap: bool,
    /// Something is built on it: an encrypted volume, a RAID, LVM.
    pub held: bool,
    /// The live system was started from it.
    pub medium: bool,
}

/// Why a disk is not offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Medium,
    Mounted(String),
    Swap,
    Held,
    ReadOnly,
    TooSmall,
}

impl Refusal {
    /// A few words for the list of disks.
    pub fn words(&self) -> String {
        match self {
            Refusal::Medium => "Sengi OS is running from it".to_string(),
            Refusal::Mounted(place) => format!("In use, mounted at {place}"),
            Refusal::Swap => "In use as swap space".to_string(),
            Refusal::Held => "In use by another volume".to_string(),
            Refusal::ReadOnly => "Cannot be written to".to_string(),
            Refusal::TooSmall => format!("Too small, {} needed", size_words(MIN_BYTES)),
        }
    }
}

/// Why `disk` must not be installed on, if there is a reason. The order
/// is that of how much it matters to say.
pub fn refusal(disk: &Disk) -> Option<Refusal> {
    if disk.medium {
        Some(Refusal::Medium)
    } else if let Some(place) = disk.mounted.first() {
        Some(Refusal::Mounted(place.clone()))
    } else if disk.swap {
        Some(Refusal::Swap)
    } else if disk.held {
        Some(Refusal::Held)
    } else if disk.read_only {
        Some(Refusal::ReadOnly)
    } else if disk.size < MIN_BYTES {
        Some(Refusal::TooSmall)
    } else {
        None
    }
}

/// The node of partition `number` of the disk at `disk`: `/dev/sda3`,
/// and `/dev/nvme0n1p3` where the disk's name ends in a digit.
pub fn partition_path(disk: &str, number: u32) -> String {
    if disk.ends_with(|c: char| c.is_ascii_digit()) {
        format!("{disk}p{number}")
    } else {
        format!("{disk}{number}")
    }
}

/// A size as disks are sold: decimal units, three figures at most.
pub fn size_words(bytes: u64) -> String {
    const UNITS: [(u64, &str); 4] = [
        (1_000_000_000_000, "TB"),
        (1_000_000_000, "GB"),
        (1_000_000, "MB"),
        (1_000, "kB"),
    ];
    for (unit, name) in UNITS {
        if bytes >= unit {
            let tenths = (bytes as u128 * 10 / unit as u128) as u64;
            return if tenths >= 100 || tenths.is_multiple_of(10) {
                format!("{} {name}", tenths / 10)
            } else {
                format!("{}.{} {name}", tenths / 10, tenths % 10)
            };
        }
    }
    format!("{bytes} bytes")
}

/// What a file system is called for people.
fn kind_words(fstype: &str) -> &str {
    match fstype {
        "ntfs" => "Windows (NTFS)",
        "vfat" => "FAT",
        "exfat" => "exFAT",
        "apfs" => "macOS (APFS)",
        "hfsplus" => "macOS (HFS+)",
        "swap" => "swap",
        "crypto_LUKS" => "encrypted",
        "BitLocker" => "Windows (BitLocker)",
        "LVM2_member" => "LVM",
        "iso9660" => "a disc image",
        "ext2" | "ext3" | "ext4" | "btrfs" | "xfs" | "f2fs" => "Linux",
        "" => "unknown",
        other => other,
    }
}

/// What is on the disk now, in a line: this is what will be erased.
pub fn contents_words(disk: &Disk) -> String {
    if disk.parts.is_empty() {
        return if disk.fstype.is_empty() {
            "Empty".to_string()
        } else if disk.label.is_empty() {
            format!("One volume, {}", kind_words(&disk.fstype))
        } else {
            format!(
                "One volume, {} \u{201c}{}\u{201d}",
                kind_words(&disk.fstype),
                disk.label
            )
        };
    }
    // The large ones say what the disk is for; a boot partition of a
    // few megabytes does not.
    let mut parts: Vec<&Part> = disk.parts.iter().collect();
    parts.sort_by_key(|part| std::cmp::Reverse(part.size));
    let named: Vec<String> = parts
        .iter()
        .take(3)
        .map(|part| {
            if part.label.is_empty() {
                format!("{} {}", size_words(part.size), kind_words(&part.fstype))
            } else {
                format!(
                    "{} {} \u{201c}{}\u{201d}",
                    size_words(part.size),
                    kind_words(&part.fstype),
                    part.label
                )
            }
        })
        .collect();
    let count = disk.parts.len();
    let more = if count > 3 { " and more" } else { "" };
    format!(
        "{count} partition{}: {}{more}",
        if count == 1 { "" } else { "s" },
        named.join(", ")
    )
}

/// What a scan reads. In tests these are made up.
pub struct Sources<'a> {
    /// `/sys`.
    pub sys: &'a Path,
    /// `/proc/self/mountinfo`.
    pub mountinfo: &'a str,
    /// `/proc/swaps`.
    pub swaps: &'a str,
    /// What `lsblk -P -o NAME,FSTYPE,LABEL,TRAN,SERIAL,WWN,PTTYPE,PTUUID,PARTTYPE,PARTUUID` printed.
    pub contents: &'a str,
    /// Where the live medium is mounted.
    pub medium: &'a str,
}

fn read_trimmed(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}

/// A size file of sysfs: a count of 512-byte sectors, whatever the
/// disk's own sector size.
fn sectors(path: &Path) -> u64 {
    read_trimmed(path)
        .parse::<u64>()
        .unwrap_or(0)
        .saturating_mul(512)
}

fn has_entries(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
}

/// The value of `key` on a line of `lsblk -P`: `KEY="value"` pairs.
fn pair<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = line;
    loop {
        let (name, after) = rest.split_once("=\"")?;
        let (value, next) = after.split_once('"')?;
        if name.trim() == key {
            return Some(value);
        }
        rest = next;
    }
}

/// What lsblk gave under `key` for the device called `name`. It writes
/// characters it does not trust as \xNN; they are left as that.
fn told(contents: &str, name: &str, key: &str) -> String {
    contents
        .lines()
        .find(|line| pair(line, "NAME") == Some(name))
        .and_then(|line| pair(line, key))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The file system and label lsblk gave for the device called `name`.
fn contents_of(contents: &str, name: &str) -> (String, String) {
    (
        told(contents, name, "FSTYPE"),
        told(contents, name, "LABEL"),
    )
}

/// Where a device is mounted: by its number (`8:1`), and by its node
/// (`/dev/sda1`) as the source of the mount. The number alone is not
/// enough: btrfs mounts under a number of its own that belongs to no
/// device, and only the source says which disk it is on.
fn mount_points<'a>(
    mountinfo: &'a str,
    number: &str,
    node: &str,
) -> impl Iterator<Item = &'a str> + 'a {
    let (number, node) = (number.to_string(), node.to_string());
    mountinfo.lines().filter_map(move |line| {
        let (before, after) = line.split_once(" - ")?;
        let mut fields = before.split(' ');
        let device = fields.nth(2)?;
        let place = fields.nth(1)?;
        let source = after.split(' ').nth(1)?;
        ((!number.is_empty() && device == number) || source == node).then_some(place)
    })
}

/// Every disk there is, eligible or not, in name order. Loop devices
/// are left out unless `allow_loop`, which only a test's plan sets.
pub fn scan(sources: &Sources, allow_loop: bool) -> Vec<Disk> {
    let block = sources.sys.join("block");
    let mut names: Vec<String> = fs::read_dir(&block)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let swapping = |name: &str| {
        sources
            .swaps
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .any(|device| device.rsplit('/').next() == Some(name))
    };
    let mut disks = Vec::new();
    for name in names {
        if NOT_DISKS.iter().any(|kind| name.starts_with(kind))
            || (name.starts_with("loop") && !allow_loop)
            // The boot areas of an eMMC card are devices of their own.
            || name.contains("boot")
            || name.contains("rpmb")
        {
            continue;
        }
        let dir = block.join(&name);
        let size = sectors(&dir.join("size"));
        // A card reader with no card in it, a loop device with no file.
        if size == 0 {
            continue;
        }
        let (fstype, label) = contents_of(sources.contents, &name);
        let mut disk = Disk {
            path: format!("/dev/{name}"),
            size,
            model: read_trimmed(&dir.join("device/model")),
            serial: told(sources.contents, &name, "SERIAL"),
            wwn: told(sources.contents, &name, "WWN"),
            tran: told(sources.contents, &name, "TRAN"),
            read_only: read_trimmed(&dir.join("ro")) == "1",
            pttype: told(sources.contents, &name, "PTTYPE"),
            ptuuid: told(sources.contents, &name, "PTUUID").to_ascii_lowercase(),
            parts: Vec::new(),
            fstype,
            label,
            mounted: Vec::new(),
            swap: swapping(&name),
            held: has_entries(&dir.join("holders")),
            medium: false,
            name,
        };
        let mut numbers = vec![(read_trimmed(&dir.join("dev")), disk.path.clone())];
        let mut parts: Vec<String> = fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                    .filter(|part| dir.join(part).join("partition").exists())
                    .collect()
            })
            .unwrap_or_default();
        parts.sort();
        for part in parts {
            let part_dir = dir.join(&part);
            numbers.push((read_trimmed(&part_dir.join("dev")), format!("/dev/{part}")));
            disk.swap |= swapping(&part);
            disk.held |= has_entries(&part_dir.join("holders"));
            let (fstype, label) = contents_of(sources.contents, &part);
            disk.parts.push(Part {
                number: read_trimmed(&part_dir.join("partition"))
                    .parse()
                    .unwrap_or(0),
                start: read_trimmed(&part_dir.join("start")).parse().unwrap_or(0),
                size: sectors(&part_dir.join("size")),
                kind: told(sources.contents, &part, "PARTTYPE").to_ascii_lowercase(),
                uuid: told(sources.contents, &part, "PARTUUID").to_ascii_lowercase(),
                name: part,
                fstype,
                label,
            });
        }
        for (number, node) in &numbers {
            for place in mount_points(sources.mountinfo, number, node) {
                if place == sources.medium {
                    disk.medium = true;
                }
                if !disk.mounted.iter().any(|known| known == place) {
                    disk.mounted.push(place.to_string());
                }
            }
        }
        disks.push(disk);
    }
    disks
}

/// What lsblk says is on every block device. Empty if it cannot be run:
/// the disks are then listed without their contents.
fn lsblk() -> String {
    Command::new("lsblk")
        .args([
            "-P",
            "-o",
            "NAME,FSTYPE,LABEL,TRAN,SERIAL,WWN,PTTYPE,PTUUID,PARTTYPE,PARTUUID",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
}

/// The disks of this machine as they are now.
pub fn scan_system(allow_loop: bool) -> Vec<Disk> {
    let read = |path: &str| fs::read_to_string(path).unwrap_or_default();
    scan(
        &Sources {
            sys: Path::new("/sys"),
            mountinfo: &read("/proc/self/mountinfo"),
            swaps: &read("/proc/swaps"),
            contents: &lsblk(),
            medium: MEDIUM,
        },
        allow_loop,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const GB: u64 = 1_000_000_000;

    /// A made-up /sys/block entry. `parts` are sizes in bytes.
    pub fn add_disk(sys: &TempDir, name: &str, number: &str, bytes: u64, parts: &[u64]) {
        let dir = format!("block/{name}");
        sys.write(&format!("{dir}/size"), &format!("{}\n", bytes / 512));
        sys.write(&format!("{dir}/dev"), &format!("{number}:0\n"));
        sys.write(&format!("{dir}/ro"), "0\n");
        sys.mkdir(&format!("{dir}/holders"));
        for (index, size) in parts.iter().enumerate() {
            let sep = if name.ends_with(|c: char| c.is_ascii_digit()) {
                "p"
            } else {
                ""
            };
            let part = format!("{dir}/{name}{sep}{}", index + 1);
            sys.write(&format!("{part}/partition"), &format!("{}\n", index + 1));
            sys.write(&format!("{part}/size"), &format!("{}\n", size / 512));
            sys.write(&format!("{part}/dev"), &format!("{number}:{}\n", index + 1));
            sys.mkdir(&format!("{part}/holders"));
        }
    }

    fn sources<'a>(
        sys: &'a TempDir,
        mountinfo: &'a str,
        swaps: &'a str,
        contents: &'a str,
    ) -> Sources<'a> {
        Sources {
            sys: sys.path(),
            mountinfo,
            swaps,
            contents,
            medium: MEDIUM,
        }
    }

    const MOUNTS: &str = "\
22 1 0:20 / /sys rw,nosuid shared:7 - sysfs sysfs rw
30 1 0:27 / / rw,relatime shared:1 - overlay overlay rw,lowerdir=/run/sengi/rootfs
31 24 8:16 / /run/sengi/medium ro,relatime shared:13 - iso9660 /dev/sdb ro
32 24 7:0 / /run/sengi/rootfs ro,relatime shared:14 - squashfs /dev/loop0 ro
40 30 8:33 / /media/stick rw shared:20 - vfat /dev/sdc1 rw
";

    #[test]
    fn disks_are_listed_with_their_partitions_and_contents() {
        let sys = TempDir::new();
        add_disk(
            &sys,
            "sda",
            "8",
            256 * GB,
            &[104_857_600, 200 * GB, 55 * GB],
        );
        sys.write("block/sda/device/model", "SanDisk SD8SN8U \n");
        add_disk(&sys, "nvme0n1", "259", 500 * GB, &[]);
        let contents = "NAME=\"sda\" FSTYPE=\"\" LABEL=\"\"\n\
            NAME=\"sda1\" FSTYPE=\"vfat\" LABEL=\"ESP\"\n\
            NAME=\"sda2\" FSTYPE=\"ntfs\" LABEL=\"Windows\"\n\
            NAME=\"sda3\" FSTYPE=\"ext4\" LABEL=\"\"\n\
            NAME=\"nvme0n1\" FSTYPE=\"\" LABEL=\"\"\n";
        let disks = scan(&sources(&sys, "", "", contents), false);
        assert_eq!(disks.len(), 2);
        let sda = disks.iter().find(|disk| disk.name == "sda").unwrap();
        assert_eq!(sda.path, "/dev/sda");
        assert_eq!(sda.size, 256 * GB);
        assert_eq!(sda.model, "SanDisk SD8SN8U");
        assert_eq!(sda.parts.len(), 3);
        assert_eq!(sda.parts[1].fstype, "ntfs");
        assert_eq!(sda.parts[1].label, "Windows");
        assert_eq!(refusal(sda), None);
        assert_eq!(
            contents_words(sda),
            "3 partitions: 200 GB Windows (NTFS) \u{201c}Windows\u{201d}, 55 GB Linux, 104 MB FAT \u{201c}ESP\u{201d}"
        );
        let nvme = disks.iter().find(|disk| disk.name == "nvme0n1").unwrap();
        assert_eq!(contents_words(nvme), "Empty");
        assert_eq!(refusal(nvme), None);
    }

    #[test]
    fn the_disk_the_live_system_runs_from_is_refused() {
        let sys = TempDir::new();
        add_disk(&sys, "sda", "8", 256 * GB, &[]);
        // A flashed stick: the image is on the whole device, 8:16.
        sys.write("block/sdb/size", &format!("{}\n", 32 * GB / 512));
        sys.write("block/sdb/dev", "8:16\n");
        let disks = scan(&sources(&sys, MOUNTS, "", ""), false);
        let stick = disks.iter().find(|disk| disk.name == "sdb").unwrap();
        assert!(stick.medium);
        assert_eq!(refusal(stick), Some(Refusal::Medium));
        let sda = disks.iter().find(|disk| disk.name == "sda").unwrap();
        assert_eq!(refusal(sda), None);
    }

    #[test]
    fn a_disk_with_anything_mounted_is_refused() {
        let sys = TempDir::new();
        sys.write("block/sdc/size", &format!("{}\n", 64 * GB / 512));
        sys.write("block/sdc/dev", "8:32\n");
        sys.write("block/sdc/sdc1/partition", "1\n");
        sys.write("block/sdc/sdc1/size", &format!("{}\n", 64 * GB / 512));
        sys.write("block/sdc/sdc1/dev", "8:33\n");
        let disks = scan(&sources(&sys, MOUNTS, "", ""), false);
        assert_eq!(disks[0].mounted, ["/media/stick"]);
        assert_eq!(
            refusal(&disks[0]),
            Some(Refusal::Mounted("/media/stick".to_string()))
        );
    }

    #[test]
    fn swap_in_use_and_volumes_built_on_a_disk_refuse_it() {
        let sys = TempDir::new();
        add_disk(&sys, "sda", "8", 256 * GB, &[250 * GB, 6 * GB]);
        add_disk(&sys, "sdd", "8", 256 * GB, &[256 * GB]);
        sys.mkdir("block/sdd/sdd1/holders/dm-0");
        let swaps = "Filename\tType\tSize\tUsed\tPriority\n/dev/sda2 partition 5999 0 -2\n/dev/zram0 partition 1 0 100\n";
        let disks = scan(&sources(&sys, "", swaps, ""), false);
        assert_eq!(refusal(&disks[0]), Some(Refusal::Swap));
        assert_eq!(refusal(&disks[1]), Some(Refusal::Held));
    }

    #[test]
    fn small_and_read_only_disks_are_refused() {
        let sys = TempDir::new();
        add_disk(&sys, "sda", "8", 4 * GB, &[]);
        add_disk(&sys, "sdb", "8", 256 * GB, &[]);
        sys.write("block/sdb/ro", "1\n");
        add_disk(&sys, "sdc", "8", MIN_BYTES, &[]);
        let disks = scan(&sources(&sys, "", "", ""), false);
        assert_eq!(refusal(&disks[0]), Some(Refusal::TooSmall));
        assert_eq!(refusal(&disks[1]), Some(Refusal::ReadOnly));
        assert_eq!(refusal(&disks[2]), None);
    }

    #[test]
    fn what_is_not_a_disk_is_not_listed() {
        let sys = TempDir::new();
        for name in [
            "loop0",
            "zram0",
            "sr0",
            "ram0",
            "dm-0",
            "md0",
            "mmcblk0boot0",
            "mmcblk0rpmb",
        ] {
            add_disk(&sys, name, "7", 16 * GB, &[]);
        }
        add_disk(&sys, "mmcblk0", "179", 64 * GB, &[64 * GB]);
        // A card reader with nothing in it.
        add_disk(&sys, "sdx", "8", 0, &[]);
        let disks = scan(&sources(&sys, "", "", ""), false);
        let names: Vec<&str> = disks.iter().map(|disk| disk.name.as_str()).collect();
        assert_eq!(names, ["mmcblk0"]);
        assert_eq!(disks[0].parts[0].name, "mmcblk0p1");
        // A test may ask for loop devices, and gets nothing else extra.
        let disks = scan(&sources(&sys, "", "", ""), true);
        let names: Vec<&str> = disks.iter().map(|disk| disk.name.as_str()).collect();
        assert_eq!(names, ["loop0", "mmcblk0"]);
    }

    #[test]
    fn a_system_with_no_block_devices_has_no_disks() {
        let sys = TempDir::new();
        assert!(scan(&sources(&sys, "", "", ""), false).is_empty());
    }

    #[test]
    fn partitions_are_named_as_the_kernel_names_them() {
        assert_eq!(partition_path("/dev/sda", 3), "/dev/sda3");
        assert_eq!(partition_path("/dev/vda", 1), "/dev/vda1");
        assert_eq!(partition_path("/dev/nvme0n1", 2), "/dev/nvme0n1p2");
        assert_eq!(partition_path("/dev/mmcblk0", 1), "/dev/mmcblk0p1");
        assert_eq!(partition_path("/dev/loop7", 3), "/dev/loop7p3");
    }

    #[test]
    fn sizes_are_said_as_disks_are_sold() {
        assert_eq!(size_words(256_060_514_304), "256 GB");
        assert_eq!(size_words(1_000_204_886_016), "1 TB");
        assert_eq!(size_words(1_500_000_000_000), "1.5 TB");
        assert_eq!(size_words(8 << 30), "8.5 GB");
        assert_eq!(size_words(104_857_600), "104 MB");
        assert_eq!(size_words(512), "512 bytes");
        assert_eq!(size_words(u64::MAX), "18446744 TB");
    }

    #[test]
    fn what_is_on_a_disk_is_said_in_a_line() {
        let part = |size: u64, fstype: &str, label: &str| Part {
            name: String::new(),
            number: 0,
            start: 0,
            kind: String::new(),
            uuid: String::new(),
            size,
            fstype: fstype.to_string(),
            label: label.to_string(),
        };
        let sys = TempDir::new();
        add_disk(&sys, "sda", "8", 256 * GB, &[]);
        let mut disk = scan(&sources(&sys, "", "", ""), false).remove(0);
        disk.fstype = "iso9660".to_string();
        disk.label = "SENGIOS".to_string();
        assert_eq!(
            contents_words(&disk),
            "One volume, a disc image \u{201c}SENGIOS\u{201d}"
        );
        disk.parts = vec![part(GB, "", "")];
        assert_eq!(contents_words(&disk), "1 partition: 1 GB unknown");
        disk.parts = vec![
            part(GB, "vfat", ""),
            part(100 * GB, "crypto_LUKS", ""),
            part(50 * GB, "ext4", "home"),
            part(2 * GB, "swap", ""),
        ];
        assert_eq!(
            contents_words(&disk),
            "4 partitions: 100 GB encrypted, 50 GB Linux \u{201c}home\u{201d}, 2 GB swap and more"
        );
    }

    #[test]
    fn lsblk_pairs_are_read() {
        let line = "NAME=\"sda1\" FSTYPE=\"vfat\" LABEL=\"EFI system\"";
        assert_eq!(pair(line, "NAME"), Some("sda1"));
        assert_eq!(pair(line, "LABEL"), Some("EFI system"));
        assert_eq!(pair(line, "UUID"), None);
        assert_eq!(pair("nonsense", "NAME"), None);
        assert_eq!(contents_of("", "sda"), (String::new(), String::new()));
    }

    #[test]
    fn a_disk_is_known_by_its_serial_its_name_and_how_it_is_attached() {
        let sys = TempDir::new();
        add_disk(&sys, "sda", "8", 256 * GB, &[]);
        add_disk(&sys, "vda", "254", 16 * GB, &[]);
        let contents = "NAME=\"sda\" FSTYPE=\"\" LABEL=\"\" TRAN=\"sata\" SERIAL=\"S3Z8NB0K123456A \" WWN=\"0x5002538e40a1b2c3\"\n\
            NAME=\"vda\" FSTYPE=\"\" LABEL=\"\" TRAN=\"virtio\" SERIAL=\"\" WWN=\"\"\n";
        let disks = scan(&sources(&sys, "", "", contents), false);
        assert_eq!(disks[0].serial, "S3Z8NB0K123456A");
        assert_eq!(disks[0].wwn, "0x5002538e40a1b2c3");
        assert_eq!(disks[0].tran, "sata");
        assert_eq!(disks[1].serial, "");
        assert_eq!(disks[1].wwn, "");
        assert_eq!(disks[1].tran, "virtio");
    }

    #[test]
    fn a_mount_is_found_by_its_source_where_its_number_is_nobodys() {
        let sys = TempDir::new();
        add_disk(&sys, "sda", "8", 256 * GB, &[GB, 255 * GB]);
        // btrfs: the number 0:31 belongs to no device; the source says
        // which partition it is.
        let mounts = "55 30 0:31 /@home /mnt/data rw,relatime shared:30 - btrfs /dev/sda2 rw,subvol=/@home\n";
        let disks = scan(&sources(&sys, mounts, "", ""), false);
        assert_eq!(disks[0].mounted, ["/mnt/data"]);
        assert_eq!(
            refusal(&disks[0]),
            Some(Refusal::Mounted("/mnt/data".to_string()))
        );
        // A source that only starts the same is another disk.
        let other = "55 30 0:31 / /mnt/data rw - btrfs /dev/sda22 rw\n56 30 0:32 / /x rw - btrfs /dev/sdab rw\n";
        let disks = scan(&sources(&sys, other, "", ""), false);
        assert!(disks[0].mounted.is_empty());
        // The whole disk as the source counts too.
        let whole = "55 30 0:31 / /mnt/whole rw - btrfs /dev/sda rw\n";
        let disks = scan(&sources(&sys, whole, "", ""), false);
        assert_eq!(disks[0].mounted, ["/mnt/whole"]);
    }

    #[test]
    fn every_refusal_has_words() {
        for refusal in [
            Refusal::Medium,
            Refusal::Mounted("/mnt".to_string()),
            Refusal::Swap,
            Refusal::Held,
            Refusal::ReadOnly,
            Refusal::TooSmall,
        ] {
            assert!(!refusal.words().is_empty());
        }
        assert_eq!(Refusal::TooSmall.words(), "Too small, 8 GB needed");
    }
}
