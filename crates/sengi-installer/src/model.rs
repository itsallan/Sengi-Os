//! What the window shows, worked out from what was typed and what the
//! machine has. Nothing here knows about Slint, so it is tested as it is.

use sengi_config::settings::Layout;
use sengi_install::beside::{self, Beside, Firmware, Offer};
use sengi_install::disks::{self, Disk};
use sengi_install::install::{SHRINK_STEP, STEPS};
use sengi_install::names;
use sengi_install::plan::{Mode, Plan, LIVE_KERNEL, LIVE_ROOT};
use sengi_install::progress::{Event, Resize};
use sengi_install::shrink::{self, Analysis, Refusal};

/// The pages before the installation starts, in order.
pub const PAGES: [&str; 5] = ["welcome", "region", "disk", "you", "review"];

/// The name a new computer gets until its owner chooses one.
pub const DEFAULT_HOST_NAME: &str = "sengi";

/// The longest a disk's own name is shown.
const MAX_MODEL_CHARS: usize = 40;

/// The page after `page`, if there is one before the installation.
pub fn page_after(page: &str) -> Option<&'static str> {
    let at = PAGES.iter().position(|known| *known == page)?;
    PAGES.get(at + 1).copied()
}

pub fn page_before(page: &str) -> Option<&'static str> {
    let at = PAGES.iter().position(|known| *known == page)?;
    at.checked_sub(1).map(|before| PAGES[before])
}

/// "Step 2 of 4". The welcome page is not a step.
pub fn place(page: &str) -> String {
    match PAGES.iter().position(|known| *known == page) {
        Some(at) if at > 0 => format!("Step {at} of {}", PAGES.len() - 1),
        _ => String::new(),
    }
}

/// What a disk is called: its model, cleaned, and its size.
pub fn disk_title(disk: &Disk) -> String {
    let model = sengi_text::line(&disk.model, MAX_MODEL_CHARS);
    let model = if model.is_empty() {
        "Disk".to_string()
    } else {
        model
    };
    format!("{model} \u{b7} {}", disks::size_words(disk.size))
}

/// How a disk is attached, in the words on its box. What lsblk reports
/// as the transport is what says whether a disk is inside the computer
/// or plugged into it; the kernel's "removable" flag does not: it is set
/// for card readers and clear for most USB disks.
pub fn attached_words(tran: &str) -> String {
    match tran {
        "" => String::new(),
        "sata" | "ata" => "SATA".to_string(),
        "nvme" => "NVMe".to_string(),
        "usb" => "USB".to_string(),
        "mmc" => "SD or eMMC".to_string(),
        "virtio" => "virtual".to_string(),
        other => sengi_text::line(other, 12).to_uppercase(),
    }
}

/// The end of a disk's serial number: enough to tell two disks of one
/// model apart against the label on them. Empty if it has none.
pub fn serial_end(serial: &str) -> String {
    let serial = sengi_text::line(serial, 64);
    let count = serial.chars().count();
    if count == 0 {
        String::new()
    } else {
        let end: String = serial.chars().skip(count.saturating_sub(6)).collect();
        format!("serial \u{2026}{end}")
    }
}

/// Where a disk is and which one it is: its node, how it is attached,
/// and the end of its serial number.
pub fn disk_place(disk: &Disk) -> String {
    [
        disk.path.clone(),
        attached_words(&disk.tran),
        serial_end(&disk.serial),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" \u{b7} ")
}

/// What is on a disk now, in a line that cannot break the page.
pub fn disk_contents(disk: &Disk) -> String {
    sengi_text::line(&disks::contents_words(disk), 120)
}

/// The second line of a disk in the list: where it is and which, and
/// then what is on it, or why it is not offered.
pub fn disk_detail(disk: &Disk) -> String {
    match disks::refusal(disk) {
        Some(refusal) => format!("{} \u{b7} {}", disk_place(disk), refusal.words()),
        None => format!(
            "{} \u{b7} now on it: {}",
            disk_place(disk),
            disk_contents(disk)
        ),
    }
}

/// What is said, in the warning colour, about the disk that was chosen.
pub fn erase_warning(disk: &Disk) -> String {
    let contents = sengi_text::line(&disks::contents_words(disk), 120);
    let now = if contents == "Empty" {
        "It looks empty now.".to_string()
    } else {
        format!("Now on it: {contents}.")
    };
    format!(
        "Everything on {} ({}) will be erased and cannot be brought back. {now}",
        disk_title(disk),
        disk.path
    )
}

/// The one sentence of the question before the disk is erased: which
/// disk, by everything that tells it from another, and what it holds.
pub fn erase_question(disk: &Disk) -> String {
    let contents = disk_contents(disk);
    let holds = if contents == "Empty" {
        "which looks empty".to_string()
    } else {
        format!("which now holds {contents}")
    };
    format!(
        "This erases {} ({}), {holds}, and cannot be undone.",
        disk_title(disk),
        disk_place(disk)
    )
}

/// The two ways of using a disk, as the rows of a list: what each is, and
/// whether it can be chosen on this disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeRow {
    pub id: &'static str,
    pub title: String,
    pub detail: String,
    pub enabled: bool,
    pub checked: bool,
}

pub const ERASE: &str = "erase";
pub const BESIDE: &str = "beside";
pub const SHRINK: &str = "shrink";

/// What installing beside another system would use on `disk`, or why it
/// cannot be offered.
/// What the person is told, in these words and no other, wherever the
/// way of installing beside another system is described: what it adds, and
/// that nothing of the other system is touched.
pub const BESIDE_CHANGES: &str = "adds one partition in the free space, adds a Sengi OS folder to the EFI system partition, and puts Sengi OS first in the firmware boot list. Your other files are not touched.";

pub fn beside_offer(disk: &Disk, firmware: impl Into<Firmware>) -> Result<Offer, beside::Why> {
    beside::offer(disk, firmware)
}

/// The plan's part for an offer.
pub fn beside_of(offer: &Offer) -> Beside {
    let region = offer.region();
    Beside {
        start: region.start,
        sectors: region.sectors,
        esp: offer.esp,
        table: offer.snapshot.clone(),
        shrink: None,
    }
}

/// How the chosen disk is to be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Way {
    /// Not said yet.
    Unchosen,
    #[default]
    Erase,
    /// Beside what is there, in free space.
    Beside,
    /// Beside Windows, which is made smaller first.
    Shrink,
}

/// What is known about making Windows smaller on the chosen disk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Look {
    #[default]
    NotYet,
    /// sengi-install is looking, which writes nothing.
    Looking,
    Ready(Analysis),
    Refused(Refusal),
}

/// What a disk starts with when it is chosen. A disk on which Windows
/// has all the room starts with nothing chosen, so that going on cannot
/// erase a Windows somebody meant to keep; the others start with erasing,
/// which the review and its question still have to get past.
pub fn first_way(disk: &Disk, firmware: impl Into<Firmware>) -> Way {
    if beside::squeeze(disk, firmware).is_some() {
        Way::Unchosen
    } else {
        Way::Erase
    }
}

/// Why making Windows smaller is refused before root has even looked:
/// what the disk list already says.
pub fn refused_at_sight(disk: &Disk, firmware: impl Into<Firmware>) -> Option<Refusal> {
    beside::squeeze(disk, firmware)
        .filter(|part| part.fstype == "BitLocker")
        .map(|_| Refusal::BitLocker)
}

/// The two rows: erase the disk, and install beside what is there, in
/// free space or, where there is none, by making Windows smaller. The
/// second is switched off, with the reason, where it cannot be done. A
/// choice that turns out not to be possible leaves nothing chosen: erasing
/// is never picked for anyone who picked something else.
pub fn mode_rows(
    disk: &Disk,
    firmware: impl Into<Firmware>,
    way: Way,
    look: &Look,
) -> Vec<ModeRow> {
    let firmware = firmware.into();
    let offer = beside_offer(disk, firmware);
    let squeeze = beside::squeeze(disk, firmware);
    let possible_beside = offer.is_ok();
    let erase = ModeRow {
        id: ERASE,
        title: "Erase the whole disk".to_string(),
        detail: "Everything on the disk is removed and Sengi OS takes all of it.".to_string(),
        enabled: true,
        checked: way == Way::Erase || (way == Way::Beside && !possible_beside),
    };
    let beside = match (offer, squeeze) {
        (Ok(offer), _) => ModeRow {
            id: BESIDE,
            title: "Install beside the existing system".to_string(),
            detail: format!(
                "Adds one partition of {} in free space and a Sengi OS folder on the EFI system partition. Your other files are not touched.",
                disks::size_words(offer.region().bytes())
            ),
            enabled: true,
            checked: way == Way::Beside,
        },
        (Err(_), Some(_)) => {
            let refusal = match look {
                Look::Refused(refusal) => Some(refusal.clone()),
                _ => refused_at_sight(disk, firmware),
            };
            let detail = match (&refusal, look) {
                (Some(refusal), _) => refusal.short().to_string(),
                (None, Look::Looking) => "Looking at Windows\u{2026}".to_string(),
                (None, Look::Ready(analysis)) => format!(
                    "Makes Windows smaller to make room. It uses {} of its {}",
                    disks::size_words(analysis.used_bytes),
                    disks::size_words(analysis.sectors.saturating_mul(512))
                ),
                (None, _) => "Makes Windows smaller to make room. Back up your files first".to_string(),
            };
            ModeRow {
                id: SHRINK,
                title: "Install beside Windows".to_string(),
                detail,
                enabled: refusal.is_none(),
                checked: way == Way::Shrink && refusal.is_none(),
            }
        }
        (Err(why), None) => ModeRow {
            id: BESIDE,
            title: "Install beside the existing system".to_string(),
            detail: why.words(),
            enabled: false,
            checked: false,
        },
    };
    vec![erase, beside]
}

/// Whether the Disk page can be left.
pub fn disk_page_ready(chosen: bool, way: Way, look: &Look, backup: bool) -> bool {
    chosen
        && match way {
            Way::Unchosen => false,
            Way::Shrink => matches!(look, Look::Ready(_)) && backup,
            Way::Erase | Way::Beside => true,
        }
}

/// A size chosen for Sengi OS on a Windows that is made smaller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Squeeze {
    pub analysis: Analysis,
    /// What Sengi OS gets, in gigabytes.
    pub gb: u64,
}

impl Squeeze {
    /// Half of what could be taken, and at least what is needed.
    pub fn new(analysis: Analysis) -> Squeeze {
        let room = analysis.room;
        Squeeze {
            gb: (room.largest_gb / 2).clamp(room.smallest_gb, room.largest_gb),
            analysis,
        }
    }

    pub fn with(analysis: Analysis, gb: u64) -> Squeeze {
        let room = analysis.room;
        Squeeze {
            gb: gb.clamp(room.smallest_gb, room.largest_gb),
            analysis,
        }
    }

    /// The plan's part.
    pub fn beside(&self) -> Option<Beside> {
        let (shrink, region) = shrink::place(&self.analysis.entry(), self.gb)?;
        Some(Beside {
            start: region.start,
            sectors: region.sectors,
            esp: self.analysis.esp,
            table: self.analysis.table.clone(),
            shrink: Some(shrink),
        })
    }

    /// How large the Windows partition is, how large it will be, and how
    /// large the new one, in bytes.
    pub fn sizes(&self) -> (u64, u64, u64) {
        let before = self.analysis.sectors.saturating_mul(512);
        let (after, new) = shrink::place(&self.analysis.entry(), self.gb)
            .map_or((before, 0), |(shrink, region)| {
                (shrink.sectors.saturating_mul(512), region.bytes())
            });
        (before, after, new)
    }

    /// What the size control says: what Windows uses, what Sengi OS may
    /// take, what it takes, and what Windows keeps.
    pub fn words(&self) -> [String; 4] {
        let (before, after, new) = self.sizes();
        let used = self.analysis.used_bytes;
        let room = self.analysis.room;
        [
            format!(
                "{} of {}",
                disks::size_words(used),
                disks::size_words(before)
            ),
            format!("From {} GB to {} GB", room.smallest_gb, room.largest_gb),
            disks::size_words(new),
            format!(
                "{}, with {} free",
                disks::size_words(after),
                disks::size_words(after.saturating_sub(used))
            ),
        ]
    }
}

/// The warning under the size control and above the review.
pub fn shrink_note(disk: &Disk, squeeze: &Squeeze) -> String {
    let (before, after, _) = squeeze.sizes();
    format!(
        "Back up your files before you go on. Windows on {} will be made smaller, from {} to {}. If the power fails or the computer is switched off while that happens, Windows and your files can be lost.",
        disk.path,
        disks::size_words(before),
        disks::size_words(after)
    )
}

/// The question before Windows is made smaller.
pub fn shrink_question(
    disk: &Disk,
    squeeze: &Squeeze,
) -> (&'static str, String, &'static str, bool) {
    let (before, after, new) = squeeze.sizes();
    (
        "Make Windows smaller and install?",
        format!(
            "This makes partition {} of {} ({}), which holds Windows, smaller, from {} to {}, creates one partition of {} for Sengi OS in the space it gives up, adds a Sengi OS folder to the EFI system partition, and puts Sengi OS first in the firmware boot list. Keep the charger plugged in and do not switch the computer off.",
            squeeze.analysis.number,
            disk_title(disk),
            disk_place(disk),
            disks::size_words(before),
            disks::size_words(after),
            disks::size_words(new)
        ),
        "Shrink and install",
        true,
    )
}

/// The rows of the review page where Windows is made smaller: the exact
/// change in plain sizes.
pub fn review_shrink(
    form: &Form,
    disk: &Disk,
    squeeze: &Squeeze,
    keyboard: &str,
    time_zone: &str,
) -> Vec<(&'static str, String)> {
    let (before, after, new) = squeeze.sizes();
    let used = squeeze.analysis.used_bytes;
    let number = squeeze.analysis.number;
    let name = form.full_name.trim();
    vec![
        (
            "Disk",
            format!("{} ({})", disk_title(disk), disk_place(disk)),
        ),
        ("On it now", disk_contents(disk)),
        (
            "Windows",
            format!(
                "Partition {number} is made smaller, from {} to {}. It uses {} and will have {} free",
                disks::size_words(before),
                disks::size_words(after),
                disks::size_words(used),
                disks::size_words(after.saturating_sub(used))
            ),
        ),
        (
            "New partition",
            format!(
                "{} for Sengi OS, in the space Windows gives up, not encrypted",
                disks::size_words(new)
            ),
        ),
        (
            "Not changed",
            "The other partitions. A Sengi OS folder, EFI/sengios, is added to the EFI system partition, and Sengi OS is put first in the firmware boot list".to_string(),
        ),
        (
            "Backup",
            "You said you have one".to_string(),
        ),
        (
            "Next Windows start",
            "Windows checks its disk first. That is expected: let it finish".to_string(),
        ),
        (
            "Good to know",
            "If the other system uses BitLocker or a TPM, it may ask for its recovery key after the boot list changes. Have the key at hand.".to_string(),
        ),
        (
            "Your account",
            if name.is_empty() {
                form.user_name.clone()
            } else {
                format!("{name} ({})", form.user_name)
            },
        ),
        ("Computer name", form.host_name.clone()),
        ("Keyboard layout", keyboard.to_string()),
        ("Time zone", time_zone.replace('_', " ")),
    ]
}

/// The note under the choice and above the review: in the warning colour
/// where something is erased. The second is whether it warns.
pub fn change_note(disk: &Disk, offer: Option<&Offer>) -> (String, bool) {
    match offer {
        None => (erase_warning(disk), true),
        Some(offer) => (
            format!(
                "Sengi OS will be added beside what is on {} ({}), in {} of free space. It {BESIDE_CHANGES}",
                disk_title(disk),
                disk.path,
                disks::size_words(offer.region().bytes())
            ),
            false,
        ),
    }
}

/// The question that is asked before anything is written: its title, its
/// sentence, the button that says yes, and whether that button is a
/// dangerous one.
pub fn question(disk: &Disk, offer: Option<&Offer>) -> (&'static str, String, &'static str, bool) {
    match offer {
        None => ("Erase this disk?", erase_question(disk), "Erase and install", true),
        Some(offer) => (
            "Install beside the existing system?",
            format!(
                "This creates one partition of {} in free space on {} ({}), adds a Sengi OS folder to the EFI system partition, and puts Sengi OS first in the firmware boot list. Your other files are not touched.",
                disks::size_words(offer.region().bytes()),
                disk_title(disk),
                disk_place(disk)
            ),
            "Install",
            false,
        ),
    }
}

/// What the list says above it.
pub fn disk_note(disks: &[Disk]) -> &'static str {
    if disks.is_empty() {
        "No disk was found. If the computer has one, it may need a driver Sengi OS does not have."
    } else if disks.iter().all(|disk| disks::refusal(disk).is_some()) {
        "None of these disks can be installed on. Sengi OS needs a disk of at least 8 GB that is not in use, and it does not install on the stick it is running from."
    } else {
        "Choose the disk to install on. Then say whether to erase it, or to install beside what is on it."
    }
}

/// What was typed on the You page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form {
    pub full_name: String,
    pub user_name: String,
    pub host_name: String,
    /// The user name was typed, not offered: it is left alone from then.
    pub user_name_typed: bool,
}

/// What is wrong with each field, in words, or nothing. A field nobody
/// has typed into yet is not complained about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Problems {
    pub full_name: String,
    pub user_name: String,
    pub host_name: String,
    pub password: String,
}

/// The two password fields, as far as the window may know them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Typed {
    pub password_chars: usize,
    pub again_chars: usize,
    pub same: bool,
}

impl Form {
    /// The full name changed: until a user name has been typed, one is
    /// offered from it.
    pub fn set_full_name(&mut self, name: &str) {
        self.full_name = name.to_string();
        if !self.user_name_typed {
            self.user_name = names::suggest_user_name(name);
        }
    }

    pub fn set_user_name(&mut self, name: &str) {
        self.user_name = name.to_string();
        self.user_name_typed = !name.is_empty();
    }

    /// `passwd` and `group` are the system's, for the names it has taken.
    pub fn problems(&self, typed: Typed, passwd: &str, group: &str) -> Problems {
        let shown = |text: &str, result: Result<(), String>| {
            if text.is_empty() {
                String::new()
            } else {
                result.err().unwrap_or_default()
            }
        };
        let user_name = names::user_name(&self.user_name).and_then(|()| {
            if names::taken(&self.user_name, passwd, group) {
                Err(format!(
                    "\u{201c}{}\u{201d} is already used by the system",
                    self.user_name
                ))
            } else {
                Ok(())
            }
        });
        Problems {
            full_name: shown(&self.full_name, names::full_name(self.full_name.trim())),
            user_name: shown(&self.user_name, user_name),
            host_name: shown(&self.host_name, names::host_name(&self.host_name)),
            password: if typed.again_chars > 0 && !typed.same {
                "The two do not match".to_string()
            } else {
                String::new()
            },
        }
    }

    /// Whether the page is filled in: every field right, not merely not
    /// yet wrong.
    pub fn ready(&self, typed: Typed, passwd: &str, group: &str) -> bool {
        names::full_name(self.full_name.trim()).is_ok()
            && names::user_name(&self.user_name).is_ok()
            && !names::taken(&self.user_name, passwd, group)
            && names::host_name(&self.host_name).is_ok()
            && typed.password_chars > 0
            && typed.same
    }

    /// The plan for these answers, for a whole disk.
    pub fn plan(&self, disk: &Disk, keyboard: &Layout, time_zone: &str) -> Plan {
        self.plan_with(disk, keyboard, time_zone, None)
    }

    /// The plan for these answers: beside another system where `beside`
    /// says where, and for the whole disk without it.
    pub fn plan_with(
        &self,
        disk: &Disk,
        keyboard: &Layout,
        time_zone: &str,
        beside: Option<Beside>,
    ) -> Plan {
        Plan {
            device: disk.path.clone(),
            mode: if beside.is_some() {
                Mode::Beside
            } else {
                Mode::Erase
            },
            beside,
            size: disk.size,
            model: disk.model.clone(),
            serial: disk.serial.clone(),
            wwn: disk.wwn.clone(),
            user: self.user_name.clone(),
            full_name: self.full_name.trim().to_string(),
            host_name: self.host_name.clone(),
            time_zone: time_zone.to_string(),
            keyboard: keyboard.clone(),
            source_root: LIVE_ROOT.into(),
            kernel: LIVE_KERNEL.into(),
            allow_loop: false,
        }
    }
}

/// The rows of the review page for an installation beside another
/// system: exactly what is made, and what is left alone.
pub fn review_beside(
    form: &Form,
    disk: &Disk,
    offer: &Offer,
    keyboard: &str,
    time_zone: &str,
) -> Vec<(&'static str, String)> {
    let name = form.full_name.trim();
    vec![
        (
            "Disk",
            format!("{} ({})", disk_title(disk), disk_place(disk)),
        ),
        ("On it now", disk_contents(disk)),
        (
            "What changes",
            format!(
                "Creates one partition of {} in free space, adds a Sengi OS folder to the EFI system partition, and puts Sengi OS first in the firmware boot list. Your other files are not touched.",
                disks::size_words(offer.region().bytes())
            ),
        ),
        (
            "Good to know",
            "If the other system uses BitLocker or a TPM, it may ask for its recovery key after the boot list changes. Have the key at hand.".to_string(),
        ),
        (
            "Put on it",
            "Sengi OS in the new partition, not encrypted".to_string(),
        ),
        (
            "Starts with",
            "UEFI, as this computer was started. Sengi OS is tried first, and the other system stays in the boot list and in the boot menu".to_string(),
        ),
        (
            "Boot files",
            "A folder of its own, EFI/sengios, on the EFI system partition that is already there".to_string(),
        ),
        (
            "Your account",
            if name.is_empty() {
                form.user_name.clone()
            } else {
                format!("{name} ({})", form.user_name)
            },
        ),
        ("Computer name", form.host_name.clone()),
        ("Keyboard layout", keyboard.to_string()),
        ("Time zone", time_zone.replace('_', " ")),
    ]
}

/// The rows of the review page: title and value.
pub fn review(
    form: &Form,
    disk: &Disk,
    keyboard: &str,
    time_zone: &str,
    uefi: bool,
) -> Vec<(&'static str, String)> {
    let name = form.full_name.trim();
    vec![
        (
            "Disk",
            format!("{} ({})", disk_title(disk), disk_place(disk)),
        ),
        ("On it now", disk_contents(disk)),
        ("What happens to it", "All of it is erased".to_string()),
        (
            "Put on it",
            "Sengi OS on the whole disk, not encrypted".to_string(),
        ),
        (
            "Starts with",
            if uefi {
                "UEFI, as this computer was started; also set up for BIOS".to_string()
            } else {
                "BIOS, as this computer was started; also set up for UEFI".to_string()
            },
        ),
        (
            "Your account",
            if name.is_empty() {
                form.user_name.clone()
            } else {
                format!("{name} ({})", form.user_name)
            },
        ),
        ("Computer name", form.host_name.clone()),
        ("Keyboard layout", keyboard.to_string()),
        ("Time zone", time_zone.replace('_', " ")),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Waiting,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    pub id: &'static str,
    pub title: &'static str,
    pub state: StepState,
    /// While the step can tell how far it is.
    pub percent: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Running,
    /// After this many seconds.
    Done(u64),
    /// The step it stopped in, and why.
    Failed(String, String),
}

/// An installation as the window follows it: built from the events the
/// installing program reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub steps: Vec<StepRow>,
    pub warnings: Vec<String>,
    pub log: String,
    /// The disk it is installing on, as its node: `/dev/sda`.
    pub disk: String,
    /// It adds a partition beside what is there and erases nothing.
    pub beside: bool,
    /// Beside another system: the number of the partition it made, once
    /// it has.
    pub made: Option<u32>,
    /// It first makes the Windows partition of this number smaller, and
    /// how far that has got once it has begun to write.
    pub shrinks: Option<u32>,
    pub resize: Option<Resize>,
    pub outcome: Outcome,
}

impl Default for Progress {
    fn default() -> Progress {
        Progress {
            steps: STEPS
                .iter()
                .map(|step| StepRow {
                    id: step.id,
                    title: step.title,
                    state: StepState::Waiting,
                    percent: None,
                })
                .collect(),
            warnings: Vec::new(),
            log: String::new(),
            disk: String::new(),
            beside: false,
            made: None,
            shrinks: None,
            resize: None,
            outcome: Outcome::Running,
        }
    }
}

impl Progress {
    fn step(&mut self, id: &str) -> Option<&mut StepRow> {
        self.steps.iter_mut().find(|step| step.id == id)
    }

    pub fn apply(&mut self, event: Event) {
        // Once it has ended, nothing said afterwards changes how.
        if self.outcome != Outcome::Running {
            return;
        }
        match event {
            Event::Begin(id) => {
                if let Some(step) = self.step(&id) {
                    step.state = StepState::Running;
                }
            }
            Event::Percent(id, percent) => {
                if let Some(step) = self.step(&id) {
                    step.percent = Some(percent.min(100));
                }
            }
            Event::End(id) => {
                if let Some(step) = self.step(&id) {
                    step.state = StepState::Done;
                    step.percent = None;
                }
            }
            Event::Warning(text) => self.warnings.push(sengi_text::line(&text, 300)),
            Event::Log(path) => self.log = sengi_text::line(&path, 200),
            Event::Disk(path) => self.disk = sengi_text::line(&path, 64),
            Event::Beside => self.beside = true,
            Event::Made(number) => self.made = Some(number),
            Event::Shrinks(number) => {
                self.shrinks = Some(number);
                if self.step(SHRINK_STEP.id).is_none() {
                    self.steps.insert(
                        1.min(self.steps.len()),
                        StepRow {
                            id: SHRINK_STEP.id,
                            title: SHRINK_STEP.title,
                            state: StepState::Waiting,
                            percent: None,
                        },
                    );
                }
            }
            Event::Resize(stage) => self.resize = Some(stage),
            Event::Done(seconds) => self.outcome = Outcome::Done(seconds),
            Event::Failed(id, why) => {
                if let Some(step) = self.step(&id) {
                    step.state = StepState::Failed;
                    step.percent = None;
                }
                self.outcome = Outcome::Failed(id, sengi_text::line(&why, 400));
            }
        }
    }

    /// The installing program has gone. If it never said how it ended,
    /// it did not end well.
    pub fn ended(&mut self, status: &str, last_words: &str) {
        if self.outcome != Outcome::Running {
            return;
        }
        let step = self
            .steps
            .iter_mut()
            .find(|step| step.state == StepState::Running);
        let id = match step {
            Some(step) => {
                step.state = StepState::Failed;
                step.id.to_string()
            }
            None => String::new(),
        };
        let said = sengi_text::line(last_words, 300);
        let why = if said.is_empty() {
            format!("The installing program stopped without finishing ({status})")
        } else {
            format!("The installing program stopped without finishing ({status}): {said}")
        };
        self.outcome = Outcome::Failed(id, why);
    }

    /// Whether the disk had been touched when it failed. The first step
    /// only looks, and so does the step that makes Windows smaller until
    /// it says that it writes.
    pub fn disk_was_touched(&self) -> bool {
        let looking = if self.shrinks.is_some() { 2 } else { 1 };
        self.resize.is_some()
            || self
                .steps
                .iter()
                .skip(looking)
                .any(|step| step.state != StepState::Waiting)
    }

    /// Whether Windows is being changed at this moment.
    pub fn shrinking(&self) -> bool {
        self.outcome == Outcome::Running
            && matches!(
                self.resize,
                Some(Resize::Writing | Resize::FileSystem | Resize::Table)
            )
    }

    /// Where Windows is made smaller: what state it was left in, and what
    /// to do about it.
    fn shrink_left(&self) -> String {
        let disk = if self.disk.is_empty() {
            "the disk"
        } else {
            &self.disk
        };
        let number = self.shrinks.unwrap_or_default();
        match self.resize {
            None => "Nothing on the computer was changed.".to_string(),
            Some(Resize::Writing) => format!(
                "The Windows file system on partition {number} of {disk} was being made smaller and may be damaged. The partition table was not changed. Remove the USB stick, start Windows and let it check the disk; do not switch it off while it does. If Windows does not start or files are missing, restore your backup. Do not run this installer on this disk again before Windows has finished its check."
            ),
            Some(Resize::FileSystem) => format!(
                "The Windows file system was made smaller, and partition {number} of {disk} was not changed; the two agree. Windows should start, and will check the disk first, which is expected: let it finish. To give it the room back, in Windows open Disk Management, right-click the Windows drive and choose Extend Volume."
            ),
            Some(Resize::Table) => format!(
                "The Windows file system was made smaller, and the partition table was being changed when this stopped. Start Windows; it checks the disk first, which is expected. If it does not start, do not format anything: the record named below holds the table of {disk} as it was, for whoever repairs it."
            ),
            Some(Resize::Done) => {
                let after = "Windows was made smaller and checked: it keeps its files and its new size, and checks the disk at its next start, which is expected.";
                if self.made.is_some() || self.step_started("partition") {
                    format!("{after} {}", self.beside_left())
                } else {
                    format!("{after} The space it gave up is free: this installer can now install in it. This live session is as it was.")
                }
            }
        }
    }

    fn step_started(&self, id: &str) -> bool {
        self.steps
            .iter()
            .any(|step| step.id == id && step.state != StepState::Waiting)
    }

    /// The step it stopped in, by the words the list shows it with.
    pub fn failed_step(&self) -> Option<&'static str> {
        self.steps
            .iter()
            .find(|step| step.state == StepState::Failed)
            .map(|step| step.title)
    }

    /// Beside another system: what is on the disk now that it failed, and
    /// how to take the new partition off again. The partition is not removed
    /// by the installer: deleting a partition is the one thing it is built
    /// never to do, and a person can see what they are removing.
    fn beside_left(&self) -> String {
        let step_started = |id: &str| self.step_started(id);
        let disk = if self.disk.is_empty() {
            "the disk".to_string()
        } else {
            self.disk.clone()
        };
        let mut said = Vec::new();
        let mut remove = String::new();
        match self.made {
            Some(number) => {
                let node = sengi_install::disks::partition_path(&disk, number);
                said.push(format!(
                    "The new partition, number {number} of {disk} ({node}), exists in the free space."
                ));
                remove = format!(
                    " To take it off again, start the computer from the live stick, open a terminal and type: sudo sfdisk --delete {disk} {number} \u{2014} after looking at lsblk {disk} to be sure that {node} is the new one, the last in the list. Until you do, this installer will not offer to install beside here, since the new partition fills the free space."
                );
            }
            None if step_started("partition") => {
                said.push(format!(
                    "A new partition may have been added to the free space of {disk}: look at lsblk {disk}, and remove it with sudo sfdisk --delete {disk} NUMBER if it is there and not wanted."
                ));
            }
            None => {}
        }
        let loader_done = self
            .steps
            .iter()
            .any(|step| step.id == "boot-loader" && step.state == StepState::Done);
        if loader_done {
            said.push(
                "The Sengi OS folder EFI/sengios on the EFI system partition and a Sengi OS entry in the firmware's boot list were made, and are still there."
                    .to_string(),
            );
        } else if step_started("boot-loader") {
            said.push(
                "A Sengi OS folder and a boot entry in the firmware may have been added to the EFI system partition and the firmware. What the installer could take out again is said above; anything not said there is still there."
                    .to_string(),
            );
        }
        if step_started("format") {
            said.push(if self.shrinks.is_some() {
                "The other partitions were checked before the new one was formatted and had not changed since Windows was made smaller."
                    .to_string()
            } else {
                "Your other partitions were checked before the new one was formatted and were as they had been."
                    .to_string()
            });
        }
        said.push("This live session is as it was.".to_string());
        format!("{}{remove}", said.join(" "))
    }

    /// What the failure page says: on which disk, in which step, why,
    /// and what that leaves. `disk` is what to call the disk; its node
    /// is added if that does not already have it.
    pub fn failure_words(&self, disk: &str) -> String {
        let Outcome::Failed(_, why) = &self.outcome else {
            return String::new();
        };
        let on = match (disk.is_empty(), self.disk.is_empty()) {
            (true, true) => "the disk".to_string(),
            (true, false) => self.disk.clone(),
            (false, true) => disk.to_string(),
            (false, false) if disk.contains(&self.disk) => disk.to_string(),
            (false, false) => format!("{disk} ({})", self.disk),
        };
        let at = match self.failed_step() {
            Some(step) => format!("Installing on {on} stopped at \u{201c}{step}\u{201d}"),
            None => format!("Installing on {on} did not start"),
        };
        let left: String = if self.shrinks.is_some() && self.resize.is_some() {
            self.shrink_left()
        } else if self.disk_was_touched() && self.beside {
            self.beside_left()
        } else if self.disk_was_touched() {
            "The disk was being written and no longer holds what it did. This live session is as it was, and the installer can be run again.".to_string()
        } else {
            "Nothing on the computer was changed.".to_string()
        };
        format!("{at}: {why}. {left}")
    }
}

/// How long it took, for people.
pub fn duration_words(seconds: u64) -> String {
    match (seconds / 60, seconds % 60) {
        (0, 1) => "1 second".to_string(),
        (0, seconds) => format!("{seconds} seconds"),
        (1, 0) => "1 minute".to_string(),
        (minutes, 0) => format!("{minutes} minutes"),
        (minutes, seconds) => format!("{minutes} min {seconds} s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str =
        "root:x:0:0:root:/root:/bin/bash\nlive:x:1000:1000::/home/live:/bin/bash\n";
    const GROUP: &str = "audio:x:29:live\n";

    fn disk() -> Disk {
        Disk {
            name: "sda".to_string(),
            path: "/dev/sda".to_string(),
            size: 256_060_514_304,
            model: "SanDisk SD8SN8U".to_string(),
            serial: String::new(),
            wwn: String::new(),
            tran: String::new(),
            read_only: false,
            pttype: String::new(),
            ptuuid: String::new(),
            parts: Vec::new(),
            fstype: String::new(),
            label: String::new(),
            mounted: Vec::new(),
            swap: false,
            held: false,
            medium: false,
        }
    }

    /// The same disk with Windows on it: an EFI system partition and a
    /// system of 100 GB, and the rest of the 256 GB free.
    fn windows_disk() -> Disk {
        let part = |number: u32, start: u64, bytes: u64, kind: &str, fstype: &str| {
            sengi_install::disks::Part {
                name: format!("sda{number}"),
                number,
                start,
                size: bytes,
                kind: kind.to_string(),
                uuid: format!("uuid-{number}"),
                fstype: fstype.to_string(),
                label: String::new(),
            }
        };
        let mut disk = disk();
        disk.pttype = "gpt".to_string();
        disk.ptuuid = "f6691c2b-d8e2-4c39-8ee0-426c24613ef0".to_string();
        disk.parts = vec![
            part(
                1,
                2048,
                104_857_600,
                "c12a7328-f81f-11d2-ba4b-00a0c93ec93b",
                "vfat",
            ),
            part(
                2,
                206_848,
                100_000_000_000,
                "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7",
                "ntfs",
            ),
        ];
        disk
    }

    fn typed(password: usize, again: usize, same: bool) -> Typed {
        Typed {
            password_chars: password,
            again_chars: again,
            same,
        }
    }

    fn filled() -> Form {
        let mut form = Form {
            host_name: "zenbook".to_string(),
            ..Form::default()
        };
        form.set_full_name("Allan Smith");
        form
    }

    #[test]
    fn the_pages_come_in_order() {
        assert_eq!(page_after("welcome"), Some("region"));
        assert_eq!(page_after("you"), Some("review"));
        assert_eq!(page_after("review"), None);
        assert_eq!(page_after("progress"), None);
        assert_eq!(page_before("region"), Some("welcome"));
        assert_eq!(page_before("welcome"), None);
        assert_eq!(place("welcome"), "");
        assert_eq!(place("region"), "Step 1 of 4");
        assert_eq!(place("review"), "Step 4 of 4");
        assert_eq!(place("progress"), "");
    }

    #[test]
    fn a_user_name_is_offered_until_one_is_typed() {
        let mut form = Form::default();
        form.set_full_name("Allan Smith");
        assert_eq!(form.user_name, "allan");
        form.set_full_name("Ada Lovelace");
        assert_eq!(form.user_name, "ada");
        form.set_user_name("countess");
        form.set_full_name("Ada King");
        assert_eq!(form.user_name, "countess");
        // Emptied again, it is offered again.
        form.set_user_name("");
        form.set_full_name("Ada King");
        assert_eq!(form.user_name, "ada");
    }

    #[test]
    fn empty_fields_are_not_complained_about_and_are_not_ready() {
        let form = Form::default();
        let none = typed(0, 0, true);
        assert_eq!(form.problems(none, PASSWD, GROUP), Problems::default());
        assert!(!form.ready(none, PASSWD, GROUP));
    }

    #[test]
    fn what_is_wrong_is_said_under_its_field() {
        let mut form = filled();
        form.set_user_name("Allan");
        form.host_name = "Zen Book".to_string();
        form.full_name = "Smith, Allan".to_string();
        let problems = form.problems(typed(4, 3, false), PASSWD, GROUP);
        assert!(problems.user_name.contains("small letter"));
        assert!(problems.host_name.contains("small letters"));
        assert!(problems.full_name.contains("comma"));
        assert_eq!(problems.password, "The two do not match");
        assert!(!form.ready(typed(4, 3, false), PASSWD, GROUP));
        // The second field not started yet is not a mismatch.
        assert_eq!(
            form.problems(typed(4, 0, false), PASSWD, GROUP).password,
            ""
        );
    }

    #[test]
    fn names_the_system_has_are_refused() {
        let mut form = filled();
        for name in ["audio", "live", "root"] {
            form.set_user_name(name);
            let ok = typed(8, 8, true);
            assert!(
                !form.problems(ok, PASSWD, GROUP).user_name.is_empty(),
                "{name}"
            );
            assert!(!form.ready(ok, PASSWD, GROUP), "{name}");
        }
    }

    #[test]
    fn a_filled_in_page_is_ready_and_makes_a_plan_that_reads_back() {
        let form = filled();
        assert!(form.ready(typed(8, 8, true), PASSWD, GROUP));
        assert!(!form.ready(typed(8, 8, false), PASSWD, GROUP));
        assert!(!form.ready(typed(0, 0, true), PASSWD, GROUP));
        let layout = Layout::parse("fr(azerty)").unwrap();
        let plan = form.plan(&disk(), &layout, "Europe/Paris");
        assert_eq!(Plan::parse(&plan.render()), Ok(plan.clone()));
        assert_eq!(plan.device, "/dev/sda");
        assert_eq!(plan.size, 256_060_514_304);
        assert_eq!(plan.user, "allan");
        assert!(!plan.allow_loop);
        assert!(!plan.render().contains("[source]"));
    }

    #[test]
    fn a_disk_is_described_by_what_it_is_and_what_is_on_it() {
        let mut disk = disk();
        assert_eq!(disk_title(&disk), "SanDisk SD8SN8U \u{b7} 256 GB");
        assert_eq!(disk_detail(&disk), "/dev/sda \u{b7} now on it: Empty");
        assert!(erase_warning(&disk).contains("It looks empty now."));
        disk.fstype = "ntfs".to_string();
        disk.label = "Data".to_string();
        assert!(erase_warning(&disk)
            .contains("Now on it: One volume, Windows (NTFS) \u{201c}Data\u{201d}."));
        assert!(erase_warning(&disk).contains("/dev/sda"));
        disk.model = String::new();

        assert_eq!(disk_title(&disk), "Disk \u{b7} 256 GB");
        disk.medium = true;
        assert_eq!(
            disk_detail(&disk),
            "/dev/sda \u{b7} Sengi OS is running from it"
        );
        disk.tran = "usb".to_string();
        disk.serial = "4C530001230512117093 ".to_string();
        assert_eq!(
            disk_detail(&disk),
            "/dev/sda \u{b7} USB \u{b7} serial \u{2026}117093 \u{b7} Sengi OS is running from it"
        );
        disk.medium = false;
        disk.fstype = String::new();
        assert_eq!(
            disk_detail(&disk),
            "/dev/sda \u{b7} USB \u{b7} serial \u{2026}117093 \u{b7} now on it: Empty"
        );
        assert_eq!(attached_words("sata"), "SATA");
        assert_eq!(attached_words("nvme"), "NVMe");
        assert_eq!(attached_words("virtio"), "virtual");
        assert_eq!(attached_words(""), "");
        assert_eq!(attached_words("sas"), "SAS");
        assert_eq!(serial_end(""), "");
        assert_eq!(serial_end("AB12"), "serial \u{2026}AB12");
    }

    #[test]
    fn a_disks_own_name_cannot_break_the_list() {
        let mut disk = disk();
        disk.model = "Evil\u{202e}\n\tDisk \u{1b}[31m".repeat(20);
        let title = disk_title(&disk);
        assert!(!title.contains('\n') && !title.contains('\u{202e}') && !title.contains('\u{1b}'));
        assert!(title.chars().count() < 80);
    }

    #[test]
    fn the_list_says_why_there_is_nothing_to_choose() {
        assert!(disk_note(&[]).starts_with("No disk was found"));
        let mut small = disk();
        small.size = 1_000_000_000;
        assert!(disk_note(&[small.clone()]).starts_with("None of these"));
        assert!(disk_note(&[small, disk()]).starts_with("Choose the disk"));
    }

    #[test]
    fn the_review_says_what_is_erased_and_who_is_made() {
        let rows = review(
            &filled(),
            &disk(),
            "French (AZERTY)",
            "America/New_York",
            true,
        );
        let value = |title: &str| {
            rows.iter()
                .find(|(row, _)| *row == title)
                .map(|(_, value)| value.as_str())
                .unwrap()
        };
        assert_eq!(value("Disk"), "SanDisk SD8SN8U \u{b7} 256 GB (/dev/sda)");
        assert_eq!(value("On it now"), "Empty");
        assert_eq!(value("What happens to it"), "All of it is erased");
        // The question names the disk by all that tells it apart, and
        // what is on it, in one sentence.
        let mut windows = disk();
        windows.tran = "sata".to_string();
        windows.serial = "162345400999".to_string();
        windows.fstype = "ntfs".to_string();
        windows.label = "Data".to_string();
        assert_eq!(
            erase_question(&windows),
            "This erases SanDisk SD8SN8U \u{b7} 256 GB (/dev/sda \u{b7} SATA \u{b7} serial \u{2026}400999), which now holds One volume, Windows (NTFS) \u{201c}Data\u{201d}, and cannot be undone."
        );
        assert!(erase_question(&disk()).contains("(/dev/sda), which looks empty, and"));
        let with_windows = review(&filled(), &windows, "x", "UTC", true);
        assert_eq!(with_windows[1].0, "On it now");
        assert!(with_windows[1].1.contains("Windows (NTFS)"));
        assert_eq!(value("Your account"), "Allan Smith (allan)");
        assert_eq!(value("Time zone"), "America/New York");
        assert!(value("Starts with").starts_with("UEFI"));
        let bios = review(&filled(), &disk(), "x", "UTC", false);
        assert!(bios[4].1.starts_with("BIOS"));
    }

    #[test]
    fn progress_follows_what_the_installing_program_says() {
        let mut progress = Progress::default();
        assert_eq!(progress.steps.len(), STEPS.len());
        assert!(!progress.disk_was_touched());
        for line in [
            "log /var/log/sengi-install.log",
            "begin check",
            "end check",
            "begin partition",
            "end partition",
            "begin format",
            "end format",
            "begin copy",
            "percent copy 42",
        ] {
            progress.apply(Event::parse(line).unwrap());
        }
        assert_eq!(progress.log, "/var/log/sengi-install.log");
        assert_eq!(progress.steps[0].state, StepState::Done);
        assert_eq!(progress.steps[3].state, StepState::Running);
        assert_eq!(progress.steps[3].percent, Some(42));
        assert_eq!(progress.steps[4].state, StepState::Waiting);
        assert!(progress.disk_was_touched());
        progress.apply(Event::End("copy".to_string()));
        assert_eq!(progress.steps[3].percent, None);
        progress.apply(Event::Warning("no boot entry".to_string()));
        progress.apply(Event::Done(75));
        assert_eq!(progress.outcome, Outcome::Done(75));
        assert_eq!(progress.warnings, ["no boot entry"]);
        // Said after the end, nothing changes it.
        progress.apply(Event::Failed("copy".to_string(), "late".to_string()));
        assert_eq!(progress.outcome, Outcome::Done(75));
    }

    #[test]
    fn a_failure_marks_its_step_and_says_whether_the_disk_was_touched() {
        let mut early = Progress::default();
        early.apply(Event::Begin("check".to_string()));
        early.apply(Event::Failed(
            "check".to_string(),
            "the disk is too small".to_string(),
        ));
        assert_eq!(early.steps[0].state, StepState::Failed);
        assert!(!early.disk_was_touched());
        assert_eq!(
            early.outcome,
            Outcome::Failed("check".to_string(), "the disk is too small".to_string())
        );

        let mut late = Progress::default();
        for line in ["begin check", "end check", "begin partition"] {
            late.apply(Event::parse(line).unwrap());
        }
        late.apply(Event::Failed(
            "partition".to_string(),
            "sfdisk said no".to_string(),
        ));
        assert!(late.disk_was_touched());
        assert_eq!(late.failed_step(), Some("Preparing the disk"));
        late.disk = "/dev/sda".to_string();
        let words = late.failure_words("SanDisk \u{b7} 256 GB");
        assert!(words.starts_with("Installing on SanDisk \u{b7} 256 GB (/dev/sda) stopped at \u{201c}Preparing the disk\u{201d}: sfdisk said no. The disk was being written"), "{words}");
        // Opened again after the fact, the window knows only the node.
        assert!(late
            .failure_words("")
            .starts_with("Installing on /dev/sda stopped at"));
        let early_words = early.failure_words("Disk (/dev/sda)");
        assert!(
            early_words.ends_with("Nothing on the computer was changed."),
            "{early_words}"
        );
        assert!(!early_words.contains("(/dev/sda) (/dev/sda)"));
    }

    #[test]
    fn a_program_that_just_stops_has_failed() {
        let mut progress = Progress::default();
        progress.apply(Event::Begin("check".to_string()));
        progress.ended("status 1", "sudo: a password is required\n");
        assert_eq!(progress.steps[0].state, StepState::Failed);
        match &progress.outcome {
            Outcome::Failed(step, why) => {
                assert_eq!(step, "check");
                assert!(why.contains("status 1") && why.contains("a password is required"));
            }
            other => panic!("{other:?}"),
        }
        // One that said it was done is done, whatever its status.
        let mut done = Progress::default();
        done.apply(Event::Done(3));
        done.ended("status 0", "");
        assert_eq!(done.outcome, Outcome::Done(3));
        // And one that never began anything failed nowhere in particular.
        let mut never = Progress::default();
        never.ended("could not be started", "");
        assert!(matches!(&never.outcome, Outcome::Failed(step, _) if step.is_empty()));
    }

    #[test]
    fn unknown_steps_and_strange_words_do_no_harm() {
        let mut progress = Progress::default();
        progress.apply(Event::Begin("defragment".to_string()));
        progress.apply(Event::Percent("defragment".to_string(), 50));
        assert!(progress
            .steps
            .iter()
            .all(|step| step.state == StepState::Waiting));
        progress.apply(Event::Warning("a\u{202e}b\u{1b}[31m".to_string()));
        assert!(!progress.warnings[0].contains('\u{202e}'));
    }

    #[test]
    fn durations_are_said_plainly() {
        assert_eq!(duration_words(1), "1 second");
        assert_eq!(duration_words(45), "45 seconds");
        assert_eq!(duration_words(60), "1 minute");
        assert_eq!(duration_words(120), "2 minutes");
        assert_eq!(duration_words(135), "2 min 15 s");
    }
    #[test]
    fn the_second_way_of_installing_is_offered_where_it_can_be_done() {
        let disk = windows_disk();
        let rows = mode_rows(&disk, true, Way::Erase, &Look::NotYet);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, ERASE);
        assert_eq!(rows[0].title, "Erase the whole disk");
        assert!(rows[0].enabled && rows[0].checked && !rows[1].checked);
        assert_eq!(rows[1].id, BESIDE);
        assert_eq!(rows[1].title, "Install beside the existing system");
        assert!(rows[1].enabled);
        assert!(
            rows[1].detail.starts_with("Adds one partition of "),
            "{}",
            rows[1].detail
        );
        assert!(rows[1].detail.contains("Your other files are not touched."));
        // Chosen, it is the checked one.
        let chosen = mode_rows(&disk, true, Way::Beside, &Look::NotYet);
        assert!(chosen[1].checked && !chosen[0].checked);
    }

    #[test]
    fn where_it_cannot_be_done_the_row_says_why_and_cannot_be_chosen() {
        // BIOS.
        let rows = mode_rows(&windows_disk(), false, Way::Beside, &Look::NotYet);
        assert!(!rows[1].enabled && !rows[1].checked && rows[0].checked);
        assert!(rows[1].detail.contains("UEFI"));
        // An MBR disk.
        let mut mbr = windows_disk();
        mbr.pttype = "dos".to_string();
        let rows = mode_rows(&mbr, true, Way::Erase, &Look::NotYet);
        assert!(!rows[1].enabled);
        assert!(rows[1].detail.contains("GPT"));
        // No EFI system partition.
        let mut none = windows_disk();
        none.parts.remove(0);
        assert!(mode_rows(&none, true, Way::Erase, &Look::NotYet)[1]
            .detail
            .contains("EFI system partition"));
        // No room, and what fills the disk is not Windows: shrink the
        // other system first.
        let mut full = windows_disk();
        full.parts[1].size = 250_000_000_000;
        full.parts[1].fstype = "ext4".to_string();
        let rows = mode_rows(&full, true, Way::Erase, &Look::NotYet);
        assert!(!rows[1].enabled);
        assert!(rows[1]
            .detail
            .contains("Shrink the other system first, from inside it"));
        // An empty disk has nothing to install beside.
        assert!(!mode_rows(&disk(), true, Way::Erase, &Look::NotYet)[1].enabled);
        // Erasing is always there.
        assert!(rows[0].enabled);
    }

    /// A computer on which Windows has all of the disk.
    fn full_windows_disk() -> Disk {
        let mut disk = windows_disk();
        disk.parts[1].size = 250_000_000_000;
        disk
    }

    fn analysis(disk: &Disk) -> Analysis {
        let part = &disk.parts[1];
        let mut analysis = Analysis {
            table: sengi_install::beside::Table::from_disk(disk)
                .unwrap()
                .snapshot(),
            esp: 1,
            number: part.number,
            start: part.start,
            sectors: part.size / 512,
            used_bytes: 80_000_000_000,
            room: shrink::Room {
                keep_bytes: 0,
                smallest_gb: 0,
                largest_gb: 0,
            },
        };
        analysis.room = shrink::room(&analysis.entry(), analysis.used_bytes).unwrap();
        analysis
    }

    #[test]
    fn a_full_windows_disk_offers_to_make_windows_smaller() {
        let disk = full_windows_disk();
        // Nothing is chosen for anyone on such a disk, and nothing goes on
        // until somebody has.
        assert_eq!(first_way(&disk, true), Way::Unchosen);
        assert_eq!(first_way(&windows_disk(), true), Way::Erase);
        assert_eq!(first_way(&disk, false), Way::Erase);
        let rows = mode_rows(&disk, true, Way::Unchosen, &Look::NotYet);
        assert_eq!(rows[1].id, SHRINK);
        assert_eq!(rows[1].title, "Install beside Windows");
        assert!(rows[1].enabled && !rows[1].checked && !rows[0].checked);
        assert!(rows[1].detail.contains("Back up"), "{}", rows[1].detail);
        assert!(!disk_page_ready(true, Way::Unchosen, &Look::NotYet, true));
        // While it is looked at, and once it has been.
        let rows = mode_rows(&disk, true, Way::Shrink, &Look::Looking);
        assert!(rows[1].checked && !rows[0].checked);
        assert!(rows[1].detail.contains("Looking"));
        let ready = Look::Ready(analysis(&disk));
        let rows = mode_rows(&disk, true, Way::Shrink, &ready);
        assert!(rows[1].checked);
        assert!(
            rows[1].detail.ends_with("It uses 80 GB of its 250 GB"),
            "{}",
            rows[1].detail
        );
        // With free space there already, nothing is made smaller.
        assert_eq!(
            mode_rows(&windows_disk(), true, Way::Erase, &Look::NotYet)[1].id,
            BESIDE
        );
        // Not under BIOS, nor with Secure Boot on.
        let rows = mode_rows(&disk, false, Way::Erase, &Look::NotYet);
        assert!(!rows[1].enabled && rows[1].detail.contains("UEFI"));
    }

    #[test]
    fn a_refused_windows_says_why_and_erasing_is_never_chosen_instead() {
        let disk = full_windows_disk();
        let refused = Look::Refused(Refusal::Hibernated);
        let rows = mode_rows(&disk, true, Way::Shrink, &refused);
        assert!(!rows[1].enabled && !rows[1].checked);
        assert_eq!(
            rows[1].detail,
            "Windows is hibernated or Fast Startup is on"
        );
        // Nothing is checked: erasing is not picked for anyone.
        assert!(!rows[0].checked);
        assert!(!disk_page_ready(true, Way::Shrink, &refused, true));
        // BitLocker is seen without looking inside.
        let mut locked = full_windows_disk();
        locked.parts[1].fstype = "BitLocker".to_string();
        assert_eq!(first_way(&locked, true), Way::Unchosen);
        let rows = mode_rows(&locked, true, Way::Unchosen, &Look::NotYet);
        assert!(!rows[1].enabled && !rows[0].checked);
        assert_eq!(rows[1].detail, "Windows is encrypted with BitLocker");
        assert_eq!(refused_at_sight(&locked, true), Some(Refusal::BitLocker));
        assert_eq!(refused_at_sight(&disk, true), None);
    }

    #[test]
    fn the_disk_page_waits_for_the_look_and_the_backup() {
        let disk = full_windows_disk();
        let ready = Look::Ready(analysis(&disk));
        assert!(disk_page_ready(true, Way::Erase, &Look::NotYet, false));
        assert!(disk_page_ready(true, Way::Beside, &Look::NotYet, false));
        assert!(!disk_page_ready(false, Way::Erase, &Look::NotYet, true));
        for look in [Look::NotYet, Look::Looking, Look::Refused(Refusal::Dirty)] {
            assert!(!disk_page_ready(true, Way::Shrink, &look, true));
        }
        assert!(!disk_page_ready(true, Way::Shrink, &ready, false));
        assert!(disk_page_ready(true, Way::Shrink, &ready, true));
    }

    #[test]
    fn the_size_chosen_stays_in_what_may_be_chosen_and_makes_the_plan() {
        let disk = full_windows_disk();
        let analysis = analysis(&disk);
        let room = analysis.room;
        assert_eq!(room.smallest_gb, 11);
        let half = Squeeze::new(analysis.clone());
        assert_eq!(half.gb, room.largest_gb / 2);
        assert_eq!(Squeeze::with(analysis.clone(), 0).gb, room.smallest_gb);
        assert_eq!(Squeeze::with(analysis.clone(), 10_000).gb, room.largest_gb);
        let chosen = Squeeze::with(analysis.clone(), 60);
        let [uses, range, sengi, windows] = chosen.words();
        assert_eq!(uses, "80 GB of 250 GB");
        assert_eq!(range, format!("From 11 GB to {} GB", room.largest_gb));
        assert_eq!(sengi, "60 GB");
        assert_eq!(windows, "189 GB, with 109 GB free");
        let beside = chosen.beside().unwrap();
        let shrink = beside.shrink.unwrap();
        assert_eq!(shrink.partition, 2);
        assert_eq!(disk.parts[1].start + shrink.sectors, beside.start);
        let layout = Layout::parse("fr(azerty)").unwrap();
        let plan = filled().plan_with(&disk, &layout, "Europe/Paris", Some(beside));
        assert_eq!(Plan::parse(&plan.render()), Ok(plan.clone()));
        assert!(plan.render().contains("[shrink]"));
        assert!(plan.render().contains("backup = true"));
    }

    #[test]
    fn the_review_and_the_question_say_the_exact_change() {
        let disk = full_windows_disk();
        let chosen = Squeeze::with(analysis(&disk), 60);
        let rows = review_shrink(&filled(), &disk, &chosen, "x", "UTC");
        let value = |title: &str| {
            rows.iter()
                .find(|(row, _)| *row == title)
                .map(|(_, value)| value.as_str())
                .unwrap()
        };
        assert_eq!(
            value("Windows"),
            "Partition 2 is made smaller, from 250 GB to 189 GB. It uses 80 GB and will have 109 GB free"
        );
        assert!(value("New partition").starts_with("60 GB for Sengi OS"));
        assert!(value("Not changed").contains("other partitions"));
        assert!(value("Next Windows start").contains("checks its disk"));
        assert!(value("Backup").contains("You said"));
        let all: String = rows.iter().map(|(_, value)| value.as_str()).collect();
        assert!(!all.to_lowercase().contains("erase"));
        let (title, body, confirm, danger) = shrink_question(&disk, &chosen);
        assert_eq!(title, "Make Windows smaller and install?");
        assert!(body.contains("from 250 GB to 189 GB"), "{body}");
        assert!(body.contains("partition of 60 GB"));
        assert!(body.contains("charger"));
        assert_eq!(confirm, "Shrink and install");
        assert!(danger);
        let note = shrink_note(&disk, &chosen);
        assert!(note.starts_with("Back up your files"));
        assert!(note.contains("from 250 GB to 189 GB"));
    }

    /// Progress of an installation that makes room, fed `lines`.
    fn shrinking(lines: &[&str]) -> Progress {
        let mut progress = Progress::default();
        for line in ["disk /dev/sda", "beside", "shrinks 2"].iter().chain(lines) {
            progress.apply(Event::parse(line).unwrap());
        }
        progress
    }

    #[test]
    fn the_step_that_makes_room_is_listed_after_the_check() {
        let progress = shrinking(&[]);
        assert_eq!(progress.steps.len(), STEPS.len() + 1);
        assert_eq!(progress.steps[1].title, "Making Windows smaller");
        let mut twice = shrinking(&["shrinks 2"]);
        assert_eq!(twice.steps.len(), STEPS.len() + 1);
        twice.apply(Event::parse("begin shrink").unwrap());
        twice.apply(Event::parse("percent shrink 40").unwrap());
        assert_eq!(twice.steps[1].percent, Some(40));
        // Only looking is not changing.
        assert!(!twice.shrinking() && !twice.disk_was_touched());
        twice.apply(Event::parse("resize writing").unwrap());
        assert!(twice.shrinking() && twice.disk_was_touched());
        twice.apply(Event::parse("resize done").unwrap());
        assert!(!twice.shrinking());
    }

    #[test]
    fn a_failure_says_what_state_windows_and_the_disk_are_in() {
        let failed = |lines: &[&str], step: &str| {
            let mut progress = shrinking(lines);
            progress.apply(Event::Failed(step.to_string(), "why".to_string()));
            progress.failure_words("")
        };
        let check = failed(&["begin check"], "check");
        assert!(
            check.ends_with("Nothing on the computer was changed."),
            "{check}"
        );
        let looking = failed(&["begin check", "end check", "begin shrink"], "shrink");
        assert!(
            looking.ends_with("Nothing on the computer was changed."),
            "{looking}"
        );
        let before = ["begin check", "end check", "begin shrink"];
        let with = |extra: &[&str], step: &str| {
            let lines: Vec<&str> = before.iter().chain(extra).copied().collect();
            failed(&lines, step)
        };
        let writing = with(&["resize writing"], "shrink");
        assert!(writing.contains("may be damaged"), "{writing}");
        assert!(writing.contains("partition table was not changed"));
        assert!(writing.contains("restore your backup"));
        let files = with(&["resize writing", "resize files"], "shrink");
        assert!(
            files.contains("partition 2 of /dev/sda was not changed"),
            "{files}"
        );
        assert!(files.contains("check the disk first, which is expected"));
        assert!(files.contains("Extend Volume"));
        let table = with(
            &["resize writing", "resize files", "resize table"],
            "shrink",
        );
        assert!(table.contains("table was being changed"), "{table}");
        assert!(table.contains("do not format"));
        let done = [
            "resize writing",
            "resize files",
            "resize table",
            "resize done",
        ];
        let early = with(&done, "shrink");
        assert!(early.contains("made smaller and checked"), "{early}");
        assert!(early.contains("space it gave up is free"));
        let lines: Vec<&str> = done
            .iter()
            .copied()
            .chain([
                "end shrink",
                "begin partition",
                "made 3",
                "end partition",
                "begin format",
                "end format",
                "begin copy",
            ])
            .collect();
        let later = with(&lines, "copy");
        assert!(later.contains("made smaller and checked"), "{later}");
        assert!(later.contains("sudo sfdisk --delete /dev/sda 3"));
        assert!(!later.contains("Your other partitions were checked"));
        for words in [writing, files, table, early, later] {
            assert!(!words.contains("Nothing on the computer was changed"));
            assert!(!words.contains("no longer holds what it did"));
        }
    }

    #[test]
    fn the_review_beside_says_exactly_what_changes() {
        let disk = windows_disk();
        let offer = beside_offer(&disk, true).unwrap();
        let rows = review_beside(&filled(), &disk, &offer, "French (AZERTY)", "Europe/Paris");
        let value = |title: &str| {
            rows.iter()
                .find(|(row, _)| *row == title)
                .map(|(_, value)| value.as_str())
                .unwrap()
        };
        let size = sengi_install::disks::size_words(offer.region().bytes());
        assert_eq!(
            value("What changes"),
            format!("Creates one partition of {size} in free space, adds a Sengi OS folder to the EFI system partition, and puts Sengi OS first in the firmware boot list. Your other files are not touched.")
        );
        assert!(value("On it now").contains("Windows (NTFS)"));
        assert!(value("Put on it").starts_with("Sengi OS in the new partition"));
        assert!(value("Boot files").contains("EFI/sengios"));
        assert!(value("Starts with").starts_with("UEFI"));
        // Nothing in it says the disk is erased.
        assert!(!rows
            .iter()
            .any(|(_, value)| value.to_lowercase().contains("erase")));
        assert!(rows.iter().all(|(title, _)| *title != "What happens to it"));
        assert_eq!(value("Your account"), "Allan Smith (allan)");
    }

    #[test]
    fn the_question_beside_is_a_plain_one_and_the_erasing_one_is_not() {
        let disk = windows_disk();
        let offer = beside_offer(&disk, true).unwrap();
        let (title, body, confirm, danger) = question(&disk, Some(&offer));
        assert_eq!(title, "Install beside the existing system?");
        assert!(body.starts_with("This creates one partition of "));
        assert!(body.ends_with("Your other files are not touched."));
        assert_eq!(confirm, "Install");
        assert!(!danger);
        let (title, body, confirm, danger) = question(&disk, None);
        assert_eq!(title, "Erase this disk?");
        assert!(body.starts_with("This erases "));
        assert_eq!(confirm, "Erase and install");
        assert!(danger);
        // The note says it in the quiet colour, and the erasing one warns.
        let (note, warns) = change_note(&disk, Some(&offer));
        assert!(!warns && note.contains("Your other files are not touched."));
        let (note, warns) = change_note(&disk, None);
        assert!(warns && note.contains("will be erased"));
    }

    #[test]
    fn the_plan_beside_names_the_free_space_and_reads_back() {
        let disk = windows_disk();
        let offer = beside_offer(&disk, true).unwrap();
        let layout = Layout::parse("fr(azerty)").unwrap();
        let plan = filled().plan_with(&disk, &layout, "Europe/Paris", Some(beside_of(&offer)));
        assert_eq!(plan.mode, Mode::Beside);
        let beside = plan.beside.clone().unwrap();
        assert_eq!(beside.start, offer.region().start);
        assert_eq!(beside.esp, 1);
        assert_eq!(Plan::parse(&plan.render()), Ok(plan));
        // Without it the plan erases, as before.
        let erase = filled().plan(&disk, &layout, "Europe/Paris");
        assert_eq!(erase.mode, Mode::Erase);
        assert_eq!(erase.beside, None);
    }

    #[test]
    fn a_failure_beside_another_system_does_not_say_the_disk_was_erased() {
        let mut progress = Progress::default();
        progress.apply(Event::parse("beside").unwrap());
        for line in ["begin check", "end check", "begin partition"] {
            progress.apply(Event::parse(line).unwrap());
        }
        progress.apply(Event::Failed(
            "partition".to_string(),
            "sfdisk said no".to_string(),
        ));
        let words = progress.failure_words("SanDisk");
        assert!(
            words.contains("A new partition may have been added"),
            "{words}"
        );
        assert!(!words.contains("no longer holds what it did"), "{words}");
    }
    #[test]
    fn a_failure_after_the_partition_exists_says_what_exists_and_how_to_remove_it() {
        let mut progress = Progress::default();
        for line in [
            "disk /dev/vdb",
            "beside",
            "begin check",
            "end check",
            "begin partition",
            "made 3",
            "end partition",
            "begin format",
            "end format",
            "begin boot-loader",
        ] {
            progress.apply(Event::parse(line).unwrap());
        }
        assert_eq!(progress.made, Some(3));
        progress.apply(Event::Failed(
            "boot-loader".to_string(),
            "the firmware did not take a boot entry".to_string(),
        ));
        let words = progress.failure_words("");
        assert!(
            words.contains("number 3 of /dev/vdb (/dev/vdb3), exists"),
            "{words}"
        );
        assert!(words.contains("sudo sfdisk --delete /dev/vdb 3"), "{words}");
        assert!(words.contains("lsblk /dev/vdb"), "{words}");
        assert!(words.contains("folder and a boot entry"), "{words}");
        assert!(
            words.contains("will not offer to install beside here"),
            "{words}"
        );
        assert!(words.contains("were as they had been"), "{words}");
        // It does not claim the installer can simply be run again.
        assert!(!words.contains("can be run again"), "{words}");
    }

    #[test]
    fn a_failure_after_the_boot_loader_step_says_that_what_it_made_is_still_there() {
        let mut progress = Progress::default();
        for line in [
            "disk /dev/vdb",
            "beside",
            "begin check",
            "end check",
            "begin partition",
            "made 3",
            "end partition",
            "begin format",
            "end format",
            "begin boot-loader",
            "end boot-loader",
            "begin finish",
        ] {
            progress.apply(Event::parse(line).unwrap());
        }
        progress.apply(Event::Failed(
            "finish".to_string(),
            "sync failed".to_string(),
        ));
        let words = progress.failure_words("");
        assert!(words.contains("were made, and are still there"), "{words}");
        assert!(!words.contains("took them out"), "{words}");
        // Failing in the step itself, nothing is claimed removed.
        let mut during = Progress::default();
        for line in [
            "beside",
            "begin partition",
            "made 3",
            "end partition",
            "begin boot-loader",
        ] {
            during.apply(Event::parse(line).unwrap());
        }
        during.apply(Event::Failed("boot-loader".to_string(), "x".to_string()));
        let words = during.failure_words("");
        assert!(
            words.contains("anything not said there is still there"),
            "{words}"
        );
        assert!(!words.contains("took them out again"), "{words}");
    }

    #[test]
    fn a_failure_before_the_partition_exists_does_not_tell_anyone_to_delete_anything_unseen() {
        let mut progress = Progress::default();
        for line in [
            "disk /dev/vdb",
            "beside",
            "begin check",
            "end check",
            "begin partition",
        ] {
            progress.apply(Event::parse(line).unwrap());
        }
        progress.apply(Event::Failed(
            "partition".to_string(),
            "sfdisk said no".to_string(),
        ));
        let words = progress.failure_words("");
        assert!(words.contains("may have been added"), "{words}");
        assert!(words.contains("look at lsblk /dev/vdb"), "{words}");
        assert!(!words.contains("were as they had been"), "{words}");
        // And one that only looked changed nothing.
        let mut early = Progress::default();
        for line in ["beside", "begin check"] {
            early.apply(Event::parse(line).unwrap());
        }
        early.apply(Event::Failed(
            "check".to_string(),
            "Secure Boot is on".to_string(),
        ));
        assert!(early
            .failure_words("")
            .ends_with("Nothing on the computer was changed."));
    }

    #[test]
    fn what_the_review_says_it_changes_is_what_changes_and_the_other_system_may_ask_for_a_key() {
        let disk = windows_disk();
        let offer = beside_offer(&disk, true).unwrap();
        let rows = review_beside(&filled(), &disk, &offer, "x", "UTC");
        let all: String = rows.iter().map(|(_, value)| value.as_str()).collect();
        for word in [
            "EFI system partition",
            "firmware boot list",
            "BitLocker",
            "recovery key",
        ] {
            assert!(all.contains(word), "{word}");
        }
        // The old promise, which was untrue, is gone everywhere.
        assert!(!all.contains("Nothing that is already on the disk is changed"));
        let rows = mode_rows(&disk, true, Way::Erase, &Look::NotYet);
        assert!(rows[1]
            .detail
            .contains("a Sengi OS folder on the EFI system partition"));
        // Secure Boot is a reason in the row, and the choice cannot be made.
        let on = Firmware {
            uefi: true,
            secure_boot: true,
        };
        let rows = mode_rows(&disk, on, Way::Beside, &Look::NotYet);
        assert!(!rows[1].enabled && rows[1].detail.contains("Secure Boot is on"));
    }
}
