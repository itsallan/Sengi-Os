//! Making Windows smaller, so that there is room for Sengi OS on a disk
//! that has none free.
//!
//! This is the one place where the installer changes data somebody
//! already has, so it does as little as possible and looks first, every
//! time:
//!
//! 1. Everything that can be known without writing is looked at
//!    ([`examine`]): the charger, which partition it is, what its first
//!    sector says, `ntfsresize --info`, the start of the hibernation file,
//!    and the room that would be left. This is all the window's question
//!    runs, and none of it opens the volume for writing.
//! 2. Once the person has said yes, the same again with `ntfs-3g.probe`,
//!    which opens the volume as a mount would, and `ntfsresize
//!    --no-action` for the exact size asked for ([`dry_run`]). Anything
//!    unexpected is a refusal. And all of it once more right before the
//!    first write.
//! 3. The NTFS file system is made to fit the size the partition will
//!    have, by `ntfsresize`, and checked. It ends a few kilobytes short of
//!    it, with the copy of its first sector in the last sector of that
//!    size, where Windows and `ntfsfix` look for it.
//! 4. Only the size of that partition is changed, to end where Sengi OS
//!    will begin, and the whole table is compared with what it was.
//!
//! Between 3 and 4 the disk is consistent: a file system may be smaller
//! than its partition. No partition is ever moved, no flag that says a
//! volume needs checking is ever cleared, and nothing is ever forced.
//!
//! Sizes are in sectors of 512 bytes unless they are called bytes.

use std::fs;
use std::path::Path;

use crate::beside::{Entry, Region, Table, ALIGN, MIN_BYTES};
use crate::disks::{partition_path, size_words, Disk, Part};
use crate::install::{Command, Head, Input, Output, System};

/// The partition type of a Windows data partition, C: among them. The
/// recovery partition, the reserved one and the system partition have
/// types of their own, and are never made smaller.
pub const BASIC_DATA_TYPE: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";

/// A gigabyte, as sizes are chosen and shown.
pub const GB: u64 = 1_000_000_000;

/// The least room Windows is left beyond what it uses.
pub const MARGIN_BYTES: u64 = 15 * GB;
/// And never less than this share of what it uses.
pub const MARGIN_PERCENT: u64 = 20;

/// How much of the hibernation file says whether Windows is hibernated.
pub const HIBERFILE_HEAD: usize = 4096;

/// Programs of the live system that making Windows smaller needs, beside
/// those every installation needs.
pub const PROGRAMS: [&str; 4] = ["ntfsresize", "ntfs-3g.probe", "ntfscat", "partx"];

/// What the person chose: which partition is made smaller, and how large
/// it is afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shrink {
    pub partition: u32,
    pub sectors: u64,
}

impl Shrink {
    /// What `ntfsresize` is told the file system's size is: that of the
    /// new partition. The volume it makes ends short of it, by room for
    /// the copy of its first sector, which it writes in the last sector.
    pub fn file_system_bytes(&self) -> u64 {
        self.sectors.saturating_mul(512)
    }
}

/// An NTFS volume, as its first sector describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ntfs {
    pub sector_bytes: u64,
    pub cluster_bytes: u64,
    /// The sectors of the volume, of its own size. NTFS keeps a copy of
    /// the first sector right after the last.
    pub sectors: u64,
    pub serial: u64,
}

impl Ntfs {
    /// Where the volume ends, room for the copy of its first sector
    /// included.
    pub fn end_bytes(&self) -> Option<u64> {
        self.sectors.checked_add(1)?.checked_mul(self.sector_bytes)
    }
}

/// What the first sector of a partition says is in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootSector {
    Ntfs(Ntfs),
    BitLocker,
    /// Anything else: another file system, an encrypted volume that says
    /// nothing about itself, or a damaged one.
    Other,
}

pub fn boot_sector(sector: &[u8]) -> BootSector {
    if sector.len() < 512 {
        return BootSector::Other;
    }
    // BitLocker writes its own name where NTFS had its.
    if &sector[3..11] == b"-FVE-FS-" {
        return BootSector::BitLocker;
    }
    if &sector[3..11] != b"NTFS    " || sector[510..512] != [0x55, 0xaa] {
        return BootSector::Other;
    }
    let sector_bytes = u64::from(u16::from_le_bytes([sector[11], sector[12]]));
    // Sectors in a cluster; above 128 it is a power of two, written as
    // its negative.
    let per_cluster = match sector[13] {
        n @ 1..=128 => u64::from(n),
        n @ 244.. => 1u64 << (256 - u32::from(n)),
        _ => return BootSector::Other,
    };
    let number = |at: usize| {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&sector[at..at + 8]);
        u64::from_le_bytes(bytes)
    };
    let sectors = number(40);
    if !matches!(sector_bytes, 512 | 1024 | 2048 | 4096) || sectors == 0 {
        return BootSector::Other;
    }
    BootSector::Ntfs(Ntfs {
        sector_bytes,
        cluster_bytes: sector_bytes * per_cluster,
        sectors,
        serial: number(72),
    })
}

/// Why Windows is not made smaller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// There is no charger plugged into a computer that has a battery.
    Battery,
    BitLocker,
    /// Not readable as NTFS: encrypted by something else, or damaged.
    NotNtfs,
    /// Hibernated, which is also what Fast Startup leaves behind.
    Hibernated,
    /// Not shut down properly: its journal still has work in it.
    Unclean,
    /// Marked as needing a disk check.
    Dirty,
    BadSectors,
    /// Mounted, or open in some other way.
    Busy,
    /// The check of the file system found something wrong.
    Damaged,
    /// Too little free space in it: what it uses, and how large the
    /// partition is, in bytes.
    NoRoom {
        used: u64,
        size: u64,
    },
    /// The disk's sectors are not of 512 bytes.
    SectorSize,
    /// Another partition has the same identifier, as a copied disk has.
    Twin,
    /// The target holds no Windows, and the partition of this number is
    /// encrypted with BitLocker: Windows may be in it, hibernated.
    OtherEncrypted(u32),
    /// The partition of this number could not be asked whether Windows
    /// is hibernated.
    Unchecked(u32),
    /// The file system ends so far before its partition that no size which
    /// could be given to Sengi OS would fit it.
    Short,
    /// Looking took longer than it may.
    Slow,
    /// A check ran and said something that is none of the above.
    Other(String),
}

/// What to do in Windows before shutting it down for this.
const SHUT_DOWN: &str = "In Windows, turn off Fast Startup (Control Panel, Power Options, \u{201c}Choose what the power buttons do\u{201d}), then shut it down with Shut down, not Restart, Sleep or Hibernate.";

/// What to do about a file system that ends before its partition does.
pub const EXTEND: &str = "In Windows, open Command Prompt as administrator, run \u{201c}diskpart\u{201d}, choose the drive with \u{201c}list volume\u{201d} and \u{201c}select volume\u{201d}, and enter \u{201c}extend filesystem\u{201d}. Then shut down fully and look again.";

impl Refusal {
    /// A few words, for the row of the choice.
    pub fn short(&self) -> &'static str {
        match self {
            Refusal::Battery => "Plug in the charger first",
            Refusal::BitLocker => "It is encrypted with BitLocker",
            Refusal::NotNtfs => "The partition cannot be read",
            Refusal::Hibernated => "Windows is hibernated or Fast Startup is on",
            Refusal::Unclean => "Windows was not shut down properly",
            Refusal::Dirty => "A Windows disk check is waiting",
            Refusal::BadSectors => "The disk reports bad sectors",
            Refusal::Busy => "The partition is in use",
            Refusal::Damaged => "The file system has errors",
            Refusal::NoRoom { .. } => "It does not have enough free space",
            Refusal::SectorSize => "This kind of disk is not supported",
            Refusal::Twin => "Two partitions have the same identifier",
            Refusal::OtherEncrypted(_) => "Another partition is encrypted with BitLocker",
            Refusal::Unchecked(_) => "Another partition could not be checked",
            Refusal::Short => "The file system is smaller than its partition",
            Refusal::Slow => "Looking took too long",
            Refusal::Other(_) => "It cannot be made smaller",
        }
    }

    /// What is wrong and what to do about it.
    pub fn words(&self) -> String {
        match self {
            Refusal::Battery => "This computer is running on its battery. Plug in the charger, then choose \u{201c}Look again\u{201d}. Losing power while a partition is being made smaller can destroy it.".to_string(),
            Refusal::BitLocker => "The partition is encrypted with BitLocker (Windows may call it device encryption), and Sengi OS does not change encrypted volumes. In Windows, save the recovery key, turn BitLocker off for this drive, wait until decryption has finished, then shut down fully.".to_string(),
            Refusal::NotNtfs => "The partition does not look like a Windows (NTFS) volume that can be read: it may be encrypted or damaged. Nothing was changed. Check it in Windows.".to_string(),
            Refusal::Hibernated => format!("Windows is hibernated, or Fast Startup saved part of it to disk. Changing the disk now would lose data. {SHUT_DOWN}"),
            Refusal::Unclean => format!("The volume has unfinished writes: Windows was not shut down properly. Start Windows, let it finish starting, then shut it down. {SHUT_DOWN}"),
            Refusal::Dirty => "The volume is marked for a disk check. Start Windows and let the check run (restart it twice if the mark stays), or run \u{201c}chkdsk X: /f\u{201d} as administrator, with the drive's letter for X, then shut down fully.".to_string(),
            Refusal::BadSectors => "The file system records bad sectors on this disk. A failing disk must not be resized. Back up your files now, and consider replacing the disk.".to_string(),
            Refusal::Busy => "The partition is open in this session, for example in Files. Close it, eject it, and choose \u{201c}Look again\u{201d}.".to_string(),
            Refusal::Damaged => "The check of the file system found errors. In Windows, run \u{201c}chkdsk X: /f\u{201d} as administrator, with the drive's letter for X, restart, then shut down fully.".to_string(),
            Refusal::NoRoom { used, size } => format!(
                "It uses {} of its {}. At least {} must stay free ({} GB, or {MARGIN_PERCENT}% of what is used if that is more), and Sengi OS needs {}. Free up space in Windows, then try again.",
                size_words(*used),
                size_words(*size),
                size_words(margin(*used)),
                MARGIN_BYTES / GB,
                size_words(smallest_gb() * GB),
            ),
            Refusal::SectorSize => "This disk has sectors of 4096 bytes, on which making a partition smaller has not been tried. Make room from inside Windows instead (Disk Management, Shrink Volume).".to_string(),
            Refusal::Twin => "Another partition has the same identifier as the one that would be made smaller, as happens when a disk was copied. Unplug the copy and choose \u{201c}Look again\u{201d}.".to_string(),
            Refusal::OtherEncrypted(number) => format!(
                "Partition {number} of this disk is encrypted with BitLocker, so whether Windows is hibernated cannot be checked. In Windows, save the recovery key, turn BitLocker off for every drive on this disk, wait until decryption has finished, then shut down fully."
            ),
            Refusal::Unchecked(number) => format!(
                "Partition {number} of this disk could not be asked whether Windows is hibernated. In Windows, run \u{201c}chkdsk X: /f\u{201d} on that drive as administrator, then shut down fully and choose \u{201c}Look again\u{201d}."
            ),
            Refusal::Short => format!("The file system ends too far before its partition does, as an interrupted attempt to make it smaller can leave it, and it cannot be taken as it is. {EXTEND}"),
            Refusal::Slow => "Looking at the disk took more than ten minutes and was stopped. Nothing was changed. The disk may be failing: in Windows, back up your files and run \u{201c}chkdsk X: /f\u{201d} as administrator.".to_string(),
            Refusal::Other(why) => format!(
                "A check of the partition did not pass: {why}. Nothing was changed. {SHUT_DOWN}"
            ),
        }
    }

    /// The refusal as one line of `sengi-install analyze`.
    pub fn code(&self) -> String {
        match self {
            Refusal::Battery => "battery".to_string(),
            Refusal::BitLocker => "bitlocker".to_string(),
            Refusal::NotNtfs => "not-ntfs".to_string(),
            Refusal::Hibernated => "hibernated".to_string(),
            Refusal::Unclean => "unclean".to_string(),
            Refusal::Dirty => "dirty".to_string(),
            Refusal::BadSectors => "bad-sectors".to_string(),
            Refusal::Busy => "busy".to_string(),
            Refusal::Damaged => "damaged".to_string(),
            Refusal::NoRoom { used, size } => format!("no-room {used} {size}"),
            Refusal::SectorSize => "sector-size".to_string(),
            Refusal::Twin => "twin".to_string(),
            Refusal::OtherEncrypted(number) => format!("other-encrypted {number}"),
            Refusal::Unchecked(number) => format!("unchecked {number}"),
            Refusal::Short => "short".to_string(),
            Refusal::Slow => "slow".to_string(),
            Refusal::Other(why) => format!(
                "other {}",
                why.split_whitespace().collect::<Vec<_>>().join(" ")
            ),
        }
    }

    pub fn from_code(code: &str) -> Option<Refusal> {
        let (word, rest) = code.split_once(' ').unwrap_or((code, ""));
        Some(match word {
            "battery" => Refusal::Battery,
            "bitlocker" => Refusal::BitLocker,
            "not-ntfs" => Refusal::NotNtfs,
            "hibernated" => Refusal::Hibernated,
            "unclean" => Refusal::Unclean,
            "dirty" => Refusal::Dirty,
            "bad-sectors" => Refusal::BadSectors,
            "busy" => Refusal::Busy,
            "damaged" => Refusal::Damaged,
            "no-room" => {
                let (used, size) = rest.split_once(' ')?;
                Refusal::NoRoom {
                    used: used.parse().ok()?,
                    size: size.parse().ok()?,
                }
            }
            "sector-size" => Refusal::SectorSize,
            "twin" => Refusal::Twin,
            "other-encrypted" => Refusal::OtherEncrypted(rest.parse().ok()?),
            "unchecked" => Refusal::Unchecked(rest.parse().ok()?),
            "short" => Refusal::Short,
            "slow" => Refusal::Slow,
            "other" => Refusal::Other(rest.to_string()),
            _ => return None,
        })
    }
}

/// What `ntfs-3g.probe` says with the status it ends with.
pub fn probe_refusal(code: Option<i32>) -> Option<Refusal> {
    match code {
        Some(0) => None,
        Some(13) => Some(Refusal::Damaged),
        Some(14) => Some(Refusal::Hibernated),
        Some(15) => Some(Refusal::Unclean),
        Some(16) => Some(Refusal::Busy),
        Some(12) => Some(Refusal::NotNtfs),
        Some(other) => Some(Refusal::Other(format!(
            "ntfs-3g.probe ended with status {other}"
        ))),
        None => Some(Refusal::Other(
            "ntfs-3g.probe was ended by a signal".to_string(),
        )),
    }
}

/// Why `ntfsresize` would not go on, from what it printed.
pub fn resize_refusal(said: &str) -> Refusal {
    let lower = said.to_ascii_lowercase();
    let has = |text: &str| lower.contains(text);
    if has("hibernat") {
        Refusal::Hibernated
    } else if has("scheduled for check") {
        Refusal::Dirty
    } else if has("unsafe state") || has("unclean") {
        Refusal::Unclean
    } else if has("bad sector") {
        Refusal::BadSectors
    } else if has("is mounted") || has("resource busy") {
        Refusal::Busy
    } else if has("inconsisten") || has("corrupt") || has("run chkdsk") {
        // Not "chkdsk" alone: a dry run says that it would schedule one.
        Refusal::Damaged
    } else {
        let last = said
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("it said nothing");
        Refusal::Other(format!("ntfsresize said: {last}"))
    }
}

/// What `ntfsresize --info` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub volume_bytes: u64,
    /// What is in use, which is also the least the volume could be made.
    pub used_bytes: u64,
}

/// The number that follows `label` on its line.
fn number_after(text: &str, label: &str) -> Option<u64> {
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with(label))?;
    let rest = line.trim_start().strip_prefix(label)?;
    let rest = rest.trim_start().trim_start_matches(':').trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// ```text
/// Current volume size: 2147480064 bytes (2148 MB)
/// Current device size: 2147483648 bytes (2148 MB)
/// Space in use       : 27 MB (1.2%)
/// You might resize at 26324992 bytes or 27 MB (freeing 2121 MB).
/// ```
///
/// `None` unless all of it is there: a volume whose use is not known is
/// not made smaller.
pub fn parse_info(text: &str) -> Option<Info> {
    let volume_bytes = number_after(text, "Current volume size")?;
    let used_mb = number_after(text, "Space in use")?;
    let least = number_after(text, "You might resize at")?;
    let used_bytes = used_mb.checked_mul(1_000_000)?.max(least);
    (volume_bytes > 0 && used_bytes <= volume_bytes.saturating_add(1_000_000)).then_some(Info {
        volume_bytes,
        used_bytes,
    })
}

/// How much free space Windows is left, for what it uses.
pub fn margin(used: u64) -> u64 {
    MARGIN_BYTES.max(used / 100 * MARGIN_PERCENT)
}

/// The smallest Sengi OS can be given, in whole gigabytes.
pub fn smallest_gb() -> u64 {
    MIN_BYTES.div_ceil(GB)
}

/// Where Sengi OS would go if it were given `gb` of the partition
/// `entry`, and what is left of that partition. `None` if that cannot be:
/// the new partition begins and ends on a mebibyte, inside the old one,
/// whose start stays where it is.
pub fn place(entry: &Entry, gb: u64) -> Option<(Shrink, Region)> {
    let wanted = gb
        .checked_mul(GB)?
        .div_ceil(512)
        .div_ceil(ALIGN)
        .checked_mul(ALIGN)?;
    let end = entry.end() / ALIGN * ALIGN;
    let start = end.checked_sub(wanted)?;
    if wanted == 0 || start <= entry.start {
        return None;
    }
    Some((
        Shrink {
            partition: entry.number,
            sectors: start - entry.start,
        },
        Region {
            start,
            sectors: wanted,
        },
    ))
}

/// What the person may choose between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    /// The smallest the Windows partition may become, in bytes.
    pub keep_bytes: u64,
    /// The least and the most Sengi OS can be given, in gigabytes.
    pub smallest_gb: u64,
    pub largest_gb: u64,
}

/// How much of the partition `entry` Sengi OS may take, when its volume
/// uses `used` bytes. Windows keeps what it uses and a margin.
pub fn room(entry: &Entry, used: u64) -> Result<Room, Refusal> {
    let size = entry.sectors.saturating_mul(512);
    let refused = Refusal::NoRoom { used, size };
    let keep_bytes = used.saturating_add(margin(used));
    let keep_sectors = keep_bytes.div_ceil(512);
    let end = entry.end() / ALIGN * ALIGN;
    let first = entry
        .start
        .saturating_add(keep_sectors)
        .div_ceil(ALIGN)
        .saturating_mul(ALIGN);
    let spare = end.saturating_sub(first).saturating_mul(512);
    let (smallest_gb, largest_gb) = (smallest_gb(), spare / GB);
    if largest_gb < smallest_gb {
        return Err(refused);
    }
    Ok(Room {
        keep_bytes,
        smallest_gb,
        largest_gb,
    })
}

/// Whether `shrink` and the new partition in `region` fit in the table:
/// one Windows data partition gets shorter, ends where the new one
/// begins, and the new one lies in what it gave up.
pub fn check_place(table: &Table, shrink: &Shrink, region: &Region) -> Result<(), String> {
    let entry = table
        .entry(shrink.partition)
        .ok_or("there is no partition to make smaller")?;
    if entry.kind != BASIC_DATA_TYPE {
        return Err(format!(
            "partition {} is not a Windows data partition",
            entry.number
        ));
    }
    if shrink.sectors == 0 || shrink.sectors >= entry.sectors {
        return Err("the partition would not become smaller".to_string());
    }
    if entry.start.checked_add(shrink.sectors) != Some(region.start) {
        return Err("the new partition does not begin where the shrunk one ends".to_string());
    }
    if region.end() > entry.end() {
        return Err("the new partition would reach beyond the space that is given up".to_string());
    }
    shrunk(table, shrink)?.region_is_free(region)
}

/// The table with the one change: the partition's size.
pub fn shrunk(table: &Table, shrink: &Shrink) -> Result<Table, String> {
    let mut after = table.clone();
    let entry = after
        .entries
        .iter_mut()
        .find(|entry| entry.number == shrink.partition)
        .ok_or("there is no partition to make smaller")?;
    if shrink.sectors == 0 || shrink.sectors >= entry.sectors {
        return Err("the partition would not become smaller".to_string());
    }
    entry.sectors = shrink.sectors;
    Ok(after)
}

/// After sfdisk has written: the table is the old one but for the size of
/// one partition. Its start, type, identifier, name and attributes, every
/// other partition, and the table's own identifier are as they were.
pub fn check_shrunk(before: &Table, after: &Table, shrink: &Shrink) -> Result<(), String> {
    let wanted = shrunk(before, shrink)?;
    if *after == wanted {
        return Ok(());
    }
    if after.id != before.id {
        return Err("the disk's identifier changed".to_string());
    }
    let numbers = wanted
        .entries
        .iter()
        .chain(&after.entries)
        .map(|entry| entry.number);
    for number in numbers {
        if wanted.entry(number) != after.entry(number) {
            return Err(format!(
                "partition {number} is not as it should be after the table was written"
            ));
        }
    }
    Err("the partition table is not the one that was asked for".to_string())
}

/// What sfdisk is given: the new size, in the disk's own sectors, and
/// nothing else, so that nothing else can change.
pub fn sfdisk_input(table: &Table, shrink: &Shrink) -> String {
    format!("size={}\n", shrink.sectors / (table.sector_size / 512))
}

pub fn sfdisk_argv(device: &str, number: u32) -> Vec<String> {
    [
        "sfdisk",
        "--lock",
        "--no-reread",
        "--no-tell-kernel",
        "--wipe",
        "never",
        "--wipe-partitions",
        "never",
        "-N",
        &number.to_string(),
        device,
    ]
    .map(String::from)
    .to_vec()
}

pub fn partx_argv(device: &str, number: u32) -> Vec<String> {
    ["partx", "--update", "--nr", &number.to_string(), device]
        .map(String::from)
        .to_vec()
}

pub fn probe_argv(node: &str) -> Vec<String> {
    ["ntfs-3g.probe", "--readwrite", node]
        .map(String::from)
        .to_vec()
}

pub fn info_argv(node: &str) -> Vec<String> {
    ["ntfsresize", "--info", "--no-progress-bar", node]
        .map(String::from)
        .to_vec()
}

pub fn dry_run_argv(node: &str, bytes: u64) -> Vec<String> {
    [
        "ntfsresize",
        "--no-action",
        "--no-progress-bar",
        "--size",
        &bytes.to_string(),
        node,
    ]
    .map(String::from)
    .to_vec()
}

/// The one command that changes the volume. No `--force`, and none of the
/// options that let it work on a volume it has doubts about.
pub fn resize_argv(node: &str, bytes: u64) -> Vec<String> {
    ["ntfsresize", "--size", &bytes.to_string(), node]
        .map(String::from)
        .to_vec()
}

/// Reads the file Windows hibernates into, of which only the start is
/// taken ([`HIBERFILE_HEAD`]). `ntfscat` mounts the volume read-only.
pub fn hiberfile_argv(node: &str) -> Vec<String> {
    ["ntfscat", node, "hiberfil.sys"].map(String::from).to_vec()
}

/// What the start of `hiberfil.sys` says, judged as libntfs-3g judges it
/// before it mounts a volume for writing: a signature means that Windows
/// is hibernated or Fast Startup saved it, and so does a file too short to
/// have a header. No such file means neither. Anything else that goes
/// wrong is a refusal too.
pub fn hiberfile_refusal(code: Option<i32>, head: &[u8], said: &str) -> Option<Refusal> {
    if head.starts_with(b"hibr") || head.starts_with(b"HIBR") {
        Some(Refusal::Hibernated)
    } else if head.len() >= HIBERFILE_HEAD {
        None
    } else if code == Some(0) {
        Some(Refusal::Hibernated)
    } else if head.is_empty() && said.contains("No such file or directory") {
        None
    } else {
        Some(Refusal::Other(
            "the hibernation file could not be read".to_string(),
        ))
    }
}

/// Where Windows keeps its kernel, as it spells it, and as old
/// installations that were upgraded may.
pub const WINDOWS_KERNELS: [&str; 2] = [
    "Windows/System32/ntoskrnl.exe",
    "WINDOWS/system32/ntoskrnl.exe",
];

/// Reads the start of a file that only a volume Windows is installed on
/// has. `ntfscat` mounts the volume read-only.
pub fn kernel_argv(node: &str, path: &str) -> Vec<String> {
    ["ntfscat", node, path].map(String::from).to_vec()
}

/// How many partitions of all the disks have the identifier `uuid`. More
/// than one means a disk that was copied.
pub fn twins(disks: &[Disk], uuid: &str) -> usize {
    disks
        .iter()
        .flat_map(|disk| &disk.parts)
        .filter(|part| part.uuid.eq_ignore_ascii_case(uuid))
        .count()
}

/// The partition of a disk that would be made smaller: its largest Windows
/// data partition that holds NTFS, or BitLocker, which is then said.
pub fn candidate(disk: &Disk) -> Option<&Part> {
    disk.parts
        .iter()
        .filter(|part| {
            part.kind == BASIC_DATA_TYPE && matches!(part.fstype.as_str(), "ntfs" | "BitLocker")
        })
        .max_by_key(|part| part.size)
}

/// Whether the power supplies of this computer, as `/sys/class/power_supply`
/// lists them, allow something that must not be cut short: either it has no
/// battery, or it is on a charger.
pub fn power(supplies: &Path) -> Result<(), Refusal> {
    let entries = match fs::read_dir(supplies) {
        Ok(entries) => entries,
        // A machine with no power supplies at all lists none.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(Refusal::Battery),
    };
    let (mut battery, mut mains, mut discharging, mut charging) = (false, false, false, true);
    for entry in entries {
        let Ok(entry) = entry else {
            return Err(Refusal::Battery);
        };
        let read = |name: &str| {
            fs::read_to_string(entry.path().join(name))
                .map(|text| text.trim().to_string())
                .unwrap_or_default()
        };
        // The battery of a mouse or a keyboard is not the computer's.
        if read("scope") == "Device" {
            continue;
        }
        match read("type").as_str() {
            "Battery" => {
                battery = true;
                let status = read("status");
                discharging |= status == "Discharging";
                charging &= status == "Charging";
            }
            _ => mains |= matches!(read("online").as_str(), "1" | "2"),
        }
    }
    if battery && (discharging || !(mains || charging)) {
        Err(Refusal::Battery)
    } else {
        Ok(())
    }
}

/// What the looking found, when nothing speaks against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Examined {
    /// The kernel's node of the partition, of the disk it was looked up on.
    pub node: String,
    pub ntfs: Ntfs,
    pub info: Info,
    pub room: Room,
    /// Windows is installed on this volume.
    pub windows: bool,
}

/// Why the looking ended without an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    Refused(Refusal),
    /// Something that is not about Windows went wrong: a program that
    /// cannot be run, a disk that is not what it was.
    Failed(String),
}

impl From<Refusal> for Stop {
    fn from(refusal: Refusal) -> Stop {
        Stop::Refused(refusal)
    }
}

/// The guard of whoever looks: every command is put to it before it runs.
pub type Allow<'a> = dyn Fn(&[String]) -> Result<(), String> + 'a;

fn run(sys: &mut dyn System, allow: &Allow, command: &Command) -> Result<Output, Stop> {
    allow(&command.argv).map_err(Stop::Failed)?;
    sys.run(command, &mut |_| {}).map_err(Stop::Failed)
}

fn said(output: &Output) -> String {
    format!("{}\n{}", output.stdout, output.stderr)
}

/// Everything that can be known about the partition `entry` of `device`
/// by looking. `entry` is as root read it from the table. With `probing`,
/// which is for after the person has said yes, `ntfs-3g.probe` also opens
/// the volume as a mount for writing would: it refuses a journal with work
/// left in it, which nothing that only reads can tell, and the library may
/// tidy one small record of the root folder as every such mount does.
/// Without it nothing opens the volume for writing.
///
/// Everything is done on the kernel's own node of the partition. A name
/// under `/dev/disk` can come to mean another disk's partition when a copy
/// of this disk is plugged in; the kernel's node of a disk that is held
/// cannot.
pub fn examine(
    sys: &mut dyn System,
    allow: &Allow,
    device: &str,
    table: &Table,
    entry: &Entry,
    probing: bool,
) -> Result<Examined, Stop> {
    sys.power()?;
    if table.sector_size != 512 {
        return Err(Refusal::SectorSize.into());
    }
    if entry.kind != BASIC_DATA_TYPE {
        return Err(Stop::Failed(format!(
            "partition {} is not a Windows data partition",
            entry.number
        )));
    }
    let node = partition_path(device, entry.number);
    if twins(&sys.disks(false), &entry.uuid) > 1 {
        return Err(Refusal::Twin.into());
    }
    let command = Command::new(&["blkid", "-s", "PARTUUID", "-o", "value", &node]);
    let output = run(sys, allow, &command)?;
    if output.code != Some(0) || !output.stdout.trim().eq_ignore_ascii_case(&entry.uuid) {
        return Err(Stop::Failed(format!(
            "{node} is not the partition that was chosen"
        )));
    }
    // The signature of the volume, read as it is.
    let first = sys.read_sector0(&node).map_err(Stop::Failed)?;
    let ntfs = match boot_sector(&first) {
        BootSector::Ntfs(ntfs) => ntfs,
        BootSector::BitLocker => return Err(Refusal::BitLocker.into()),
        BootSector::Other => return Err(Refusal::NotNtfs.into()),
    };
    if ntfs
        .end_bytes()
        .is_none_or(|end| end > entry.sectors.saturating_mul(512))
    {
        return Err(Refusal::Damaged.into());
    }
    if probing {
        let output = run(sys, allow, &command_of(probe_argv(&node)))?;
        if let Some(refusal) = probe_refusal(output.code) {
            return Err(refusal.into());
        }
    }
    let output = run(sys, allow, &command_of(info_argv(&node)))?;
    if output.code != Some(0) {
        return Err(resize_refusal(&said(&output)).into());
    }
    let info = parse_info(&output.stdout)
        .ok_or_else(|| Refusal::Other("ntfsresize did not say how much is in use".to_string()))?;
    // After the check above, which refuses a volume that ntfscat would too.
    if let Some(refusal) = hibernation(sys, allow, &node)? {
        return Err(refusal.into());
    }
    let windows = holds_windows(sys, allow, &node)?;
    check_others(sys, allow, device, table, entry.number, windows)?;
    let room = fitting(room(entry, info.used_bytes)?, entry, &ntfs)?;
    Ok(Examined {
        node,
        ntfs,
        info,
        room,
        windows,
    })
}

/// The start of a file of the volume at `command`'s node.
fn head(
    sys: &mut dyn System,
    allow: &Allow,
    argv: Vec<String>,
    bytes: usize,
) -> Result<Head, Stop> {
    let command = command_of(argv);
    allow(&command.argv).map_err(Stop::Failed)?;
    sys.head(&command, bytes).map_err(Stop::Failed)
}

/// What speaks against the volume at `node` in its hibernation file.
fn hibernation(sys: &mut dyn System, allow: &Allow, node: &str) -> Result<Option<Refusal>, Stop> {
    let found = head(sys, allow, hiberfile_argv(node), HIBERFILE_HEAD)?;
    Ok(hiberfile_refusal(found.code, &found.bytes, &found.said))
}

fn holds_windows(sys: &mut dyn System, allow: &Allow, node: &str) -> Result<bool, Stop> {
    for path in WINDOWS_KERNELS {
        if head(sys, allow, kernel_argv(node, path), 2)?
            .bytes
            .starts_with(b"MZ")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Every other partition of the disk that holds NTFS is asked whether
/// Windows is hibernated: the one that is made smaller need not be the one
/// Windows is on, and a hibernated Windows would write what it remembers of
/// the disk when it wakes. Where the target holds no Windows, a partition
/// that cannot be looked into because BitLocker encrypts it may be the one
/// that does.
fn check_others(
    sys: &mut dyn System,
    allow: &Allow,
    device: &str,
    table: &Table,
    target: u32,
    windows: bool,
) -> Result<(), Stop> {
    for other in table.entries.iter().filter(|entry| entry.number != target) {
        let node = partition_path(device, other.number);
        match boot_sector(&sys.read_sector0(&node).map_err(Stop::Failed)?) {
            BootSector::BitLocker if !windows => {
                return Err(Refusal::OtherEncrypted(other.number).into())
            }
            BootSector::Ntfs(_) => match hibernation(sys, allow, &node)? {
                None => {}
                Some(Refusal::Hibernated) => return Err(Refusal::Hibernated.into()),
                Some(_) => return Err(Refusal::Unchecked(other.number).into()),
            },
            _ => {}
        }
    }
    Ok(())
}

/// `room` without the sizes that the file system as it is cannot take. A
/// partition is made no larger than a cluster or two beyond where its file
/// system ends, unless the file system is made smaller too: where it already
/// ends well before the partition, as an attempt that stopped between the
/// two leaves it, Sengi OS has to be given at least what lies beyond it.
pub fn fitting(room: Room, entry: &Entry, ntfs: &Ntfs) -> Result<Room, Refusal> {
    let end = ntfs.end_bytes().ok_or(Refusal::Damaged)?;
    let reach = end.saturating_add(2 * ntfs.cluster_bytes);
    // How large the partition would be if Sengi OS got nothing.
    let whole = (entry.end() / ALIGN * ALIGN)
        .saturating_sub(entry.start)
        .saturating_mul(512);
    let mut smallest_gb = room.smallest_gb.max(whole.saturating_sub(reach) / GB);
    while smallest_gb <= room.largest_gb
        && place(entry, smallest_gb).is_some_and(|(shrink, _)| shrink.file_system_bytes() > reach)
    {
        smallest_gb += 1;
    }
    if smallest_gb > room.largest_gb {
        return Err(Refusal::Short);
    }
    Ok(Room {
        smallest_gb,
        ..room
    })
}

fn command_of(argv: Vec<String>) -> Command<'static> {
    Command {
        argv,
        input: Input::None,
        cleanup: false,
        whole: false,
    }
}

/// `ntfsresize` goes through the whole of the resize and writes nothing.
pub fn dry_run(sys: &mut dyn System, allow: &Allow, node: &str, bytes: u64) -> Result<(), Stop> {
    let output = run(sys, allow, &command_of(dry_run_argv(node, bytes)))?;
    if output.code != Some(0) || !output.stdout.contains("test run ended successfully") {
        return Err(resize_refusal(&said(&output)).into());
    }
    Ok(())
}

/// After the volume was made smaller: it is the volume that was there, it
/// says it ends inside the new partition, it did become about as small as
/// asked, and `last`, the last sector of the size that was asked for,
/// holds the copy of its first sector.
pub fn check_volume(
    before: &Ntfs,
    first: &[u8],
    last: &[u8],
    shrink: &Shrink,
) -> Result<(), String> {
    let BootSector::Ntfs(now) = boot_sector(first) else {
        return Err("its first sector is not that of an NTFS volume".to_string());
    };
    if now.serial != before.serial
        || now.sector_bytes != before.sector_bytes
        || now.cluster_bytes != before.cluster_bytes
    {
        return Err("it is not the volume that was there".to_string());
    }
    let target = shrink.file_system_bytes();
    let end = now.end_bytes().ok_or("it says it has no end")?;
    if end > target {
        return Err(format!(
            "it ends {} bytes in, beyond the {target} asked for",
            end
        ));
    }
    if end.saturating_add(2 * now.cluster_bytes) < target {
        return Err("it became much smaller than was asked for".to_string());
    }
    if first.len() < 512 || last.len() < 512 || last[..512] != first[..512] {
        return Err("the copy of its first sector is not where it should be".to_string());
    }
    Ok(())
}

/// What `sengi-install analyze` says about a disk: the table as root read
/// it, the partition that would be made smaller, and what may be chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    /// [`Table::snapshot`].
    pub table: String,
    pub esp: u32,
    pub number: u32,
    pub start: u64,
    pub sectors: u64,
    pub used_bytes: u64,
    pub room: Room,
    /// Windows is installed on the partition. Where it is not, the
    /// partition is called by its label and size.
    pub windows: bool,
}

impl Analysis {
    /// The partition as the table has it, as far as the sizes go.
    pub fn entry(&self) -> Entry {
        Entry {
            number: self.number,
            start: self.start,
            sectors: self.sectors,
            kind: BASIC_DATA_TYPE.to_string(),
            uuid: String::new(),
            name: String::new(),
            attrs: String::new(),
        }
    }

    pub fn render(&self) -> String {
        format!(
            "table {}\nesp {}\npartition {} {} {}\nused {}\nroom {} {} {}\nwindows {}\n",
            self.table,
            self.esp,
            self.number,
            self.start,
            self.sectors,
            self.used_bytes,
            self.room.keep_bytes,
            self.room.smallest_gb,
            self.room.largest_gb,
            u8::from(self.windows),
        )
    }

    /// What was printed: an analysis, a refusal, or neither, which is
    /// an error. Nothing is taken from half an answer.
    pub fn parse(text: &str) -> Result<Result<Analysis, Refusal>, String> {
        let line = |word: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(word)?.strip_prefix(' '))
                .ok_or(format!("the analysis does not say {word}"))
        };
        if let Ok(code) = line("refused") {
            return Refusal::from_code(code)
                .map(Err)
                .ok_or(format!("the analysis refused with {code}"));
        }
        let numbers = |word: &str, count: usize| -> Result<Vec<u64>, String> {
            let found: Option<Vec<u64>> = line(word)?
                .split(' ')
                .map(|number| number.parse().ok())
                .collect();
            found
                .filter(|found| found.len() == count)
                .ok_or(format!("the analysis has no numbers for {word}"))
        };
        let partition = numbers("partition", 3)?;
        let room = numbers("room", 3)?;
        Ok(Ok(Analysis {
            table: line("table")?.to_string(),
            esp: numbers("esp", 1)?[0] as u32,
            number: partition[0] as u32,
            start: partition[1],
            sectors: partition[2],
            used_bytes: numbers("used", 1)?[0],
            room: Room {
                keep_bytes: room[0],
                smallest_gb: room[1],
                largest_gb: room[2],
            },
            windows: numbers("windows", 1)?[0] == 1,
        }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testutil::TempDir;

    /// The first sector of an NTFS volume of `sectors` sectors of 512
    /// bytes, in clusters of 4096.
    pub fn ntfs_sector(sectors: u64) -> Vec<u8> {
        let mut sector = vec![0u8; 512];
        sector[..3].copy_from_slice(&[0xeb, 0x52, 0x90]);
        sector[3..11].copy_from_slice(b"NTFS    ");
        sector[11..13].copy_from_slice(&512u16.to_le_bytes());
        sector[13] = 8;
        sector[40..48].copy_from_slice(&sectors.to_le_bytes());
        sector[72..80].copy_from_slice(&0x1b9e_5d0e_716a_0e77_u64.to_le_bytes());
        sector[510] = 0x55;
        sector[511] = 0xaa;
        sector
    }

    /// What `ntfsresize --info` printed for a volume, with these figures.
    pub fn info_text(volume: u64, used_mb: u64) -> String {
        format!(
            "ntfsresize v2022.10.3 (libntfs-3g)\nDevice name        : /dev/sda2\nNTFS volume version: 3.1\nCluster size       : 4096 bytes\nCurrent volume size: {volume} bytes ({} MB)\nCurrent device size: {} bytes ({} MB)\nChecking filesystem consistency ...\nAccounting clusters ...\nSpace in use       : {used_mb} MB (1.2%)\nCollecting resizing constraints ...\nYou might resize at {} bytes or {used_mb} MB (freeing 1 MB).\nPlease make a test run using both the -n and -s options before real resizing!\n",
            volume / 1_000_000,
            volume + 4096,
            volume / 1_000_000,
            used_mb * 1_000_000 - 500_000,
        )
    }

    /// A Windows partition of 200 GB that starts where Windows puts it.
    fn windows() -> Entry {
        Entry {
            number: 3,
            start: 239_616,
            sectors: 390_625_000,
            kind: BASIC_DATA_TYPE.to_string(),
            uuid: "dd068ba6-d1df-4fe6-9721-13fc754e035f".to_string(),
            name: "Basic data partition".to_string(),
            attrs: String::new(),
        }
    }

    fn table() -> Table {
        let mut esp = windows();
        esp.number = 1;
        esp.start = 2048;
        esp.sectors = 204_800;
        esp.kind = crate::beside::ESP_TYPE.to_string();
        esp.uuid = "aaaa".to_string();
        let mut recovery = windows();
        recovery.number = 4;
        recovery.start = 239_616 + 390_625_000 + 600;
        recovery.sectors = 1_000_000;
        recovery.kind = "de94bba4-06d1-4d40-a16a-bfd50179d6ac".to_string();
        recovery.uuid = "bbbb".to_string();
        recovery.attrs = "RequiredPartition GUID:63".to_string();
        Table {
            id: "table".to_string(),
            first: 2048,
            end: 500_000_000,
            sector_size: 512,
            entries: vec![esp, windows(), recovery],
        }
    }

    #[test]
    fn the_first_sector_says_what_is_in_a_partition() {
        let BootSector::Ntfs(ntfs) = boot_sector(&ntfs_sector(1_953_120)) else {
            panic!("not read as NTFS");
        };
        assert_eq!(ntfs.sector_bytes, 512);
        assert_eq!(ntfs.cluster_bytes, 4096);
        assert_eq!(ntfs.sectors, 1_953_120);
        assert_eq!(ntfs.end_bytes(), Some(999_997_952));
        // Clusters of 2 MB are written as a power of two.
        let mut large = ntfs_sector(10);
        large[13] = 0xf4;
        let BootSector::Ntfs(large) = boot_sector(&large) else {
            panic!("not read as NTFS");
        };
        assert_eq!(large.cluster_bytes, 4096 * 512);
        // BitLocker, in any state: it puts its name there.
        let mut locked = ntfs_sector(10);
        locked[3..11].copy_from_slice(b"-FVE-FS-");
        assert_eq!(boot_sector(&locked), BootSector::BitLocker);
        // Anything else is not something to resize.
        for change in [
            |s: &mut Vec<u8>| s[3..11].copy_from_slice(b"EXFAT   "),
            |s: &mut Vec<u8>| s[510] = 0,
            |s: &mut Vec<u8>| s[13] = 0,
            |s: &mut Vec<u8>| s[13] = 200,
            |s: &mut Vec<u8>| s[11..13].copy_from_slice(&520u16.to_le_bytes()),
            |s: &mut Vec<u8>| s[40..48].fill(0),
            |s: &mut Vec<u8>| s.fill(0x5a),
        ] {
            let mut sector = ntfs_sector(10);
            change(&mut sector);
            assert_eq!(boot_sector(&sector), BootSector::Other);
        }
        assert_eq!(boot_sector(&[0; 100]), BootSector::Other);
    }

    #[test]
    fn what_ntfsresize_found_is_read_in_full_or_not_at_all() {
        let text = info_text(2_147_480_064, 27);
        assert_eq!(
            parse_info(&text),
            Some(Info {
                volume_bytes: 2_147_480_064,
                used_bytes: 27_000_000,
            })
        );
        for gone in ["Current volume size", "Space in use", "You might resize"] {
            let cut: String = text
                .lines()
                .filter(|line| !line.starts_with(gone))
                .map(|line| format!("{line}\n"))
                .collect();
            assert_eq!(parse_info(&cut), None, "{gone}");
        }
        // More in use than the volume holds is nonsense.
        assert_eq!(parse_info(&info_text(1_000_000_000, 5000)), None);
        assert_eq!(parse_info(""), None);
    }

    #[test]
    fn what_the_tools_say_becomes_a_refusal_in_words() {
        for (said, refusal) in [
            // As ntfsresize 2022.10.3 and libntfs-3g write them.
            ("ERROR: Volume is scheduled for check.\nRun chkdsk /f and please try again, or see option -f.", Refusal::Dirty),
            ("Windows is hibernated, refused to mount.\nERROR(1): Opening '/dev/sda2' as NTFS failed: Operation not permitted", Refusal::Hibernated),
            ("The disk contains an unclean file system (0, 0).\nMetadata kept in Windows cache, refused to mount.", Refusal::Unclean),
            ("ERROR: This software has detected that the disk has at least 2 bad sectors.\n", Refusal::BadSectors),
            ("ERROR: Device '/dev/sda2' is mounted read-write. You must 'umount' it first.", Refusal::Busy),
            ("ERROR: Cluster allocation inconsistency!\nPlease run chkdsk.", Refusal::Damaged),
        ] {
            assert_eq!(resize_refusal(said), refusal, "{said}");
        }
        assert_eq!(
            resize_refusal("ERROR: out of cheese\n\n"),
            Refusal::Other("ntfsresize said: ERROR: out of cheese".to_string())
        );
        // What a dry run prints on its way does not decide anything.
        assert!(matches!(
            resize_refusal("Checking filesystem consistency ...\nSchedule chkdsk for NTFS consistency check at Windows boot time ...\nERROR: out of memory\n"),
            Refusal::Other(_)
        ));
        assert_eq!(probe_refusal(Some(0)), None);
        assert_eq!(probe_refusal(Some(14)), Some(Refusal::Hibernated));
        assert_eq!(probe_refusal(Some(15)), Some(Refusal::Unclean));
        assert_eq!(probe_refusal(Some(13)), Some(Refusal::Damaged));
        // Any other status is a refusal too: nothing but 0 goes on.
        for code in [Some(1), Some(11), Some(18), Some(19), None] {
            assert!(probe_refusal(code).is_some(), "{code:?}");
        }
    }

    fn all_refusals() -> Vec<Refusal> {
        vec![
            Refusal::Battery,
            Refusal::BitLocker,
            Refusal::NotNtfs,
            Refusal::Hibernated,
            Refusal::Unclean,
            Refusal::Dirty,
            Refusal::BadSectors,
            Refusal::Busy,
            Refusal::Damaged,
            Refusal::NoRoom {
                used: 90 * GB,
                size: 100 * GB,
            },
            Refusal::SectorSize,
            Refusal::Twin,
            Refusal::OtherEncrypted(3),
            Refusal::Unchecked(4),
            Refusal::Short,
            Refusal::Slow,
            Refusal::Other("it said nothing".to_string()),
        ]
    }

    #[test]
    fn every_refusal_says_why_and_what_to_do_and_survives_the_line() {
        for refusal in all_refusals() {
            assert!(!refusal.short().is_empty() && refusal.short().len() < 60);
            assert!(refusal.words().len() > refusal.short().len());
            assert_eq!(Refusal::from_code(&refusal.code()), Some(refusal.clone()));
        }
        for refusal in [Refusal::Hibernated, Refusal::Unclean] {
            assert!(refusal.words().contains("Fast Startup"));
            assert!(refusal.words().contains("Shut down"));
        }
        assert!(Refusal::BitLocker.words().contains("turn BitLocker off"));
        assert!(Refusal::BitLocker.words().contains("recovery key"));
        assert!(Refusal::Dirty.words().contains("chkdsk"));
        assert!(Refusal::Battery.words().contains("charger"));
        assert_eq!(Refusal::from_code("no-room 1"), None);
        assert_eq!(Refusal::from_code("fine"), None);
    }

    #[test]
    fn a_file_system_that_ends_early_raises_the_least_sengi_os_may_be_given() {
        let entry = windows();
        let room = room(&entry, 50 * GB).unwrap();
        let volume = |sectors: u64| Ntfs {
            sector_bytes: 512,
            cluster_bytes: 4096,
            sectors,
            serial: 1,
        };
        // A volume that fills its partition leaves the choice as it is.
        assert_eq!(fitting(room, &entry, &volume(entry.sectors - 1)), Ok(room));
        // An earlier attempt made it ready for 40 GB of Sengi OS: less
        // than that would leave the partition ending far beyond it, and
        // that size is still offered, and gives exactly the same.
        let (earlier, _) = place(&entry, 40).unwrap();
        let ended = volume(earlier.sectors - 8);
        let fitted = fitting(room, &entry, &ended).unwrap();
        assert_eq!(fitted.smallest_gb, 40);
        for gb in [fitted.smallest_gb, fitted.largest_gb] {
            let (shrink, _) = place(&entry, gb).unwrap();
            assert!(shrink.file_system_bytes() <= ended.end_bytes().unwrap() + 2 * 4096);
        }
        let (less, _) = place(&entry, 39).unwrap();
        assert!(less.file_system_bytes() > ended.end_bytes().unwrap() + 2 * 4096);
        // So far before it that nothing can be offered.
        assert_eq!(
            fitting(room, &entry, &volume(1_000_000)),
            Err(Refusal::Short)
        );
        assert!(Refusal::Short.words().contains("extend filesystem"));
    }

    #[test]
    fn windows_keeps_the_larger_of_fifteen_gigabytes_and_a_fifth() {
        assert_eq!(margin(0), 15 * GB);
        assert_eq!(margin(75 * GB), 15 * GB);
        assert_eq!(margin(76 * GB), 15_200_000_000);
        assert_eq!(margin(500 * GB), 100 * GB);
        assert_eq!(smallest_gb(), 11);
        assert!(smallest_gb() * GB >= MIN_BYTES);
        // 200 GB with 50 used: 65 are kept, and Sengi OS may have the rest.
        let entry = windows();
        let room = room(&entry, 50 * GB).unwrap();
        assert_eq!(room.keep_bytes, 65 * GB);
        assert_eq!(room.smallest_gb, 11);
        assert_eq!(room.largest_gb, 134);
        // Whatever of that is chosen leaves Windows its margin.
        for gb in [room.smallest_gb, 64, room.largest_gb] {
            let (shrink, region) = place(&entry, gb).unwrap();
            assert!(shrink.file_system_bytes() >= room.keep_bytes, "{gb}");
            assert!(region.bytes() >= gb * GB && region.bytes() >= MIN_BYTES);
        }
        // And one more would not.
        let (shrink, _) = place(&entry, room.largest_gb + 1).unwrap();
        assert!(shrink.sectors * 512 < room.keep_bytes + 512 * 2048);
    }

    #[test]
    fn a_full_windows_is_not_made_smaller() {
        let entry = windows();
        // 200 GB, of which the least that must stay is used * 1.2.
        for used in [158 * GB, 190 * GB, 200 * GB] {
            assert_eq!(
                room(&entry, used),
                Err(Refusal::NoRoom {
                    used,
                    size: entry.sectors * 512
                }),
                "{used}"
            );
        }
        assert!(room(&entry, 155 * GB).is_ok());
        // A small partition has no room even when empty.
        let mut small = windows();
        small.sectors = 25 * GB / 512;
        assert!(room(&small, 0).is_err());
        small.sectors = 28 * GB / 512;
        assert!(room(&small, 0).is_ok());
    }

    #[test]
    fn the_new_partition_is_at_the_end_on_whole_mebibytes_and_the_start_stays() {
        let mut entry = windows();
        // An end and a start that are on no boundary.
        entry.start = 239_617;
        entry.sectors = 390_624_001;
        let (shrink, region) = place(&entry, 64).unwrap();
        assert_eq!(region.start % ALIGN, 0);
        assert_eq!(region.sectors % ALIGN, 0);
        assert!(region.end() <= entry.end());
        assert!(entry.end() - region.end() < ALIGN);
        assert_eq!(entry.start + shrink.sectors, region.start);
        assert_eq!(shrink.partition, 3);
        assert!(region.bytes() >= 64 * GB && region.bytes() < 64 * GB + ALIGN * 512);
        // The file system is given the partition's size, and no more.
        assert_eq!(shrink.file_system_bytes(), shrink.sectors * 512);
        // Nothing, or all of it and more, is no choice.
        assert_eq!(place(&entry, 0), None);
        assert_eq!(place(&entry, 200), None);
        assert_eq!(place(&entry, u64::MAX), None);
    }

    #[test]
    fn only_one_windows_partition_may_shrink_and_only_into_what_it_gives_up() {
        let table = table();
        let (shrink, region) = place(table.entry(3).unwrap(), 50).unwrap();
        assert_eq!(check_place(&table, &shrink, &region), Ok(()));
        let bad = |shrink: Shrink, region: Region, word: &str| {
            let error = check_place(&table, &shrink, &region).unwrap_err();
            assert!(error.contains(word), "{shrink:?} {region:?}: {error}");
        };
        // The system partition, the recovery partition, one that is not there.
        bad(
            Shrink {
                partition: 1,
                ..shrink
            },
            region,
            "not a Windows data partition",
        );
        bad(
            Shrink {
                partition: 4,
                ..shrink
            },
            region,
            "not a Windows data partition",
        );
        bad(
            Shrink {
                partition: 9,
                ..shrink
            },
            region,
            "no partition",
        );
        // Not smaller.
        bad(
            Shrink {
                sectors: 390_625_000,
                ..shrink
            },
            region,
            "smaller",
        );
        bad(
            Shrink {
                sectors: 0,
                ..shrink
            },
            region,
            "smaller",
        );
        // The new partition somewhere else, or reaching past the old end.
        bad(
            shrink,
            Region {
                start: region.start + ALIGN,
                sectors: region.sectors,
            },
            "does not begin",
        );
        bad(
            shrink,
            Region {
                start: region.start,
                sectors: region.sectors + ALIGN,
            },
            "beyond",
        );
        // Too small a new partition.
        bad(
            shrink,
            Region {
                start: region.start,
                sectors: ALIGN,
            },
            "under",
        );
    }

    #[test]
    fn after_the_write_only_the_one_size_may_differ() {
        let before = table();
        let (shrink, _) = place(before.entry(3).unwrap(), 50).unwrap();
        let after = shrunk(&before, &shrink).unwrap();
        assert_eq!(after.entry(3).unwrap().sectors, shrink.sectors);
        assert_eq!(check_shrunk(&before, &after, &shrink), Ok(()));
        // The same table, unwritten, is not what was asked for.
        assert!(check_shrunk(&before, &before, &shrink)
            .unwrap_err()
            .contains("partition 3"));
        // Every field of every partition, and the table's own.
        let changes: [fn(&mut Entry); 7] = [
            |e| e.start += 1,
            |e| e.sectors -= 1,
            |e| e.kind = "x".to_string(),
            |e| e.uuid = "x".to_string(),
            |e| e.name = "x".to_string(),
            |e| e.attrs = "x".to_string(),
            |e| e.number += 20,
        ];
        for change in changes {
            for index in 0..3 {
                let mut bad = after.clone();
                change(&mut bad.entries[index]);
                assert!(check_shrunk(&before, &bad, &shrink).is_err());
            }
        }
        for change in [
            |t: &mut Table| t.id = "other".to_string(),
            |t: &mut Table| t.first += 1,
            |t: &mut Table| t.end -= 1,
            |t: &mut Table| t.sector_size = 4096,
            |t: &mut Table| {
                t.entries.pop();
            },
            |t: &mut Table| {
                let mut extra = t.entries[0].clone();
                extra.number = 9;
                t.entries.push(extra);
            },
        ] {
            let mut bad = after.clone();
            change(&mut bad);
            assert!(check_shrunk(&before, &bad, &shrink).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn sfdisk_is_told_the_size_and_nothing_else() {
        let table = table();
        let (shrink, _) = place(table.entry(3).unwrap(), 50).unwrap();
        assert_eq!(
            sfdisk_input(&table, &shrink),
            format!("size={}\n", shrink.sectors)
        );
        assert_eq!(
            sfdisk_argv("/dev/sda", 3).join(" "),
            "sfdisk --lock --no-reread --no-tell-kernel --wipe never --wipe-partitions never -N 3 /dev/sda"
        );
        assert_eq!(
            partx_argv("/dev/sda", 3).join(" "),
            "partx --update --nr 3 /dev/sda"
        );
    }

    #[test]
    fn nothing_that_is_run_on_a_volume_forces_anything() {
        let node = "/dev/disk/by-partuuid/abcd";
        let all = [
            probe_argv(node),
            info_argv(node),
            // ntfscat names the file after the volume.
            hiberfile_argv(node)[..2].to_vec(),
            dry_run_argv(node, 5),
            resize_argv(node, 5),
        ];
        for argv in &all {
            for arg in argv {
                assert!(
                    ![
                        "-f",
                        "-ff",
                        "--force",
                        "-b",
                        "--bad-sectors",
                        "--clear-dirty",
                        "-d",
                        "-x",
                        "--expand"
                    ]
                    .contains(&arg.as_str()),
                    "{argv:?}"
                );
            }
            assert_eq!(argv.last().map(String::as_str), Some(node));
        }
        assert_eq!(
            resize_argv(node, 5).join(" "),
            format!("ntfsresize --size 5 {node}")
        );
        assert_eq!(
            dry_run_argv(node, 5).join(" "),
            format!("ntfsresize --no-action --no-progress-bar --size 5 {node}")
        );
    }

    #[test]
    fn the_largest_windows_data_partition_is_the_one() {
        let part = |number: u32, size: u64, kind: &str, fstype: &str| Part {
            name: format!("sda{number}"),
            number,
            start: 0,
            size,
            kind: kind.to_string(),
            uuid: format!("{number}"),
            fstype: fstype.to_string(),
            label: String::new(),
        };
        let mut disk = crate::disks::Disk {
            name: "sda".to_string(),
            path: "/dev/sda".to_string(),
            size: 0,
            model: String::new(),
            serial: String::new(),
            wwn: String::new(),
            tran: String::new(),
            read_only: false,
            pttype: "gpt".to_string(),
            ptuuid: "x".to_string(),
            parts: vec![
                part(1, 100, crate::beside::ESP_TYPE, "vfat"),
                part(2, 900, "de94bba4-06d1-4d40-a16a-bfd50179d6ac", "ntfs"),
                part(3, 500, BASIC_DATA_TYPE, "ntfs"),
                part(4, 300, BASIC_DATA_TYPE, "ntfs"),
                part(5, 800, BASIC_DATA_TYPE, "exfat"),
                part(6, 800, "0fc63daf-8483-4772-8e79-3d69d8477de4", "ext4"),
            ],
            fstype: String::new(),
            label: String::new(),
            mounted: Vec::new(),
            swap: false,
            held: false,
            medium: false,
        };
        // Never the recovery partition, however large, nor another system's.
        assert_eq!(candidate(&disk).map(|part| part.number), Some(3));
        // BitLocker is chosen too, so that it is said.
        disk.parts[3] = part(4, 700, BASIC_DATA_TYPE, "BitLocker");
        assert_eq!(candidate(&disk).map(|part| part.number), Some(4));
        disk.parts.truncate(2);
        assert_eq!(candidate(&disk), None);
    }

    fn supplies(list: &[(&str, &[(&str, &str)])]) -> TempDir {
        let tmp = TempDir::new();
        for (name, files) in list {
            for (file, contents) in *files {
                tmp.write(&format!("{name}/{file}"), &format!("{contents}\n"));
            }
        }
        tmp
    }

    #[test]
    fn a_laptop_needs_its_charger() {
        let ok = |list: &[(&str, &[(&str, &str)])]| power(supplies(list).path());
        let battery = |status: &'static str| [("type", "Battery"), ("status", status)];
        let charger = |online: &'static str| [("type", "Mains"), ("online", online)];
        // A desktop: nothing at all, or only the charger.
        assert_eq!(ok(&[]), Ok(()));
        assert_eq!(power(Path::new("/no/such/directory")), Ok(()));
        assert_eq!(ok(&[("AC", &charger("1"))]), Ok(()));
        // On the charger.
        for status in ["Charging", "Full", "Not charging", "Unknown"] {
            assert_eq!(
                ok(&[("BAT0", &battery(status)), ("AC", &charger("1"))]),
                Ok(()),
                "{status}"
            );
        }
        // Charging over USB, with no charger listed.
        assert_eq!(ok(&[("BAT0", &battery("Charging"))]), Ok(()));
        assert_eq!(
            ok(&[
                ("BAT0", &battery("Full")),
                ("ucsi", &[("type", "USB"), ("online", "1")])
            ]),
            Ok(())
        );
        // Off it, or nothing says it is on.
        for list in [
            vec![("BAT0", battery("Discharging")), ("AC", charger("0"))],
            vec![("BAT0", battery("Full")), ("AC", charger("0"))],
            vec![("BAT0", battery("Unknown"))],
            // A charger that is too weak: the battery still drains.
            vec![("BAT0", battery("Discharging")), ("AC", charger("1"))],
            vec![
                ("BAT0", battery("Charging")),
                ("BAT1", battery("Discharging")),
                ("AC", charger("1")),
            ],
        ] {
            let list: Vec<(&str, &[(&str, &str)])> = list
                .iter()
                .map(|(name, files)| (*name, &files[..]))
                .collect();
            assert_eq!(ok(&list), Err(Refusal::Battery), "{list:?}");
        }
        // A mouse's battery is not the computer's.
        assert_eq!(
            ok(&[(
                "hid",
                &[
                    ("type", "Battery"),
                    ("status", "Discharging"),
                    ("scope", "Device")
                ]
            )]),
            Ok(())
        );
    }

    #[test]
    fn the_volume_after_the_resize_has_to_be_the_one_asked_for() {
        let BootSector::Ntfs(before) = boot_sector(&ntfs_sector(390_625_000 - 1)) else {
            panic!();
        };
        let (shrink, _) = place(&windows(), 50).unwrap();
        let target = shrink.file_system_bytes();
        // As ntfsresize does it: whole clusters, and room for a copy of
        // the first sector.
        let sectors = (target - 512) / 4096 * 8;
        let first = ntfs_sector(sectors);
        assert_eq!(check_volume(&before, &first, &first, &shrink), Ok(()));
        // The copy that should be in the last sector is not.
        for last in [vec![0; 512], ntfs_sector(before.sectors), vec![0; 10]] {
            let error = check_volume(&before, &first, &last, &shrink).unwrap_err();
            assert!(error.contains("copy"), "{error}");
        }
        let check = |first: &[u8], word: &str| {
            let error = check_volume(&before, first, first, &shrink).unwrap_err();
            assert!(error.contains(word), "{word}: {error}");
        };
        check(&[0; 512], "not that of an NTFS");
        // Not made smaller, too large by a cluster, much too small.
        check(&ntfs_sector(before.sectors), "beyond");
        check(&ntfs_sector(sectors + 8), "beyond");
        check(&ntfs_sector(sectors / 2), "much smaller");
        // Another volume.
        let mut other = ntfs_sector(sectors);
        other[72] ^= 1;
        check(&other, "not the volume");
    }

    #[test]
    fn a_hibernation_file_with_a_signature_means_hibernated() {
        let missing = "ERROR: Couldn't open inode: No such file or directory\n";
        let full = |start: &[u8]| {
            let mut head = vec![0u8; HIBERFILE_HEAD];
            head[..start.len()].copy_from_slice(start);
            head
        };
        // Hibernated or Fast Startup, however much of it was read.
        for start in [&b"hibr"[..], b"HIBR"] {
            assert_eq!(
                hiberfile_refusal(None, &full(start), ""),
                Some(Refusal::Hibernated)
            );
            assert_eq!(
                hiberfile_refusal(Some(0), start, ""),
                Some(Refusal::Hibernated)
            );
        }
        // No such file, or one that Windows has resumed from.
        assert_eq!(hiberfile_refusal(Some(1), b"", missing), None);
        assert_eq!(hiberfile_refusal(None, &full(b"wake"), ""), None);
        assert_eq!(hiberfile_refusal(None, &full(b""), ""), None);
        // Too short to have a header, as ntfs-3g takes it.
        assert_eq!(
            hiberfile_refusal(Some(0), b"short", ""),
            Some(Refusal::Hibernated)
        );
        // It could not be read: nothing is assumed.
        for (code, head, said) in [
            (Some(1), &b""[..], "ERROR: Volume is corrupt"),
            (Some(1), b"part", missing),
            (None, b"", ""),
        ] {
            assert!(
                matches!(hiberfile_refusal(code, head, said), Some(Refusal::Other(_))),
                "{code:?} {said}"
            );
        }
    }

    #[test]
    fn what_analyze_prints_reads_back_whole_or_not_at_all() {
        let analysis = Analysis {
            table: table().snapshot(),
            esp: 1,
            number: 3,
            start: 239_616,
            sectors: 390_625_000,
            used_bytes: 50 * GB,
            room: room(&windows(), 50 * GB).unwrap(),
            windows: true,
        };
        assert_eq!(
            Analysis::parse(&analysis.render()),
            Ok(Ok(analysis.clone()))
        );
        assert_eq!(analysis.entry().sectors, 390_625_000);
        for refusal in all_refusals() {
            let text = format!("refused {}\n", refusal.code());
            assert_eq!(Analysis::parse(&text), Ok(Err(refusal)));
        }
        let text = analysis.render();
        let data = Analysis {
            windows: false,
            ..analysis.clone()
        };
        assert_eq!(Analysis::parse(&data.render()), Ok(Ok(data)));
        for word in ["table", "esp", "partition", "used", "room", "windows"] {
            let cut: String = text
                .lines()
                .filter(|line| !line.starts_with(word))
                .map(|line| format!("{line}\n"))
                .collect();
            assert!(Analysis::parse(&cut).is_err(), "{word}");
        }
        assert!(Analysis::parse("").is_err());
        assert!(Analysis::parse("refused perhaps\n").is_err());
    }
}
