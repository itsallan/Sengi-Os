//! Installing beside another system: what is free on a disk that is
//! already in use, and the rules that keep everything else as it is.
//!
//! Only one thing is ever written to such a disk: one new partition, in
//! free space, and a directory and a boot entry for Sengi OS on the EFI
//! system partition that is already there. Nothing is shrunk, moved,
//! formatted or wiped. The checks here are what holds the installation
//! to that, and each of them stops it before anything is written or, if
//! it can only be known afterwards, before anything is formatted:
//!
//! - the table is read again as root, just before it is written, and must
//!   be the one the person was shown ([`check_before`]);
//! - the region must lie inside the disk's usable area, on a boundary of
//!   a mebibyte, and overlap no partition ([`Table::region_is_free`]);
//! - after sfdisk has written, the table is read again and every old
//!   partition must be as it was, with exactly one new partition, the one
//!   asked for ([`check_after`]);
//! - then the kernel's own view must say the same ([`check_kernel`]);
//! - and no command that writes to a disk may name anything but the new
//!   partition ([`guard`]).
//!
//! All sizes here are in sectors of 512 bytes, as sysfs gives them, also
//! for a disk whose own sectors are larger; [`Table::parse_dump`] and
//! [`append_input`] convert.

use crate::disks::{partition_path, size_words, Disk};

/// The least free space that is offered: Sengi OS and room to use it.
pub const MIN_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// The least free room on the EFI system partition for GRUB.
pub const ESP_MIN_FREE_BYTES: u64 = 10 * 1024 * 1024;

/// A mebibyte, in sectors: regions start and end on one.
pub const ALIGN: u64 = 2048;

/// Partition types, in small letters.
pub const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
pub const LINUX_TYPE: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";

/// The name the new partition gets in the table.
pub const PART_NAME: &str = "Sengi OS";

/// Where the table may be used, when only the disk's size is known: a
/// mebibyte at either end is left alone, which is more than any GPT
/// needs.
const EDGE: u64 = ALIGN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub number: u32,
    pub start: u64,
    pub sectors: u64,
    pub kind: String,
    pub uuid: String,
    /// The partition's name and attributes (Windows marks its recovery
    /// partition with them), as root reads them. The window cannot know
    /// them, so they are not in the snapshot, but they are compared
    /// before and after the table is written.
    pub name: String,
    pub attrs: String,
}

impl Entry {
    pub fn end(&self) -> u64 {
        self.start.saturating_add(self.sectors)
    }
}

/// A stretch of free sectors: the first and how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub start: u64,
    pub sectors: u64,
}

impl Region {
    pub fn end(&self) -> u64 {
        self.start.saturating_add(self.sectors)
    }

    pub fn bytes(&self) -> u64 {
        self.sectors.saturating_mul(512)
    }
}

/// A GPT as it was read, in sectors of 512 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The table's own identifier, in small letters.
    pub id: String,
    /// The first sector partitions may use, and the one after the last.
    pub first: u64,
    pub end: u64,
    /// The disk's own sector size: 512 or 4096.
    pub sector_size: u64,
    pub entries: Vec<Entry>,
}

fn align_up(sector: u64) -> u64 {
    sector.div_ceil(ALIGN).saturating_mul(ALIGN)
}

fn align_down(sector: u64) -> u64 {
    sector / ALIGN * ALIGN
}

impl Table {
    /// The table as the window knows it, from what the kernel and udev
    /// say, without being root. It cannot know the table's own limits,
    /// so a mebibyte at each end is left out.
    pub fn from_disk(disk: &Disk) -> Result<Table, Why> {
        if disk.pttype != "gpt" || disk.ptuuid.is_empty() {
            return Err(Why::NotGpt);
        }
        let mut entries = Vec::new();
        for part in &disk.parts {
            if part.number == 0 || part.kind.is_empty() || part.uuid.is_empty() {
                return Err(Why::Unreadable);
            }
            entries.push(Entry {
                number: part.number,
                start: part.start,
                sectors: part.size / 512,
                kind: part.kind.to_ascii_lowercase(),
                uuid: part.uuid.to_ascii_lowercase(),
                name: String::new(),
                attrs: String::new(),
            });
        }
        entries.sort_by_key(|entry| entry.number);
        Ok(Table {
            id: disk.ptuuid.to_ascii_lowercase(),
            first: EDGE,
            end: (disk.size / 512).saturating_sub(EDGE),
            sector_size: 512,
            entries,
        })
    }

    /// The table from what `sfdisk -d` prints, which is what root sees:
    ///
    /// ```text
    /// label: gpt
    /// label-id: F6691C2B-D8E2-4C39-8EE0-426C24613EF0
    /// device: /dev/sda
    /// unit: sectors
    /// first-lba: 2048
    /// last-lba: 134217694
    /// sector-size: 512
    ///
    /// /dev/sda1 : start=        2048, size=      204800, type=C12A..., uuid=6251..., name="EFI"
    /// ```
    ///
    /// Anything it does not understand is an error: a table that cannot
    /// be read is not one to write to.
    pub fn parse_dump(text: &str) -> Result<Table, String> {
        let mut label = None;
        let mut id = String::new();
        let (mut first, mut last) = (None, None);
        let mut sector_size = 512;
        let mut raw: Vec<Entry> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((node, fields)) = line.split_once(" : ") {
                raw.push(parse_partition_line(node, fields)?);
                continue;
            }
            let (key, value) = line.split_once(':').ok_or(format!(
                "the table has a line that is not understood: {line}"
            ))?;
            let value = value.trim();
            let number = || {
                value
                    .parse::<u64>()
                    .map_err(|_| format!("the table has a number that is not one: {line}"))
            };
            match key.trim() {
                "label" => label = Some(value.to_string()),
                "label-id" => id = value.to_ascii_lowercase(),
                "first-lba" => first = Some(number()?),
                "last-lba" => last = Some(number()?),
                "sector-size" => sector_size = number()?,
                "unit" if value != "sectors" => {
                    return Err(format!("the table is in {value}, not sectors"))
                }
                _ => {}
            }
        }
        if label.as_deref() != Some("gpt") {
            return Err("the partition table is not a GPT".to_string());
        }
        if id.is_empty() {
            return Err("the partition table has no identifier".to_string());
        }
        if sector_size != 512 && sector_size != 4096 {
            return Err(format!("the disk has sectors of {sector_size} bytes"));
        }
        let factor = sector_size / 512;
        let (Some(first), Some(last)) = (first, last) else {
            return Err("the table does not say where its usable space is".to_string());
        };
        let mut entries = Vec::new();
        for entry in raw {
            let converted = Entry {
                start: entry
                    .start
                    .checked_mul(factor)
                    .ok_or("a number is too large")?,
                sectors: entry
                    .sectors
                    .checked_mul(factor)
                    .ok_or("a number is too large")?,
                ..entry
            };
            entries.push(converted);
        }
        entries.sort_by_key(|entry| entry.number);
        for pair in entries.windows(2) {
            if pair[0].number == pair[1].number {
                return Err(format!("partition {} is listed twice", pair[0].number));
            }
        }
        Ok(Table {
            id,
            first: first.checked_mul(factor).ok_or("a number is too large")?,
            end: last
                .checked_add(1)
                .and_then(|end| end.checked_mul(factor))
                .ok_or("a number is too large")?,
            sector_size,
            entries,
        })
    }

    /// What the person was shown, as one line that says exactly which
    /// table it was: the identifier, and each partition's number, place,
    /// type and own identifier. Changing any of them changes this.
    pub fn snapshot(&self) -> String {
        let mut out = format!("gpt {}", self.id);
        for entry in &self.entries {
            out.push_str(&format!(
                ";{}:{}:{}:{}:{}",
                entry.number, entry.start, entry.sectors, entry.kind, entry.uuid
            ));
        }
        out
    }

    /// The free stretches that are big enough, on mebibyte boundaries.
    pub fn free_regions(&self) -> Vec<Region> {
        let mut spans: Vec<(u64, u64)> = self
            .entries
            .iter()
            .map(|entry| (entry.start, entry.end()))
            .collect();
        spans.sort();
        let mut found = Vec::new();
        let mut gap = |from: u64, to: u64| {
            let (start, end) = (align_up(from), align_down(to));
            if end > start && (end - start).saturating_mul(512) >= MIN_BYTES {
                found.push(Region {
                    start,
                    sectors: end - start,
                });
            }
        };
        let mut cursor = self.first;
        for (start, end) in spans {
            if start > cursor {
                gap(cursor, start);
            }
            cursor = cursor.max(end);
        }
        if self.end > cursor {
            gap(cursor, self.end);
        }
        found
    }

    /// Whether `region` may be written to: inside what the table can
    /// use, on whole mebibytes, big enough, and touching no partition.
    pub fn region_is_free(&self, region: &Region) -> Result<(), String> {
        if !region.start.is_multiple_of(ALIGN) || !region.sectors.is_multiple_of(ALIGN) {
            return Err("the free space does not start and end on a whole mebibyte".to_string());
        }
        if region.bytes() < MIN_BYTES {
            return Err(format!("the free space is under {}", size_words(MIN_BYTES)));
        }
        if region.start < self.first || region.end() > self.end {
            return Err("the free space is not inside the disk's usable space".to_string());
        }
        for entry in &self.entries {
            if entry.start < region.end() && region.start < entry.end() {
                return Err(format!(
                    "the free space is not free: partition {} is in it",
                    entry.number
                ));
            }
        }
        Ok(())
    }

    pub fn entry(&self, number: u32) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.number == number)
    }
}

/// What follows `key` in a line of the dump, up to the closing quote, or
/// to the next comma where there is no quote.
fn quoted_after(line: &str, key: &str) -> String {
    let Some(at) = line.find(key) else {
        return String::new();
    };
    let rest = &line[at + key.len()..];
    match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or_default().to_string(),
        None => rest
            .split(',')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

fn parse_partition_line(node: &str, fields: &str) -> Result<Entry, String> {
    let digits: String = node
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let number: u32 = digits
        .parse()
        .map_err(|_| format!("{node} has no partition number"))?;
    // The name may hold commas and anything else, and the attributes
    // come after it: both are taken first, and what is left is split.
    let name = quoted_after(fields, "name=");
    let attrs = quoted_after(fields, "attrs=");
    let fields = fields.split(", name=").next().unwrap_or_default();
    let (mut start, mut sectors) = (None, None);
    let (mut kind, mut uuid) = (String::new(), String::new());
    for field in fields.split(',') {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "start" => start = value.parse::<u64>().ok(),
            "size" => sectors = value.parse::<u64>().ok(),
            "type" => kind = value.to_ascii_lowercase(),
            "uuid" => uuid = value.to_ascii_lowercase(),
            _ => {}
        }
    }
    match (start, sectors) {
        (Some(start), Some(sectors)) if !kind.is_empty() && !uuid.is_empty() => Ok(Entry {
            number,
            start,
            sectors,
            kind,
            uuid,
            name,
            attrs,
        }),
        _ => Err(format!("partition {number} is not described in full")),
    }
}

/// What the computer was started with, as far as installing beside
/// another system cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Firmware {
    pub uefi: bool,
    /// Secure Boot is on: the firmware would refuse the unsigned GRUB that
    /// is put on the EFI system partition.
    pub secure_boot: bool,
}

impl From<bool> for Firmware {
    fn from(uefi: bool) -> Firmware {
        Firmware {
            uefi,
            secure_boot: false,
        }
    }
}

/// The firmware variable that says whether Secure Boot is on: four bytes
/// of attributes, then one byte, 1 for on.
pub const SECURE_BOOT_VAR: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// Whether the contents of [`SECURE_BOOT_VAR`] say Secure Boot is on.
pub fn secure_boot_from(var: &[u8]) -> bool {
    var.len() >= 5 && var[var.len() - 1] == 1
}

/// How this computer was started, as it is now.
pub fn firmware_now() -> Firmware {
    Firmware {
        uefi: std::path::Path::new("/sys/firmware/efi").is_dir(),
        secure_boot: std::fs::read(SECURE_BOOT_VAR).is_ok_and(|var| secure_boot_from(&var)),
    }
}

/// Why installing beside another system is not offered on a disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    NotUefi,
    NoSystem,
    NotGpt,
    Unreadable,
    NoEsp,
    EspNotFat,
    NoRoom,
    SecureBoot,
    SeveralEsp,
}

impl Why {
    /// One line, for the row of the choice.
    pub fn words(&self) -> String {
        match self {
            Why::NotUefi => "This computer did not start with UEFI, which this needs".to_string(),
            Why::NoSystem => "There is nothing on this disk to install beside".to_string(),
            Why::NotGpt => "The disk does not use a GPT partition table".to_string(),
            Why::Unreadable => "The disk's partition table could not be read".to_string(),
            Why::NoEsp => "There is no EFI system partition on this disk".to_string(),
            Why::EspNotFat => "The EFI system partition is not a FAT volume".to_string(),
            Why::SecureBoot => {
                "Secure Boot is on. Turn it off in the firmware settings first".to_string()
            }
            Why::SeveralEsp => {
                "There is more than one EFI system partition, and which one holds the other system is not known".to_string()
            }
            Why::NoRoom => format!(
                "No free space of {} or more. Shrink the other system first, from inside it",
                size_words(MIN_BYTES)
            ),
        }
    }
}

/// What can be done on a disk that qualifies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The existing EFI system partition's number.
    pub esp: u32,
    /// The free stretches, the biggest first.
    pub regions: Vec<Region>,
    /// What the table was, for the plan to hold the installation to.
    pub snapshot: String,
}

impl Offer {
    /// The stretch Sengi OS would take: the biggest.
    pub fn region(&self) -> Region {
        self.regions[0]
    }
}

/// Whether Sengi OS can go beside what is on `disk`, and where.
pub fn offer(disk: &Disk, firmware: impl Into<Firmware>) -> Result<Offer, Why> {
    let firmware = firmware.into();
    if !firmware.uefi {
        return Err(Why::NotUefi);
    }
    if firmware.secure_boot {
        return Err(Why::SecureBoot);
    }
    if disk.parts.is_empty() {
        return Err(Why::NoSystem);
    }
    let table = Table::from_disk(disk)?;
    let esps: Vec<&crate::disks::Part> = disk
        .parts
        .iter()
        .filter(|part| part.kind == ESP_TYPE)
        .collect();
    if esps.len() > 1 {
        return Err(Why::SeveralEsp);
    }
    let esp = esps.into_iter().find(|part| part.fstype == "vfat");
    let Some(esp) = esp else {
        return Err(if disk.parts.iter().any(|part| part.kind == ESP_TYPE) {
            Why::EspNotFat
        } else {
            Why::NoEsp
        });
    };
    let mut regions = table.free_regions();
    if regions.is_empty() {
        return Err(Why::NoRoom);
    }
    regions.sort_by_key(|region| std::cmp::Reverse(region.sectors));
    Ok(Offer {
        esp: esp.number,
        regions,
        snapshot: table.snapshot(),
    })
}

/// What the plan carries of a choice made beside another system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beside {
    pub start: u64,
    pub sectors: u64,
    pub esp: u32,
    /// The table as it was shown: [`Table::snapshot`].
    pub table: String,
}

impl Beside {
    pub fn region(&self) -> Region {
        Region {
            start: self.start,
            sectors: self.sectors,
        }
    }
}

/// Before anything is written: the table as root reads it now is the one
/// that was shown, the EFI system partition is still one, and the free
/// space is still free.
pub fn check_before(table: &Table, beside: &Beside) -> Result<(), String> {
    if table.snapshot() != beside.table {
        return Err(
            "the partition table of the disk has changed since it was shown; nothing was written"
                .to_string(),
        );
    }
    match table.entry(beside.esp) {
        Some(entry) if entry.kind == ESP_TYPE => {}
        _ => return Err("the EFI system partition is not what it was".to_string()),
    }
    table.region_is_free(&beside.region())
}

/// After sfdisk has written, before the kernel is asked to read it:
/// every partition there was is as it was, and there is one more, which
/// is exactly what was asked for. Gives the new partition's number.
pub fn check_after(
    before: &Table,
    after: &Table,
    beside: &Beside,
    uuid: &str,
) -> Result<u32, String> {
    if after.id != before.id {
        return Err("the disk's identifier changed".to_string());
    }
    for old in &before.entries {
        match after.entry(old.number) {
            Some(now) if now == old => {}
            _ => {
                return Err(format!(
                    "partition {} is not as it was after the table was written",
                    old.number
                ))
            }
        }
    }
    let new: Vec<&Entry> = after
        .entries
        .iter()
        .filter(|entry| before.entry(entry.number).is_none())
        .collect();
    let [new] = new.as_slice() else {
        return Err(format!("the table has {} new partitions, not 1", new.len()));
    };
    let region = beside.region();
    if new.start != region.start
        || new.sectors != region.sectors
        || new.kind != LINUX_TYPE
        || new.uuid != uuid.to_ascii_lowercase()
    {
        return Err("the new partition is not the one that was asked for".to_string());
    }
    // The same test as before, with the new partition left out.
    let mut old_only = after.clone();
    old_only.entries.retain(|entry| entry.number != new.number);
    old_only.region_is_free(&region)?;
    Ok(new.number)
}

/// The kernel's view, number, first sector and length, after it has been
/// told about the new partition: the old ones where they were, and the new
/// one, nothing else.
pub fn check_kernel(
    before: &Table,
    seen: &[(u32, u64, u64)],
    region: &Region,
    new: u32,
) -> Result<(), String> {
    let mut want: Vec<(u32, u64, u64)> = before
        .entries
        .iter()
        .map(|entry| (entry.number, entry.start, entry.sectors))
        .collect();
    want.push((new, region.start, region.sectors));
    want.sort();
    let mut seen = seen.to_vec();
    seen.sort();
    if seen == want {
        Ok(())
    } else {
        Err(format!(
            "the kernel shows {} partitions that are not the ones the table has",
            seen.len()
        ))
    }
}

/// What sfdisk is given to add the partition: the script is the one line,
/// in the disk's own sectors, no more.
pub fn append_input(table: &Table, region: &Region, uuid: &str) -> String {
    let factor = table.sector_size / 512;
    format!(
        "start={}, size={}, type={}, uuid={}, name=\"{PART_NAME}\"\n",
        region.start / factor,
        region.sectors / factor,
        LINUX_TYPE.to_ascii_uppercase(),
        uuid.to_ascii_uppercase()
    )
}

/// A version 4 identifier written as GPT wants it, from 32 hexadecimal
/// digits. `None` for anything else.
pub fn make_uuid(hex: &str) -> Option<String> {
    let hex = hex.trim().to_ascii_lowercase();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let variant = match hex.as_bytes()[16] {
        b'0' | b'4' | b'8' | b'c' => '8',
        b'1' | b'5' | b'9' | b'd' => '9',
        b'2' | b'6' | b'a' | b'e' => 'a',
        _ => 'b',
    };
    Some(format!(
        "{}-{}-4{}-{}{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        variant,
        &hex[17..20],
        &hex[20..32]
    ))
}

/// Sector 0 of a GPT disk is a protective MBR: a signature and at most
/// one entry, of type 0xEE. A hybrid MBR, which also lists partitions of
/// its own for old systems, is not something to write beside.
pub fn mbr_is_plain(sector: &[u8]) -> Result<(), String> {
    if sector.len() < 512 {
        return Err("sector 0 of the disk could not be read".to_string());
    }
    let mut protective = 0;
    for entry in sector[446..510].chunks(16) {
        let kind = entry[4];
        if kind == 0 && entry.iter().all(|byte| *byte == 0) {
            continue;
        }
        if kind == 0xEE {
            protective += 1;
        } else {
            return Err(
                "the disk has a hybrid MBR, which lists partitions of its own: not written beside"
                    .to_string(),
            );
        }
    }
    if protective > 1 {
        return Err("the disk has more than one protective MBR entry".to_string());
    }
    Ok(())
}

/// Programs that write to what they are given.
const WRITERS: [&str; 14] = [
    "mkfs",
    "mkfs.ext2",
    "mkfs.ext3",
    "mkfs.ext4",
    "mkfs.vfat",
    "mkfs.fat",
    "mkfs.ntfs",
    "mkswap",
    "wipefs",
    "dd",
    "sgdisk",
    "parted",
    "gdisk",
    "fdisk",
];

/// Programs that may be given the whole disk or an old partition because
/// they only look, or only touch the new partition or the boot entries.
const LOOKERS: [&str; 3] = ["blkid", "udevadm", "lsblk"];

/// The rule, in code, that nothing writes to anything but the new
/// partition. `argv` is a command about to run; `chroot DIR cmd...` is
/// judged by what runs inside. `device` is the whole disk's node, `old`
/// the nodes of the partitions that were there, `esp` the one of them
/// that GRUB may be put on, `new` the partition made.
pub fn guard(
    argv: &[String],
    device: &str,
    old: &[String],
    esp: &str,
    new: Option<&str>,
    deletable: &[String],
) -> Result<(), String> {
    let argv: &[String] = match argv.first().map(String::as_str) {
        Some("chroot") => argv.get(2..).unwrap_or_default(),
        _ => argv,
    };
    let Some(program) = argv
        .first()
        .map(|program| program.rsplit('/').next().unwrap_or(""))
    else {
        return Ok(());
    };
    let args = &argv[1..];
    let names = |node: &str| args.iter().any(|arg| arg == node);
    let refuse = |why: &str| Err(format!("{} was not run: {why}", argv.join(" ")));

    // The boot sector of the whole disk belongs to whatever starts it.
    if program == "grub-install" {
        if args.iter().any(|arg| arg.contains("i386-pc")) {
            return refuse("installing beside another system leaves the disk's boot sector alone");
        }
        if args.iter().any(|arg| arg == "--removable") {
            return refuse("the fallback boot path of the EFI system partition is not ours");
        }
        if names(device) || old.iter().any(|node| names(node)) {
            return refuse("it names a disk or an old partition");
        }
    }
    if program == "sfdisk" {
        let reading = args
            .iter()
            .any(|arg| arg == "-d" || arg == "--dump" || arg == "--json");
        let appending = args.iter().any(|arg| arg == "--append" || arg == "-a");
        if !reading && !appending {
            return refuse("sfdisk may only read the table or append one partition");
        }
        if args.iter().any(|arg| arg == "--wipe") {
            // `--wipe never` is the one form that cannot erase anything.
            let at = args.iter().position(|arg| arg == "--wipe").unwrap_or(0);
            if args.get(at + 1).map(String::as_str) != Some("never") {
                return refuse("sfdisk may not wipe the disk");
            }
        }
        if args
            .iter()
            .any(|arg| arg.starts_with("--delete") || arg == "-X" || arg == "--part-type")
        {
            return refuse("sfdisk may not change old partitions");
        }
    }
    // Only the exact forms that an installation uses of the programs
    // that can change the disk or the firmware.
    match program {
        "blockdev" => {
            return refuse("the kernel is told of the new partition only, not of the whole table")
        }
        "partx" => {
            let fine = args.len() == 4
                && args[0] == "--add"
                && args[1] == "--nr"
                && args[2].chars().all(|c| c.is_ascii_digit())
                && !args[2].is_empty()
                && args[3] == device
                && new.is_some_and(|node| node.ends_with(args[2].as_str()));
            if !fine {
                return refuse("partx may only add the new partition");
            }
        }
        "efibootmgr" => {
            let words: Vec<&str> = args.iter().map(String::as_str).collect();
            let fine = match words.as_slice() {
                [] | ["-v"] => true,
                ["--quiet", "--create", "--disk", disk, "--part", number, "--label", _, "--loader", _] => {
                    *disk == device && number.chars().all(|c| c.is_ascii_digit())
                }
                ["--quiet", "--bootorder", order] => order
                    .split(',')
                    .all(|entry| entry.len() == 4 && entry.chars().all(|c| c.is_ascii_hexdigit())),
                ["--quiet", "--bootnum", number, "--delete-bootnum"] => {
                    deletable.iter().any(|entry| entry == number)
                }
                _ => false,
            };
            if !fine {
                return refuse(
                    "efibootmgr may only list, add our entry, put it first, or delete one of ours",
                );
            }
        }
        _ => {}
    }
    if WRITERS.contains(&program) || program.starts_with("mkfs") {
        // Exactly one program, and exactly the new partition.
        let fine = program == "mkfs.ext4"
            && new.is_some_and(|node| args.last().map(String::as_str) == Some(node));
        if !fine {
            return refuse("only the new partition may be formatted");
        }
        if names(device) || old.iter().any(|node| names(node)) {
            return refuse("it names a disk or an old partition");
        }
    }
    // The old partitions: looked at, and the EFI system partition
    // mounted. Never anything that writes.
    if old.iter().any(|node| names(node)) {
        let node_is_esp = |node: &String| node == esp;
        let only_esp = old.iter().filter(|node| names(node)).all(node_is_esp);
        let allowed =
            LOOKERS.contains(&program) || (only_esp && matches!(program, "mount" | "umount"));
        if !allowed {
            return refuse("it names a partition that was there before");
        }
    }
    if names(device)
        && !LOOKERS.contains(&program)
        && !matches!(program, "sfdisk" | "partx" | "efibootmgr")
    {
        return refuse("it names the whole disk");
    }
    Ok(())
}

/// The nodes of the partitions in `table`, as the kernel names them.
pub fn nodes(device: &str, table: &Table) -> Vec<String> {
    table
        .entries
        .iter()
        .map(|entry| partition_path(device, entry.number))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::disks::{Disk, Part};

    /// What `sfdisk -d` printed for a 64 GiB disk that came with
    /// Windows: an EFI system partition, a reserved one, and the system.
    pub const DUMP: &str = "\
label: gpt
label-id: F6691C2B-D8E2-4C39-8EE0-426C24613EF0
device: /dev/vda
unit: sectors
first-lba: 2048
last-lba: 134217694
sector-size: 512

/dev/vda1 : start=        2048, size=      204800, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, uuid=62516844-55D2-4917-A468-C8A0FBB8B6CD, name=\"EFI System Partition\"
/dev/vda2 : start=      206848, size=       32768, type=E3C9E316-0B5C-4DB8-817D-F92DF00215AE, uuid=E0630F21-420B-4BD5-A58E-4B7484031246, name=\"Microsoft reserved\"
/dev/vda3 : start=      239616, size=    41943040, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, uuid=DD068BA6-D1DF-4FE6-9721-13FC754E035F, name=\"Basic data partition, with a comma\"
";

    pub fn table() -> Table {
        Table::parse_dump(DUMP).unwrap()
    }

    fn beside_for(table: &Table) -> Beside {
        let region = table.free_regions()[0];
        Beside {
            start: region.start,
            sectors: region.sectors,
            esp: 1,
            table: table.snapshot(),
        }
    }

    #[test]
    fn a_dump_from_sfdisk_is_read() {
        let table = table();
        assert_eq!(table.id, "f6691c2b-d8e2-4c39-8ee0-426c24613ef0");
        assert_eq!(table.first, 2048);
        assert_eq!(table.end, 134_217_695);
        assert_eq!(table.entries.len(), 3);
        let esp = table.entry(1).unwrap();
        assert_eq!((esp.start, esp.sectors), (2048, 204_800));
        assert_eq!(esp.kind, ESP_TYPE);
        assert_eq!(esp.uuid, "62516844-55d2-4917-a468-c8a0fbb8b6cd");
        // A comma in a name does not upset the fields before it.
        assert_eq!(table.entry(3).unwrap().sectors, 41_943_040);
    }

    #[test]
    fn a_dump_of_an_nvme_disk_and_of_large_sectors_is_read() {
        let dump = "label: gpt\nlabel-id: AAAA\ndevice: /dev/nvme0n1\nunit: sectors\nfirst-lba: 6\nlast-lba: 1000\nsector-size: 4096\n\n/dev/nvme0n1p1 : start=256, size=1024, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, uuid=BBBB, name=\"x\"\n/dev/nvme0n1p4 : start=2048, size=2048, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, uuid=CCCC\n";
        let table = Table::parse_dump(dump).unwrap();
        assert_eq!(table.sector_size, 4096);
        // In sectors of 512 bytes.
        assert_eq!(table.entry(1).unwrap().start, 2048);
        assert_eq!(table.entry(1).unwrap().sectors, 8192);
        assert_eq!(table.entry(4).unwrap().start, 16_384);
        assert_eq!(table.end, 1001 * 8);
        // And written back in the disk's own.
        let region = Region {
            start: 32_768,
            sectors: 20_971_520,
        };
        let input = append_input(&table, &region, "0a1b2c3d-1111-4222-8333-444455556666");
        assert!(input.starts_with("start=4096, size=2621440, type=0FC63DAF"));
    }

    #[test]
    fn a_dump_that_is_not_understood_is_refused() {
        for text in [
            "",
            "label: dos\nlabel-id: 0x1\nfirst-lba: 1\nlast-lba: 2\n",
            &DUMP.replace("label: gpt", "label: dos"),
            &DUMP.replace("label-id: F6691C2B-D8E2-4C39-8EE0-426C24613EF0\n", ""),
            &DUMP.replace("last-lba: 134217694\n", ""),
            &DUMP.replace("sector-size: 512", "sector-size: 520"),
            &DUMP.replace("unit: sectors", "unit: bytes"),
            &DUMP.replace("start=        2048", "start=        zero"),
            &DUMP.replace("uuid=62516844-55D2-4917-A468-C8A0FBB8B6CD, ", ""),
            &format!("{DUMP}this is not a line\n"),
            &format!("{DUMP}/dev/vda3 : start=1, size=2, type=A, uuid=B\n"),
        ] {
            assert!(Table::parse_dump(text).is_err(), "{text}");
        }
    }

    #[test]
    fn free_space_is_what_no_partition_covers_and_is_big_enough() {
        let table = table();
        let regions = table.free_regions();
        // Only the stretch after the system: the gap before the first
        // partition is a mebibyte at most.
        assert_eq!(regions.len(), 1);
        let region = regions[0];
        assert_eq!(region.start, 42_182_656);
        assert_eq!(region.start % ALIGN, 0);
        assert_eq!(region.end() % ALIGN, 0);
        assert!(region.end() <= table.end);
        assert!(region.bytes() >= MIN_BYTES);
        assert_eq!(table.region_is_free(&region), Ok(()));
    }

    #[test]
    fn gaps_between_partitions_count_and_small_ones_do_not() {
        let mut table = table();
        // Move the system out: a gap of 20 GiB after the reserved
        // partition, and the rest of the disk after the system.
        table.entries[2].start = 239_616 + 41_943_040;
        let regions = table.free_regions();
        assert_eq!(regions.len(), 2, "{regions:?}");
        assert_eq!(regions[0].start, 239_616);
        assert_eq!(regions[0].sectors, 41_943_040);
        assert!(regions[1].start >= 239_616 + 2 * 41_943_040);
        // Partitions that overlap each other do not make a gap: nothing
        // is offered before the end of the one that reaches furthest.
        table.entries[2].start = 100_000;
        let regions = table.free_regions();
        assert_eq!(regions.len(), 1);
        assert!(regions[0].start >= 100_000 + 41_943_040);
        // A whole disk of nothing but one big hole.
        table.entries.clear();
        let regions = table.free_regions();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].start, 2048);
    }

    #[test]
    fn a_region_that_touches_a_partition_is_refused() {
        let table = table();
        let good = table.free_regions()[0];
        for (start, sectors, word) in [
            (good.start - ALIGN, good.sectors, "partition 3"),
            (good.start, good.sectors + ALIGN * 4096, "usable"),
            (good.start + 1, good.sectors - ALIGN, "mebibyte"),
            (good.start, good.sectors - 1, "mebibyte"),
            (good.start, ALIGN * 1024, "under"),
            (0, good.sectors, "usable"),
            (2048, 41_943_040 * 2, "partition 1"),
            (206_848, 41_943_040, "partition 2"),
        ] {
            let error = table
                .region_is_free(&Region { start, sectors })
                .unwrap_err();
            assert!(error.contains(word), "{start} {sectors}: {error}");
        }
    }

    fn disk() -> Disk {
        let part = |number: u32, start: u64, sectors: u64, kind: &str, fstype: &str| Part {
            name: format!("vda{number}"),
            number,
            start,
            size: sectors * 512,
            kind: kind.to_string(),
            uuid: [
                "62516844-55d2-4917-a468-c8a0fbb8b6cd",
                "e0630f21-420b-4bd5-a58e-4b7484031246",
                "dd068ba6-d1df-4fe6-9721-13fc754e035f",
                "4444",
            ][number as usize - 1]
                .to_string(),
            fstype: fstype.to_string(),
            label: String::new(),
        };
        Disk {
            name: "vda".to_string(),
            path: "/dev/vda".to_string(),
            size: 134_217_728 * 512,
            model: String::new(),
            serial: String::new(),
            wwn: String::new(),
            tran: String::new(),
            read_only: false,
            pttype: "gpt".to_string(),
            ptuuid: "f6691c2b-d8e2-4c39-8ee0-426c24613ef0".to_string(),
            parts: vec![
                part(1, 2048, 204_800, ESP_TYPE, "vfat"),
                part(
                    2,
                    206_848,
                    32_768,
                    "e3c9e316-0b5c-4db8-817d-f92df00215ae",
                    "",
                ),
                part(
                    3,
                    239_616,
                    41_943_040,
                    "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7",
                    "ntfs",
                ),
            ],
            fstype: String::new(),
            label: String::new(),
            mounted: Vec::new(),
            swap: false,
            held: false,
            medium: false,
        }
    }

    #[test]
    fn a_disk_with_another_system_and_room_is_offered() {
        let offer = offer(&disk(), true).unwrap();
        assert_eq!(offer.esp, 1);
        assert_eq!(offer.regions.len(), 1);
        assert_eq!(offer.region().start, 42_182_656);
        assert!(offer
            .snapshot
            .starts_with("gpt f6691c2b-d8e2-4c39-8ee0-426c24613ef0;1:2048:204800:c12a7328"));
    }

    #[test]
    fn the_biggest_free_stretch_is_the_one_offered() {
        let mut disk = disk();
        // The system shrinks to 12 GiB, a small partition follows after
        // another 12 GiB of room, and the rest of the disk is free.
        disk.parts[2].size = 25_165_824 * 512;
        let mut tail = disk.parts[2].clone();
        tail.number = 4;
        tail.start = 25_405_440 + 25_165_824;
        tail.size = 2048 * 512;
        disk.parts.push(tail);
        let offer = offer(&disk, true).unwrap();
        assert_eq!(offer.regions.len(), 2);
        // The bigger one, after the small partition, is the one offered.
        assert_eq!(offer.region().start, 25_405_440 + 25_165_824 + 2048);
        assert_eq!(offer.regions[1].start, 25_405_440);
        assert_eq!(offer.regions[1].sectors, 25_165_824);
    }

    #[test]
    fn why_it_is_not_offered_is_said() {
        let good = disk();
        assert_eq!(offer(&good, false), Err(Why::NotUefi));
        let mut empty = good.clone();
        empty.parts.clear();
        assert_eq!(offer(&empty, true), Err(Why::NoSystem));
        let mut mbr = good.clone();
        mbr.pttype = "dos".to_string();
        assert_eq!(offer(&mbr, true), Err(Why::NotGpt));
        let mut none = good.clone();
        none.pttype = String::new();
        assert_eq!(offer(&none, true), Err(Why::NotGpt));
        let mut unknown = good.clone();
        unknown.parts[1].uuid = String::new();
        assert_eq!(offer(&unknown, true), Err(Why::Unreadable));
        let mut no_esp = good.clone();
        no_esp.parts[0].kind = "0fc63daf-8483-4772-8e79-3d69d8477de4".to_string();
        assert_eq!(offer(&no_esp, true), Err(Why::NoEsp));
        let mut ntfs_esp = good.clone();
        ntfs_esp.parts[0].fstype = "ntfs".to_string();
        assert_eq!(offer(&ntfs_esp, true), Err(Why::EspNotFat));
        let mut full = good.clone();
        full.parts[2].size = 134_000_000 * 512;
        assert_eq!(offer(&full, true), Err(Why::NoRoom));
        let mut small = good;
        small.parts[2].size = (134_217_728 - 239_616 - 8_000_000) * 512;
        assert_eq!(offer(&small, true), Err(Why::NoRoom));
        for why in [
            Why::NotUefi,
            Why::NoSystem,
            Why::NotGpt,
            Why::Unreadable,
            Why::NoEsp,
            Why::EspNotFat,
            Why::NoRoom,
        ] {
            assert!(!why.words().is_empty());
        }
        assert!(Why::NoRoom
            .words()
            .contains("Shrink the other system first"));
    }

    #[test]
    fn the_window_and_root_see_one_table() {
        // What the window put in the plan is what root reads.
        let offer = offer(&disk(), true).unwrap();
        assert_eq!(offer.snapshot, table().snapshot());
    }

    #[test]
    fn a_table_that_changed_since_it_was_shown_stops_everything() {
        let before = table();
        let beside = beside_for(&before);
        assert_eq!(check_before(&before, &beside), Ok(()));
        let mut moved = before.clone();
        moved.entries[2].sectors += ALIGN;
        assert!(check_before(&moved, &beside)
            .unwrap_err()
            .contains("changed"));
        let mut other_id = before.clone();
        other_id.id = "something else".to_string();
        assert!(check_before(&other_id, &beside).is_err());
        let mut added = before.clone();
        added.entries.push(Entry {
            number: 4,
            start: beside.start + ALIGN,
            sectors: ALIGN,
            kind: LINUX_TYPE.to_string(),
            uuid: "x".to_string(),
            name: String::new(),
            attrs: String::new(),
        });
        assert!(check_before(&added, &beside).is_err());
        let mut retyped = before.clone();
        retyped.entries[0].kind = LINUX_TYPE.to_string();
        assert!(check_before(&retyped, &beside).is_err());
        // The snapshot being right is not enough when the plan names a
        // partition that is not the EFI system partition.
        let wrong = Beside {
            esp: 3,
            ..beside.clone()
        };
        assert!(check_before(&before, &wrong).unwrap_err().contains("EFI"));
        let missing = Beside { esp: 9, ..beside };
        assert!(check_before(&before, &missing).is_err());
    }

    #[test]
    fn a_region_that_is_not_free_is_refused_even_with_the_right_table() {
        let before = table();
        let mut beside = beside_for(&before);
        beside.start = 206_848;
        assert!(check_before(&before, &beside)
            .unwrap_err()
            .contains("not free"));
    }

    fn written(before: &Table, beside: &Beside) -> Table {
        let mut after = before.clone();
        after.entries.push(Entry {
            number: 4,
            start: beside.start,
            sectors: beside.sectors,
            kind: LINUX_TYPE.to_string(),
            uuid: "new".to_string(),
            name: String::new(),
            attrs: String::new(),
        });
        after
    }

    #[test]
    fn what_was_written_is_checked_against_what_there_was() {
        let before = table();
        let beside = beside_for(&before);
        let after = written(&before, &beside);
        assert_eq!(check_after(&before, &after, &beside, "new"), Ok(4));

        // An old partition changed in any way at all.
        for change in [
            |entry: &mut Entry| entry.start += 1,
            |entry: &mut Entry| entry.sectors -= ALIGN,
            |entry: &mut Entry| entry.kind = LINUX_TYPE.to_string(),
            |entry: &mut Entry| entry.uuid = "other".to_string(),
        ] {
            for index in 0..3 {
                let mut bad = after.clone();
                change(&mut bad.entries[index]);
                let error = check_after(&before, &bad, &beside, "new").unwrap_err();
                assert!(error.contains("not as it was"), "{error}");
            }
        }
        // An old partition gone.
        let mut gone = after.clone();
        gone.entries.remove(1);
        assert!(check_after(&before, &gone, &beside, "new").is_err());
        // The disk has another identifier.
        let mut renamed = after.clone();
        renamed.id = "new id".to_string();
        assert!(check_after(&before, &renamed, &beside, "new").is_err());
        // Nothing was added, or two things.
        assert!(check_after(&before, &before, &beside, "new")
            .unwrap_err()
            .contains("0 new"));
        let mut two = after.clone();
        two.entries.push(Entry {
            number: 5,
            start: 1,
            sectors: 1,
            kind: LINUX_TYPE.to_string(),
            uuid: "y".to_string(),
            name: String::new(),
            attrs: String::new(),
        });
        assert!(check_after(&before, &two, &beside, "new")
            .unwrap_err()
            .contains("2 new"));
        // The new one is somewhere else, or bigger, or of another type.
        for change in [
            |entry: &mut Entry| entry.start += ALIGN,
            |entry: &mut Entry| entry.sectors += ALIGN,
            |entry: &mut Entry| entry.kind = ESP_TYPE.to_string(),
        ] {
            let mut bad = after.clone();
            change(bad.entries.last_mut().unwrap());
            assert!(check_after(&before, &bad, &beside, "new")
                .unwrap_err()
                .contains("asked for"));
        }
    }

    #[test]
    fn the_kernels_view_has_the_old_partitions_and_the_new_one() {
        let before = table();
        let region = beside_for(&before).region();
        let mut seen: Vec<(u32, u64, u64)> = before
            .entries
            .iter()
            .map(|entry| (entry.number, entry.start, entry.sectors))
            .collect();
        // Not yet told about the new one.
        assert!(check_kernel(&before, &seen, &region, 4).is_err());
        seen.push((4, region.start, region.sectors));
        assert_eq!(check_kernel(&before, &seen, &region, 4), Ok(()));
        seen.reverse();
        assert_eq!(check_kernel(&before, &seen, &region, 4), Ok(()));
        // An old one moved, or an extra.
        let mut moved = seen.clone();
        moved[0].1 += 1;
        assert!(check_kernel(&before, &moved, &region, 4).is_err());
        let mut extra = seen.clone();
        extra.push((9, 1, 1));
        assert!(check_kernel(&before, &extra, &region, 4).is_err());
        assert!(check_kernel(&before, &[], &region, 4).is_err());
    }

    #[test]
    fn sfdisk_is_given_one_line_for_the_new_partition() {
        let table = table();
        let region = beside_for(&table).region();
        let input = append_input(&table, &region, "0a1b2c3d-1111-4222-8333-444455556666");
        assert_eq!(input.lines().count(), 1);
        assert_eq!(
            input,
            format!(
                "start={}, size={}, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, uuid=0A1B2C3D-1111-4222-8333-444455556666, name=\"Sengi OS\"\n",
                region.start, region.sectors
            )
        );
        assert!(!input.contains("label"));
    }

    fn argv(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    fn judged(line: &str) -> Result<(), String> {
        let old: Vec<String> = ["/dev/vda1", "/dev/vda2", "/dev/vda3"]
            .map(String::from)
            .to_vec();
        guard(
            &argv(line),
            "/dev/vda",
            &old,
            "/dev/vda1",
            Some("/dev/vda4"),
            &["0003".to_string()],
        )
    }

    #[test]
    fn nothing_that_writes_may_name_anything_but_the_new_partition() {
        // What an installation runs.
        for line in [
            "sfdisk -d /dev/vda",
            "sfdisk --lock --no-reread --no-tell-kernel --wipe never --wipe-partitions always --append /dev/vda",
            "partx --add --nr 4 /dev/vda",
            "mkfs.ext4 -q -F -L sengi-os /dev/vda4",
            "blkid -s UUID -o value /dev/vda1",
            "blkid -s TYPE -o value /dev/vda1",
            "mount -t vfat -o ro /dev/vda1 /run/x",
            "mount -t vfat -o umask=0077 /dev/vda1 /run/x/boot/efi",
            "mount -t ext4 /dev/vda4 /run/x",
            "umount /run/x/boot/efi",
            "chroot /run/x grub-install --target=x86_64-efi --efi-directory=/boot/efi --boot-directory=/boot --bootloader-id=sengios --no-nvram --no-uefi-secure-boot",
            "chroot /run/x update-grub",
            "efibootmgr --quiet --create --disk /dev/vda --part 1 --label Sengi --loader x",
            "efibootmgr -v",
            "efibootmgr",
            "efibootmgr --quiet --bootorder 0001,0000",
            "efibootmgr --quiet --bootnum 0003 --delete-bootnum",
        ] {
            assert_eq!(judged(line), Ok(()), "{line}");
        }
        // What would destroy something.
        for line in [
            "blockdev --rereadpt /dev/vda",
            "blockdev --rereadpt /dev/vda3",
            "partx --delete --nr 1 /dev/vda",
            "partx --add --nr 3 /dev/vda",
            "partx --add /dev/vda",
            "efibootmgr --quiet --bootnum 0000 --delete-bootnum",
            "efibootmgr --delete-bootnum",
            "efibootmgr --quiet --bootorder zzzz",
            "efibootmgr --quiet --create --disk /dev/sdb --part 1 --label Sengi --loader x",
            "efibootmgr --quiet --bootnum 0003",
            "mkfs.ext4 -q -F /dev/vda3",
            "mkfs.ext4 -q -F /dev/vda1",
            "mkfs.vfat -F 32 /dev/vda1",
            "mkfs.vfat -F 32 /dev/vda4",
            "mkfs.ext4 -q -F /dev/vda",
            "mkfs.ext4 -q -F /dev/vda4 /dev/vda3",
            "mkfs.btrfs /dev/vda4",
            "wipefs --all /dev/vda3",
            "wipefs --all /dev/vda4",
            "dd if=/dev/zero of=/dev/vda3",
            "sgdisk --zap-all /dev/vda",
            "parted /dev/vda rm 3",
            "fdisk /dev/vda",
            "sfdisk /dev/vda",
            "sfdisk --lock --wipe always --wipe-partitions always /dev/vda",
            "sfdisk --delete /dev/vda 3",
            "sfdisk --part-type /dev/vda 3 83",
            "chroot /run/x grub-install --target=i386-pc --boot-directory=/boot /dev/vda",
            "chroot /run/x grub-install --target=x86_64-efi --removable --efi-directory=/boot/efi",
            "chroot /run/x grub-install --target=x86_64-efi /dev/vda",
            "mount -t ntfs /dev/vda3 /run/x",
            "umount /dev/vda3",
            "cp /dev/zero /dev/vda3",
            "chroot /run/x mkfs.ext4 /dev/vda3",
        ] {
            assert!(judged(line).is_err(), "{line} was allowed");
        }
    }

    #[test]
    fn a_guard_without_a_new_partition_yet_allows_no_formatting_at_all() {
        let old = vec!["/dev/vda1".to_string()];
        assert!(guard(
            &argv("mkfs.ext4 /dev/vda2"),
            "/dev/vda",
            &old,
            "/dev/vda1",
            None,
            &[]
        )
        .is_err());
    }

    #[test]
    fn the_nodes_follow_the_kernels_naming() {
        let table = table();
        assert_eq!(nodes("/dev/vda", &table)[2], "/dev/vda3");
        assert_eq!(nodes("/dev/nvme0n1", &table)[0], "/dev/nvme0n1p1");
    }
    #[test]
    fn secure_boot_is_read_from_the_variable() {
        assert!(secure_boot_from(&[6, 0, 0, 0, 1]));
        assert!(!secure_boot_from(&[6, 0, 0, 0, 0]));
        assert!(!secure_boot_from(&[]));
        assert!(!secure_boot_from(&[1]));
    }

    #[test]
    fn the_new_partition_gets_an_identifier_that_was_chosen_before() {
        let uuid = make_uuid("0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(uuid.len(), 36);
        assert_eq!(&uuid[14..15], "4");
        assert!("89ab".contains(&uuid[19..20]));
        assert_eq!(make_uuid("short"), None);
        assert_eq!(make_uuid(&"z".repeat(32)), None);
        // A partition that has another identifier is not the one asked for.
        let before = table();
        let beside = beside_for(&before);
        let after = written(&before, &beside);
        assert_eq!(check_after(&before, &after, &beside, "new"), Ok(4));
        assert!(check_after(&before, &after, &beside, "someone else")
            .unwrap_err()
            .contains("asked for"));
    }

    #[test]
    fn names_and_attributes_are_read_and_a_change_in_them_is_seen() {
        let dump = DUMP.replace(
            "name=\"EFI System Partition\"",
            "name=\"EFI System Partition\", attrs=\"RequiredPartition GUID:63\"",
        );
        let table = Table::parse_dump(&dump).unwrap();
        assert_eq!(table.entry(1).unwrap().name, "EFI System Partition");
        assert_eq!(table.entry(1).unwrap().attrs, "RequiredPartition GUID:63");
        assert_eq!(table.entry(3).unwrap().attrs, "");
        // The same snapshot as the window\'s, which does not know them.
        assert_eq!(table.snapshot(), super::tests::table().snapshot());
        let beside = beside_for(&table);
        let after = written(&table, &beside);
        let mut flagged = after.clone();
        flagged.entries[0].attrs = String::new();
        assert!(check_after(&table, &flagged, &beside, "new")
            .unwrap_err()
            .contains("not as it was"));
        let mut renamed = after;
        renamed.entries[1].name = "Recovery".to_string();
        assert!(check_after(&table, &renamed, &beside, "new").is_err());
    }

    #[test]
    fn only_a_plain_protective_mbr_is_written_beside() {
        let mut sector = vec![0u8; 512];
        sector[510] = 0x55;
        sector[511] = 0xaa;
        assert_eq!(mbr_is_plain(&sector), Ok(()));
        // The protective entry of a GPT disk.
        sector[446 + 4] = 0xee;
        sector[446 + 8] = 1;
        assert_eq!(mbr_is_plain(&sector), Ok(()));
        // A hybrid MBR lists a partition of its own.
        sector[462 + 4] = 0x07;
        sector[462 + 8] = 9;
        assert!(mbr_is_plain(&sector).unwrap_err().contains("hybrid"));
        // Two protective entries, or a short read.
        let mut two = vec![0u8; 512];
        two[446 + 4] = 0xee;
        two[446 + 8] = 1;
        two[462 + 4] = 0xee;
        two[462 + 8] = 1;
        assert!(mbr_is_plain(&two).is_err());
        assert!(mbr_is_plain(&[0u8; 100]).is_err());
    }

    #[test]
    fn secure_boot_and_several_system_partitions_are_not_offered() {
        let good = disk();
        let on = Firmware {
            uefi: true,
            secure_boot: true,
        };
        assert_eq!(offer(&good, on), Err(Why::SecureBoot));
        assert!(Why::SecureBoot.words().contains("Turn it off"));
        let mut two = good.clone();
        let mut second = two.parts[0].clone();
        second.number = 4;
        second.start = 99_999_999;
        two.parts.push(second);
        assert_eq!(offer(&two, true), Err(Why::SeveralEsp));
        assert!(!Why::SeveralEsp.words().is_empty());
        // Secure Boot only matters under UEFI: BIOS is said first.
        let bios = Firmware {
            uefi: false,
            secure_boot: true,
        };
        assert_eq!(offer(&good, bios), Err(Why::NotUefi));
    }
}
