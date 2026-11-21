//! What the window shows, worked out from what was typed and what the
//! machine has. Nothing here knows about Slint, so it is tested as it is.

use sengi_config::settings::Layout;
use sengi_install::disks::{self, Disk};
use sengi_install::install::STEPS;
use sengi_install::names;
use sengi_install::plan::{Plan, LIVE_KERNEL, LIVE_ROOT};
use sengi_install::progress::Event;

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

/// What the list says above it.
pub fn disk_note(disks: &[Disk]) -> &'static str {
    if disks.is_empty() {
        "No disk was found. If the computer has one, it may need a driver Sengi OS does not have."
    } else if disks.iter().all(|disk| disks::refusal(disk).is_some()) {
        "None of these disks can be installed on. Sengi OS needs a disk of at least 8 GB that is not in use, and it does not install on the stick it is running from."
    } else {
        "Choose the disk to install on. All of it is used: what is on it now is erased."
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

    /// The plan for these answers.
    pub fn plan(&self, disk: &Disk, keyboard: &Layout, time_zone: &str) -> Plan {
        Plan {
            device: disk.path.clone(),
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
        ("Disk", format!("{} ({})", disk_title(disk), disk.path)),
        ("What is on it", "All of it is erased".to_string()),
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
    /// only looks.
    pub fn disk_was_touched(&self) -> bool {
        self.steps
            .iter()
            .skip(1)
            .any(|step| step.state != StepState::Waiting)
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
            parts: Vec::new(),
            fstype: String::new(),
            label: String::new(),
            mounted: Vec::new(),
            swap: false,
            held: false,
            medium: false,
        }
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
        assert_eq!(value("What is on it"), "All of it is erased");
        assert_eq!(value("Your account"), "Allan Smith (allan)");
        assert_eq!(value("Time zone"), "America/New York");
        assert!(value("Starts with").starts_with("UEFI"));
        let bios = review(&filled(), &disk(), "x", "UTC", false);
        assert!(bios[3].1.starts_with("BIOS"));
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
}
