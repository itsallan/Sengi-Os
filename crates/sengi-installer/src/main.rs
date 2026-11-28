//! sengi-installer: asks where and for whom to install Sengi OS, shows
//! what that will erase, and follows the installation.
//!
//! The window runs as the live session's user. The one thing that needs
//! root, writing the disk, is done by sengi-install, started through
//! sudo with the plan and the password on its standard input. The live
//! user may use sudo without a password, which is what makes that work
//! and is acceptable only there: whoever sits at a live session owns the
//! machine already. The installed system has no such rule and no
//! installer.

use std::cell::{Cell, RefCell};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, VecModel};

use sengi_config::settings::Layout;
use sengi_config::{Dirs, Value};
use sengi_install::disks::{self, Disk};
use sengi_install::progress::Event;
use sengi_install::shrink::{Analysis, Refusal};
use sengi_install::{PROGRESS, UNIT};
use sengi_installer::model::{self, Form, Look, Outcome, Progress, Squeeze, StepState, Typed, Way};
use sengi_installer::ui::{
    ChooserState, DialogSpec, DialogState, InfoRow, InstallerWindow, Item, SengiStepState, StepRow,
    Wizard,
};
use sengi_lock::entry::PasswordEntry;
use sengi_settings::app::{narrow, Choice, MAX_CHOICES};
use sengi_settings::keyboard::Database;
use sengi_settings::settings::Settings;
use sengi_settings::{datetime, derive};
use sengi_system::secret;

const USAGE: &str = "\
Usage: sengi-installer

Open the Sengi OS installer. It runs in the live session, from the USB
stick, and puts Sengi OS on a disk of this computer.

  -h, --help     show this help
  -V, --version  show the version
";

/// What the dock and the compositor know the window by. The `.desktop`
/// file has the same name.
const APP_ID: &str = "sengi-installer";

/// How the installation is handed over: to sengi-install, as root,
/// without being asked for a password (see the top of this file). That
/// program starts a service for the installation and returns. From then
/// on the installation is no child of this window and no part of this
/// session; the window only follows what it reports.
const START: [&str; 3] = ["-n", "/usr/bin/sengi-install", "start"];

/// How often the report is looked at for more.
const FOLLOW_EVERY: Duration = Duration::from_millis(200);
/// How long a service that was just started has to show itself.
const STARTING: Duration = Duration::from_secs(15);

/// How it is known that the installation is still going.
enum Alive {
    /// The service is active.
    Service,
    /// The stand-in of a demo is running.
    Child(std::process::Child),
}

impl Alive {
    fn check(&mut self) -> bool {
        match self {
            // "activating" counts: the unit is on its way.
            Alive::Service => Command::new("systemctl")
                .args(["is-active", UNIT])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .is_ok_and(|output| {
                    matches!(
                        String::from_utf8_lossy(&output.stdout).trim(),
                        "active" | "activating" | "deactivating"
                    )
                }),
            Alive::Child(child) => matches!(child.try_wait(), Ok(None)),
        }
    }
}

/// What a chooser on the Region page is choosing.
enum Choosing {
    Layout,
    /// A variant of this layout.
    Variant(String),
    Zone,
}

struct Chooser {
    choosing: Choosing,
    choices: Vec<Choice>,
}

/// A stand-in for the machine and the installing program, for looking
/// at the window without a disk to erase. Debug builds only.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(debug_assertions), allow(dead_code))]
enum Demo {
    Off,
    Works,
    Fails,
}

struct App {
    window: InstallerWindow,
    demo: Demo,
    page: Cell<&'static str>,
    disks: RefCell<Vec<Disk>>,
    chosen: RefCell<Option<Disk>>,
    /// How the chosen disk is to be used.
    way: Cell<Way>,
    /// What is known about making its Windows smaller, which look is the
    /// latest, what was chosen to give Sengi OS, and whether the person
    /// has said that there is a backup.
    look: RefCell<Look>,
    looks: Cell<u32>,
    room_gb: Cell<u64>,
    backup: Cell<bool>,
    form: RefCell<Form>,
    password: RefCell<PasswordEntry>,
    again: RefCell<PasswordEntry>,
    layout: RefCell<Layout>,
    zone: RefCell<String>,
    database: RefCell<Option<Database>>,
    chooser: RefCell<Option<Chooser>>,
    progress: RefCell<Progress>,
    installing: Cell<bool>,
    /// The names the system has taken.
    passwd: String,
    group: String,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Run `task` on the UI thread with the application, from any thread.
fn post(task: impl FnOnce(&Rc<App>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        let app = APP.with(|app| app.borrow().clone());
        if let Some(app) = app {
            task(&app);
        }
    });
}

fn rows<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

fn facts(pairs: Vec<(&str, String)>) -> ModelRc<InfoRow> {
    rows(
        pairs
            .into_iter()
            .map(|(title, value)| InfoRow {
                title: title.into(),
                value: value.into(),
            })
            .collect(),
    )
}

fn firmware() -> sengi_install::beside::Firmware {
    sengi_install::beside::firmware_now()
}

/// What the disks of a demo look like when the machine is not asked.
fn demo_uefi() -> sengi_install::beside::Firmware {
    sengi_install::beside::Firmware {
        uefi: true,
        secure_boot: false,
    }
}

/// Disks to show when there is no machine to ask.
fn demo_disks() -> Vec<Disk> {
    let mut start = 2048;
    let mut part = move |name: &str, size: u64, fstype: &str, label: &str, kind: &str| {
        let digits: String = name
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let number = digits.parse().unwrap_or(1);
        let at = start;
        start += size / 512;
        disks::Part {
            name: name.to_string(),
            number,
            start: at,
            kind: kind.to_string(),
            uuid: format!("demo-{name}"),
            size,
            fstype: fstype.to_string(),
            label: label.to_string(),
        }
    };
    let esp = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
    let disk = |name: &str, size: u64, model: &str| Disk {
        name: name.to_string(),
        path: format!("/dev/{name}"),
        size,
        model: model.to_string(),
        serial: String::new(),
        wwn: String::new(),
        tran: String::new(),
        read_only: false,
        pttype: "gpt".to_string(),
        ptuuid: "5f6c9a8e-0000-4000-8000-000000000001".to_string(),
        parts: Vec::new(),
        fstype: String::new(),
        label: String::new(),
        mounted: Vec::new(),
        swap: false,
        held: false,
        medium: false,
    };
    let mut main = disk("sda", 256_060_514_304, "SanDisk SD8SN8U256G");
    main.parts = vec![
        part("sda1", 104_857_600, "vfat", "SYSTEM", esp),
        part(
            "sda2",
            200_000_000_000,
            "ntfs",
            "Windows",
            "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7",
        ),
        part(
            "sda3",
            55_000_000_000,
            "ext4",
            "",
            "0fc63daf-8483-4772-8e79-3d69d8477de4",
        ),
    ];
    main.tran = "sata".to_string();
    main.serial = "162345400999".to_string();
    let mut stick = disk("sdb", 30_752_000_000, "Ultra Fit");
    stick.tran = "usb".to_string();
    stick.medium = true;
    stick.fstype = "iso9660".to_string();
    stick.label = "SENGIOS".to_string();
    stick.serial = "4C530001230512117093".to_string();
    let mut card = disk("mmcblk0", 3_900_000_000, "SD04G");
    card.tran = "mmc".to_string();
    card.serial = "0x1a2b3c4d".to_string();
    let mut second = disk("nvme0n1", 500_107_862_016, "Samsung SSD 970 EVO 500GB");
    second.tran = "nvme".to_string();
    second.serial = "S466NX0M123456H".to_string();
    second.parts = vec![part(
        "nvme0n1p1",
        500_000_000_000,
        "ext4",
        "data",
        "0fc63daf-8483-4772-8e79-3d69d8477de4",
    )];
    second.mounted = vec!["/media/live/data".to_string()];
    // A computer that came with Windows, which has all of the disk.
    let mut full = disk("sdd", 512_110_190_592, "Micron 1100 512GB");
    full.tran = "sata".to_string();
    full.serial = "17241A2B3C4D".to_string();
    full.parts = vec![
        part("sdd1", 104_857_600, "vfat", "", esp),
        part(
            "sdd2",
            512_000_000_000,
            "ntfs",
            "Windows",
            sengi_install::shrink::BASIC_DATA_TYPE,
        ),
    ];
    // Placed from the start of its own disk, so that nothing is free.
    full.parts[0].start = 2048;
    full.parts[1].start = 206_848;
    // A computer that came with Windows and has most of its disk free.
    let mut third = disk("sdc", 1_000_204_886_016, "WD Blue SN570 1TB");
    third.tran = "sata".to_string();
    third.serial = "WD-2214AB778899".to_string();
    third.parts = vec![
        part("sdc1", 272_629_760, "vfat", "", esp),
        part(
            "sdc2",
            250_000_000_000,
            "ntfs",
            "Windows",
            "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7",
        ),
    ];
    vec![main, third, full, second, stick, card]
}

/// What a look at the Windows of a demo finds.
fn demo_analysis(disk: &Disk) -> Analysis {
    let part = sengi_install::shrink::candidate(disk);
    let (number, start, size) = part.map_or((2, 206_848, disk.size), |part| {
        (part.number, part.start, part.size)
    });
    let mut analysis = Analysis {
        table: sengi_install::beside::Table::from_disk(disk)
            .map(|table| table.snapshot())
            .unwrap_or_default(),
        esp: 1,
        number,
        start,
        sectors: size / 512,
        used_bytes: 184_300_000_000,
        room: sengi_install::shrink::Room {
            keep_bytes: 0,
            smallest_gb: 11,
            largest_gb: 11,
        },
    };
    if let Ok(room) = sengi_install::shrink::room(&analysis.entry(), analysis.used_bytes) {
        analysis.room = room;
    }
    analysis
}

/// Ask sengi-install, as root, about the Windows partition of `disk`.
fn analyze(disk: &str) -> Result<Result<Analysis, Refusal>, String> {
    let output = Command::new("sudo")
        .args(["-n", "/usr/bin/sengi-install", "analyze", disk])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("it could not be looked at: {error}"))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr).into_owned();
        let last = said
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("it said nothing");
        return Err(last.trim_start_matches("sengi-install: ").to_string());
    }
    Analysis::parse(&String::from_utf8_lossy(&output.stdout))
}

/// What stands in for sengi-install in a demo: it says what the real
/// one says, slowly enough to be looked at.
fn demo_script(fails: bool) -> String {
    let end = if fails {
        "echo 'begin boot-loader'; sleep 1; echo 'failed boot-loader grub-install ended with status 1: cannot find a device for /boot/efi'; exit 1"
    } else {
        "for s in boot-loader finish; do echo \"begin $s\"; sleep 0.7; echo \"end $s\"; done; echo 'done 135'"
    };
    format!(
        "read -r password; cat >/dev/null; echo 'log /var/log/sengi-install.log'; echo 'disk /dev/sda'; \
         for s in check partition format; do echo \"begin $s\"; sleep 0.7; echo \"end $s\"; done; \
         echo 'begin copy'; for p in 8 21 42; do echo \"percent copy $p\"; sleep 0.5; done; sleep ${{SENGI_DEMO_PAUSE:-1}}; \
         for p in 63 87 100; do echo \"percent copy $p\"; sleep 0.5; done; echo 'end copy'; \
         for s in configure account boot-image; do echo \"begin $s\"; sleep 0.7; echo \"end $s\"; done; {end}"
    )
}

impl App {
    fn wizard(&self) -> Wizard<'_> {
        self.window.global::<Wizard>()
    }

    /// Go to `page` and set the bar under it to match.
    fn show(&self, page: &'static str) {
        self.page.set(page);
        let wizard = self.wizard();
        self.window.invoke_release();
        wizard.set_place(model::place(page).into());
        wizard.set_back_label(
            if model::page_before(page).is_some() {
                "Back"
            } else {
                ""
            }
            .into(),
        );
        let way = self.way.get();
        wizard.set_next_erases(page == "review" && way != Way::Beside);
        wizard.set_next_label(
            match (page, way) {
                ("review", Way::Erase) => "Erase disk and install\u{2026}",
                ("review", Way::Beside) => "Install Sengi OS\u{2026}",
                ("review", Way::Shrink) => "Shrink Windows and install\u{2026}",
                _ => "Continue",
            }
            .into(),
        );
        match page {
            "disk" => self.scan(),
            "you" => self.render_form(),
            "review" => self.render_review(),
            _ => {}
        }
        self.set_can_next();
        wizard.set_page(page.into());
        self.window.invoke_settle();
    }

    fn set_can_next(&self) {
        let can = match self.page.get() {
            "region" => !self.zone.borrow().is_empty(),
            "disk" => self.disk_ready(),
            "you" => self.form_ready(),
            "progress" => self.progress.borrow().outcome != Outcome::Running,
            _ => true,
        };
        self.wizard().set_can_next(can);
    }

    // Region.

    fn layout_name(&self) -> String {
        let layout = self.layout.borrow();
        match self.database.borrow().as_ref() {
            Some(database) => database.describe(&layout),
            None => layout.render(),
        }
    }

    fn render_region(&self) {
        let wizard = self.wizard();
        wizard.set_keyboard(self.layout_name().into());
        let zone = self.zone.borrow();
        wizard.set_zone(
            if zone.is_empty() {
                "Choose\u{2026}".to_string()
            } else {
                zone.replace('_', " ")
            }
            .into(),
        );
    }

    fn open_chooser(
        &self,
        title: &str,
        searchable: bool,
        choosing: Choosing,
        choices: Vec<Choice>,
    ) {
        let state = self.window.global::<ChooserState>();
        state.set_title(title.into());
        state.set_searchable(searchable);
        *self.chooser.borrow_mut() = Some(Chooser { choosing, choices });
        self.fill_chooser("", true);
        state.set_generation(state.get_generation().wrapping_add(1));
        state.set_open(true);
    }

    /// Show the choices that match what was typed. A long list is shown
    /// a stretch at a time, as Settings shows it.
    fn fill_chooser(&self, typed: &str, opening: bool) {
        let state = self.window.global::<ChooserState>();
        let chooser = self.chooser.borrow();
        let Some(open) = chooser.as_ref() else { return };
        let matching = narrow(&open.choices, typed);
        let total = matching.len();
        let at = matching.iter().position(|choice| choice.current);
        let first = match at {
            Some(at) if opening && total > MAX_CHOICES => {
                at.saturating_sub(MAX_CHOICES / 2).min(total - MAX_CHOICES)
            }
            _ => 0,
        };
        let items: Vec<Item> = matching
            .iter()
            .skip(first)
            .take(MAX_CHOICES)
            .map(|choice| Item {
                id: choice.id.as_str().into(),
                title: choice.title.as_str().into(),
                detail: choice.detail.as_str().into(),
                action: "Choose".into(),
                checked: choice.current,
                enabled: true,
                ..Item::default()
            })
            .collect();
        state.set_empty(if total == 0 { "Nothing matches" } else { "" }.into());
        state.set_current(match at {
            Some(at) if at >= first && at < first + MAX_CHOICES => (at - first) as i32,
            _ => 0,
        });
        state.set_items(rows(items));
    }

    fn close_chooser(&self) {
        *self.chooser.borrow_mut() = None;
        self.window.global::<ChooserState>().set_open(false);
        // The choice may be what the page was waiting for.
        self.set_can_next();
        if self.wizard().get_can_next() {
            self.window.invoke_focus_next();
        } else {
            self.window.invoke_release();
        }
    }

    fn choose_keyboard(&self) {
        let database = self.database.borrow();
        let Some(database) = database.as_ref() else {
            return;
        };
        let now = self.layout.borrow().layout.clone();
        let choices = database
            .layouts
            .iter()
            .map(|layout| Choice {
                id: layout.name.clone(),
                title: layout.description.clone(),
                detail: layout.name.clone(),
                current: layout.name == now,
            })
            .collect();
        self.open_chooser("Keyboard layout", true, Choosing::Layout, choices);
    }

    fn choose_zone(&self) {
        let now = self.zone.borrow().clone();
        let choices = datetime::zones()
            .into_iter()
            .map(|zone| Choice {
                current: zone.name == now,
                id: zone.name,
                title: zone.city,
                detail: zone.region,
            })
            .collect();
        self.open_chooser("Time zone", true, Choosing::Zone, choices);
    }

    fn chosen(self: &Rc<Self>, id: String) {
        let Some(chooser) = self.chooser.borrow_mut().take() else {
            return;
        };
        match chooser.choosing {
            Choosing::Zone => {
                *self.zone.borrow_mut() = id;
                self.close_chooser();
            }
            Choosing::Layout => {
                let variants: Vec<Choice> = self
                    .database
                    .borrow()
                    .as_ref()
                    .map(|database| {
                        database
                            .variants_of(&id)
                            .into_iter()
                            .map(|variant| Choice {
                                id: variant.name.clone(),
                                title: variant.description.clone(),
                                detail: variant.name.clone(),
                                current: false,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if variants.is_empty() {
                    self.set_layout(id, String::new());
                    self.close_chooser();
                } else {
                    let mut choices = vec![Choice {
                        id: String::new(),
                        title: "The usual one".to_string(),
                        detail: "No variant".to_string(),
                        current: true,
                    }];
                    choices.extend(variants);
                    self.open_chooser("Variant", true, Choosing::Variant(id), choices);
                }
            }
            Choosing::Variant(layout) => {
                self.set_layout(layout, id);
                self.close_chooser();
            }
        }
        self.render_region();
    }

    /// Take the layout for the installed system, and switch the running
    /// session to it now: the password is typed on a later page, and it
    /// has to be typed with the keyboard it will be typed with again.
    fn set_layout(&self, layout: String, variant: String) {
        let Some(layout) = Layout::parse(&Layout { layout, variant }.render()) else {
            return;
        };
        *self.layout.borrow_mut() = layout.clone();
        if self.demo != Demo::Off {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("installer-keyboard".to_string())
            .spawn(move || {
                let dirs = Dirs::from_env();
                let (mut settings, _) = Settings::load(&dirs);
                let value = Value::List(vec![layout.render()]);
                match settings.set("keyboard", "layouts", value) {
                    Ok(()) => {
                        for problem in derive::apply(&dirs, &settings).problems() {
                            eprintln!("sengi-installer: {problem}");
                        }
                        derive::reconfigure();
                    }
                    Err(problem) => eprintln!("sengi-installer: {problem}"),
                }
            });
        if let Err(error) = spawned {
            eprintln!("sengi-installer: could not switch the keyboard layout: {error}");
        }
    }

    // Disk.

    /// Look at the disks, again or for the first time. A disk that was
    /// chosen stays chosen only if it is still there and still the same.
    fn scan(&self) {
        let found = match self.demo {
            Demo::Off => disks::scan_system(false),
            _ => demo_disks(),
        };
        let kept = self.chosen.borrow().as_ref().and_then(|chosen| {
            found
                .iter()
                .find(|disk| *disk == chosen && disks::refusal(disk).is_none())
                .cloned()
        });
        if kept.is_none() {
            self.forget_way();
        }
        *self.chosen.borrow_mut() = kept;
        *self.disks.borrow_mut() = found;
        self.render_disks();
    }

    fn render_disks(&self) {
        let wizard = self.wizard();
        let disks = self.disks.borrow();
        let chosen = self.chosen.borrow();
        let items: Vec<Item> = disks
            .iter()
            .map(|disk| {
                let offered = disks::refusal(disk).is_none();
                let is_chosen = chosen
                    .as_ref()
                    .is_some_and(|chosen| chosen.path == disk.path);
                Item {
                    id: disk.path.as_str().into(),
                    title: model::disk_title(disk).into(),
                    detail: model::disk_detail(disk).into(),
                    action: if offered && !is_chosen { "Choose" } else { "" }.into(),
                    checked: is_chosen,
                    enabled: offered,
                    ..Item::default()
                }
            })
            .collect();
        wizard.set_disk_note(model::disk_note(&disks).into());
        wizard.set_disks(rows(items));
        // The keyboard starts on the disk that was chosen, or on the
        // first that can be. Nothing is chosen for anyone.
        let at = disks.iter().position(|disk| match chosen.as_ref() {
            Some(chosen) => chosen.path == disk.path,
            None => disks::refusal(disk).is_none(),
        });
        if let Some(at) = at {
            wizard.set_disk_current(at as i32);
        }
        wizard.set_disk_chosen(chosen.is_some());
        let uefi = self.uefi_now();
        let mode_items: Vec<Item> = chosen
            .as_ref()
            .map(|disk| {
                model::mode_rows(disk, uefi, self.way.get(), &self.look.borrow())
                    .into_iter()
                    .map(|row| Item {
                        id: row.id.into(),
                        title: row.title.into(),
                        detail: row.detail.into(),
                        action: if row.enabled && !row.checked {
                            "Choose"
                        } else {
                            ""
                        }
                        .into(),
                        checked: row.checked,
                        enabled: row.enabled,
                        ..Item::default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(at) = mode_items.iter().position(|item| item.checked) {
            wizard.set_mode_current(at as i32);
        }
        wizard.set_modes(rows(mode_items));
        let (note, warns) = chosen
            .as_ref()
            .map(|disk| self.note_for(disk))
            .unwrap_or_default();
        wizard.set_erase_warning(note.into());
        wizard.set_note_warns(warns);
        drop(chosen);
        self.render_shrink();
        self.set_can_next();
    }

    /// What is said about the change to `disk` that is chosen.
    fn note_for(&self, disk: &Disk) -> (String, bool) {
        match self.way.get() {
            Way::Unchosen => (String::new(), false),
            Way::Shrink => match self.squeeze() {
                Some(squeeze) => (model::shrink_note(disk, &squeeze), true),
                None => (String::new(), true),
            },
            _ => model::change_note(disk, self.offer_for(disk).as_ref()),
        }
    }

    /// The size control and what goes with it, as far as the chosen disk
    /// has them.
    fn render_shrink(&self) {
        let wizard = self.wizard();
        let at_sight = self
            .chosen
            .borrow()
            .as_ref()
            .and_then(|disk| model::refused_at_sight(disk, self.uefi_now()));
        let look = self.look.borrow().clone();
        let shrinking = self.way.get() == Way::Shrink;
        let (state, words) = match (at_sight, &look) {
            (Some(refusal), _) => ("refused", refusal.words()),
            (None, Look::Refused(refusal)) if shrinking => ("refused", refusal.words()),
            (None, Look::Looking) if shrinking => ("looking", String::new()),
            (None, Look::Ready(_)) if shrinking => ("ready", String::new()),
            _ => ("", String::new()),
        };
        wizard.set_shrink_state(state.into());
        wizard.set_shrink_words(words.into());
        wizard.set_shrink_backup(self.backup.get());
        if let Some(squeeze) = self.squeeze() {
            let room = squeeze.analysis.room;
            let [uses, range, sengi, windows] = squeeze.words();
            wizard.set_shrink_uses(uses.into());
            wizard.set_shrink_range(range.into());
            wizard.set_shrink_sengi(sengi.into());
            wizard.set_shrink_windows(windows.into());
            wizard.set_shrink_min(room.smallest_gb as i32);
            wizard.set_shrink_max(room.largest_gb as i32);
            wizard.set_shrink_gb(squeeze.gb as i32);
        }
    }

    /// The size that is chosen, once Windows has been looked at.
    fn squeeze(&self) -> Option<Squeeze> {
        match &*self.look.borrow() {
            Look::Ready(analysis) => Some(Squeeze::with(analysis.clone(), self.room_gb.get())),
            _ => None,
        }
    }

    fn disk_ready(&self) -> bool {
        model::disk_page_ready(
            self.chosen.borrow().is_some(),
            self.way.get(),
            &self.look.borrow(),
            self.backup.get(),
        )
    }

    /// Back to nothing chosen about how a disk is used.
    fn forget_way(&self) {
        self.way.set(Way::Erase);
        self.forget_look();
    }

    /// What was found out about Windows no longer stands: it is looked at
    /// again when it is wanted, and the backup is asked about again.
    fn forget_look(&self) {
        *self.look.borrow_mut() = Look::NotYet;
        self.looks.set(self.looks.get().wrapping_add(1));
        self.backup.set(false);
    }

    /// Look at the disks and at Windows again, as the button asks.
    fn look_again(&self) {
        self.forget_look();
        self.scan();
        if self.way.get() == Way::Shrink {
            self.look_at_windows();
        }
    }

    /// Have sengi-install look at the Windows partition of the chosen
    /// disk, which writes nothing, on a thread of its own: on a large
    /// volume it takes a while.
    fn look_at_windows(&self) {
        let Some(disk) = self.chosen.borrow().clone() else {
            return;
        };
        *self.look.borrow_mut() = Look::Looking;
        let look = self.looks.get().wrapping_add(1);
        self.looks.set(look);
        self.render_disks();
        if self.demo != Demo::Off {
            post(move |app| app.looked(look, Ok(Ok(demo_analysis(&disk)))));
            return;
        }
        let path = disk.path.clone();
        let spawned = std::thread::Builder::new()
            .name("installer-windows".to_string())
            .spawn(move || {
                let answer = analyze(&path);
                post(move |app| app.looked(look, answer));
            });
        if let Err(error) = spawned {
            self.looked(look, Err(error.to_string()));
        }
    }

    /// The answer to look number `look`. One that is not the latest is for
    /// a disk, or a state of one, that is no longer asked about.
    fn looked(&self, look: u32, answer: Result<Result<Analysis, Refusal>, String>) {
        if look != self.looks.get() || *self.look.borrow() != Look::Looking {
            return;
        }
        // The disk the person is shown and the one root read are one.
        let shown = self
            .chosen
            .borrow()
            .as_ref()
            .and_then(|disk| sengi_install::beside::Table::from_disk(disk).ok())
            .map(|table| table.snapshot());
        let found = match answer {
            Ok(Ok(analysis)) if shown.as_deref() != Some(analysis.table.as_str()) => {
                Look::Refused(Refusal::Other(
                    "the partition table of the disk is not the one that was listed".to_string(),
                ))
            }
            Ok(Ok(analysis)) => {
                self.room_gb.set(Squeeze::new(analysis.clone()).gb);
                Look::Ready(analysis)
            }
            Ok(Err(refusal)) => Look::Refused(refusal),
            Err(why) => Look::Refused(Refusal::Other(sengi_text::line(&why, 300))),
        };
        *self.look.borrow_mut() = found;
        self.render_disks();
    }

    fn size_picked(&self, gb: i32) {
        self.room_gb.set(gb.max(0) as u64);
        self.render_shrink();
        if let Some(disk) = self.chosen.borrow().as_ref() {
            let (note, warns) = self.note_for(disk);
            let wizard = self.wizard();
            wizard.set_erase_warning(note.into());
            wizard.set_note_warns(warns);
        }
    }

    fn backup_ticked(&self, ticked: bool) {
        self.backup.set(ticked);
        self.render_shrink();
        self.set_can_next();
    }

    fn uefi_now(&self) -> sengi_install::beside::Firmware {
        match self.demo {
            Demo::Off => firmware(),
            _ => demo_uefi(),
        }
    }

    /// What installing beside what is on `disk` would use, if that is what
    /// was chosen and it can be done.
    fn offer_for(&self, disk: &Disk) -> Option<sengi_install::beside::Offer> {
        if self.way.get() != Way::Beside {
            return None;
        }
        model::beside_offer(disk, self.uefi_now()).ok()
    }

    fn pick_mode(&self, id: &str) {
        let Some(disk) = self.chosen.borrow().clone() else {
            return;
        };
        let way = match id {
            model::BESIDE if model::beside_offer(&disk, self.uefi_now()).is_ok() => Way::Beside,
            model::SHRINK
                if sengi_install::beside::squeeze(&disk, self.uefi_now()).is_some()
                    && model::refused_at_sight(&disk, self.uefi_now()).is_none() =>
            {
                Way::Shrink
            }
            model::ERASE => Way::Erase,
            _ => return,
        };
        self.way.set(way);
        if way == Way::Shrink && *self.look.borrow() == Look::NotYet {
            self.look_at_windows();
        } else {
            self.render_disks();
        }
    }

    fn pick_disk(&self, path: &str) {
        let picked = self
            .disks
            .borrow()
            .iter()
            .find(|disk| disk.path == path && disks::refusal(disk).is_none())
            .cloned();
        if picked.is_some() {
            // Another disk starts again from erasing it: beside is chosen
            // on purpose, for the disk it was chosen on.
            if self.chosen.borrow().as_ref().map(|disk| &disk.path)
                != picked.as_ref().map(|disk| &disk.path)
            {
                self.forget_way();
                if let Some(disk) = picked.as_ref() {
                    self.way.set(model::first_way(disk, self.uefi_now()));
                }
            }
            *self.chosen.borrow_mut() = picked;
            self.render_disks();
        }
    }

    // You.

    fn typed(&self) -> Typed {
        let (password, again) = (self.password.borrow(), self.again.borrow());
        Typed {
            password_chars: password.chars(),
            again_chars: again.chars(),
            same: password.same_as(&again),
        }
    }

    fn form_ready(&self) -> bool {
        self.form
            .borrow()
            .ready(self.typed(), &self.passwd, &self.group)
    }

    fn render_form(&self) {
        let wizard = self.wizard();
        let typed = self.typed();
        let form = self.form.borrow();
        let problems = form.problems(typed, &self.passwd, &self.group);
        // Only what the program changed is written back: writing a
        // field's own text into it would move its caret.
        if wizard.get_user_name() != form.user_name.as_str() {
            wizard.set_user_name(form.user_name.as_str().into());
        }
        wizard.set_full_name_problem(problems.full_name.into());
        wizard.set_user_name_problem(problems.user_name.into());
        wizard.set_host_name_problem(problems.host_name.into());
        wizard.set_password_problem(problems.password.into());
        wizard.set_password_length(typed.password_chars as i32);
        wizard.set_again_length(typed.again_chars as i32);
        drop(form);
        self.set_can_next();
    }

    fn entry(&self, field: &str) -> &RefCell<PasswordEntry> {
        if field == "again" {
            &self.again
        } else {
            &self.password
        }
    }

    // Review.

    fn render_review(&self) {
        let chosen = self.chosen.borrow();
        let Some(disk) = chosen.as_ref() else { return };
        let wizard = self.wizard();
        let offer = self.offer_for(disk);
        let squeeze = self.squeeze().filter(|_| self.way.get() == Way::Shrink);
        let rows = match (&offer, &squeeze) {
            (_, Some(squeeze)) => model::review_shrink(
                &self.form.borrow(),
                disk,
                squeeze,
                &self.layout_name(),
                &self.zone.borrow(),
            ),
            (Some(offer), None) => model::review_beside(
                &self.form.borrow(),
                disk,
                offer,
                &self.layout_name(),
                &self.zone.borrow(),
            ),
            (None, None) => model::review(
                &self.form.borrow(),
                disk,
                &self.layout_name(),
                &self.zone.borrow(),
                self.uefi_now().uefi,
            ),
        };
        wizard.set_review(facts(rows));
        let (note, warns) = self.note_for(disk);
        wizard.set_erase_warning(note.into());
        wizard.set_note_warns(warns);
    }

    /// The question before the disk is erased. Its dialog starts with
    /// the keyboard on Cancel.
    fn ask_to_erase(&self) {
        let chosen = self.chosen.borrow();
        let Some(disk) = chosen.as_ref() else { return };
        let (title, body, confirm, danger) = match self.way.get() {
            Way::Unchosen => return,
            Way::Shrink => match self.squeeze() {
                Some(squeeze) if self.backup.get() => model::shrink_question(disk, &squeeze),
                _ => return,
            },
            _ => model::question(disk, self.offer_for(disk).as_ref()),
        };
        let state = self.window.global::<DialogState>();
        state.set_spec(DialogSpec {
            title: title.into(),
            body: body.into(),
            confirm: confirm.into(),
            cancel: "Cancel".into(),
            danger,
            ..DialogSpec::default()
        });
        state.set_open(true);
    }

    fn close_dialog(&self) {
        self.window.global::<DialogState>().set_open(false);
        self.window.invoke_settle();
    }

    // The installation.

    fn start(self: &Rc<Self>) {
        let chosen = self.chosen.borrow().clone();
        let (Some(disk), false) = (chosen, self.installing.get()) else {
            return;
        };
        if !self.form_ready() {
            return;
        }
        let way = self.way.get();
        let beside = match way {
            Way::Unchosen => return,
            Way::Erase => None,
            Way::Beside => self.offer_for(&disk).map(|offer| model::beside_of(&offer)),
            Way::Shrink => self
                .squeeze()
                .filter(|_| self.backup.get())
                .and_then(|squeeze| squeeze.beside()),
        };
        // Beside was chosen and cannot be done any more: nothing starts,
        // and nothing is erased in its place.
        if way != Way::Erase && beside.is_none() {
            return;
        }
        let plan = self
            .form
            .borrow()
            .plan_with(&disk, &self.layout.borrow(), &self.zone.borrow(), beside)
            .render();
        let Some(password) = self.password.borrow_mut().take() else {
            return;
        };
        self.again.borrow_mut().clear();
        self.show_progress();

        // The password first, on a line of its own, then the plan.
        let hand_over = |mut command: Command, stdout: Stdio| {
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(stdout)
                .stderr(Stdio::piped())
                .spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(password.expose().as_bytes())?;
                stdin.write_all(b"\n")?;
                stdin.write_all(plan.as_bytes())?;
            }
            Ok::<_, std::io::Error>(child)
        };
        match self.demo {
            Demo::Off => {
                let mut command = Command::new("sudo");
                command.args(START);
                // It returns as soon as the service is started, or says
                // why it was not.
                let handed =
                    hand_over(command, Stdio::null()).and_then(|child| child.wait_with_output());
                match handed {
                    Ok(output) if output.status.success() => {
                        self.follow(PathBuf::from(PROGRESS), Alive::Service);
                    }
                    Ok(output) => {
                        let said = String::from_utf8_lossy(&output.stderr).into_owned();
                        let last = said
                            .lines()
                            .rev()
                            .find(|line| !line.trim().is_empty())
                            .unwrap_or_default();
                        self.ended(&output.status.to_string(), last);
                    }
                    Err(error) => self.ended("it could not be started", &error.to_string()),
                }
            }
            demo => {
                let report = std::env::temp_dir()
                    .join(format!("sengi-installer-demo-{}", std::process::id()));
                let mut command = Command::new("sh");
                command.args(["-c", &demo_script(demo == Demo::Fails)]);
                let started = std::fs::File::create(&report)
                    .and_then(|file| hand_over(command, Stdio::from(file)));
                match started {
                    Ok(child) => self.follow(report, Alive::Child(child)),
                    Err(error) => self.ended("it could not be started", &error.to_string()),
                }
            }
        }
    }

    /// The page the installation is followed on, with nothing on it yet.
    fn show_progress(&self) {
        self.installing.set(true);
        *self.progress.borrow_mut() = Progress::default();
        let wizard = self.wizard();
        self.window.invoke_release();
        self.page.set("progress");
        wizard.set_place("".into());
        wizard.set_back_label("".into());
        wizard.set_next_label("".into());
        wizard.set_next_erases(false);
        wizard.set_outcome("".into());
        wizard.set_outcome_words("".into());
        wizard.set_page("progress".into());
        self.render_progress();
    }

    /// Read the installation's report as it grows, from the start, and
    /// pass on what it says. It is over when the report says how it
    /// ended, or when the installation has gone without saying.
    fn follow(&self, report: PathBuf, mut alive: Alive) {
        let spawned = std::thread::Builder::new()
            .name("installer-progress".to_string())
            .spawn(move || {
                let started = Instant::now();
                let mut read = 0u64;
                let mut pending = String::new();
                let mut seen = false;
                let mut quiet = 0u32;
                let mut last_look = false;
                loop {
                    let mut over = false;
                    if let Ok(mut file) = std::fs::File::open(&report) {
                        seen = true;
                        let mut more = String::new();
                        if file.seek(SeekFrom::Start(read)).is_ok()
                            && file.read_to_string(&mut more).is_ok()
                        {
                            read += more.len() as u64;
                            quiet = if more.is_empty() { quiet + 1 } else { 0 };
                            pending.push_str(&more);
                        }
                        // Only whole lines: the last may still be on its way.
                        while let Some(end) = pending.find('\n') {
                            let line: String = pending.drain(..=end).collect();
                            if let Some(event) = Event::parse(&line) {
                                over |= matches!(event, Event::Done(_) | Event::Failed(..));
                                post(move |app| app.event(event));
                            }
                        }
                    } else {
                        quiet += 1;
                    }
                    if over || last_look {
                        break;
                    }
                    // Nothing new for a second: is it still there? A
                    // service just started is given time to appear.
                    if quiet > 0
                        && quiet.is_multiple_of(5)
                        && (seen || started.elapsed() > STARTING)
                    {
                        // Gone. What it wrote last is read once more.
                        last_look = !alive.check();
                        continue;
                    }
                    std::thread::sleep(FOLLOW_EVERY);
                }
                post(|app| app.ended("it stopped", ""));
            });
        if let Err(error) = spawned {
            self.ended("its report could not be followed", &error.to_string());
        }
    }

    /// An installation that was started before this window was opened:
    /// one that is running is followed again, and one that finished is
    /// shown as finished. True if there was one.
    fn reattach(&self) -> bool {
        if self.demo != Demo::Off {
            return false;
        }
        let Ok(report) = std::fs::read_to_string(PROGRESS) else {
            return false;
        };
        let mut alive = Alive::Service;
        if alive.check() {
            self.show_progress();
            self.follow(PathBuf::from(PROGRESS), alive);
            return true;
        }
        let mut progress = Progress::default();
        for event in report.lines().filter_map(Event::parse) {
            progress.apply(event);
        }
        if !matches!(progress.outcome, Outcome::Done(_)) {
            return false;
        }
        self.show_progress();
        *self.progress.borrow_mut() = progress;
        self.installing.set(false);
        self.render_progress();
        true
    }

    fn event(&self, event: Event) {
        self.progress.borrow_mut().apply(event);
        self.render_progress();
    }

    /// The installing program has gone.
    fn ended(&self, status: &str, last_words: &str) {
        self.progress.borrow_mut().ended(status, last_words);
        self.installing.set(false);
        self.render_progress();
    }

    fn render_progress(&self) {
        let wizard = self.wizard();
        let progress = self.progress.borrow();
        let steps: Vec<StepRow> = progress
            .steps
            .iter()
            .map(|step| StepRow {
                text: step.title.into(),
                state: match step.state {
                    StepState::Waiting => SengiStepState::Waiting,
                    StepState::Running => SengiStepState::Running,
                    StepState::Done => SengiStepState::Done,
                    StepState::Failed => SengiStepState::Failed,
                },
                percent: step.percent.map_or(-1, i32::from),
            })
            .collect();
        wizard.set_steps(rows(steps));
        wizard.set_warnings(progress.warnings.join(" ").into());
        wizard.set_shrinking(progress.shrinking());
        // While the program runs, the window waits for it. When it has
        // said how it ended and has gone, the buttons come back.
        if self.installing.get() {
            return;
        }
        let disk = self
            .chosen
            .borrow()
            .as_ref()
            .map(model::disk_title)
            .unwrap_or_else(|| progress.disk.clone());
        match &progress.outcome {
            Outcome::Running => {}
            Outcome::Done(seconds) => {
                wizard.set_outcome("done".into());
                let windows = if progress.shrinks.is_some() {
                    " Windows was made smaller: the next time it starts it checks its disk first. That is expected; let it finish."
                } else {
                    ""
                };
                wizard.set_outcome_words(
                    format!(
                        "Sengi OS is on {disk}. It took {}. Restart the computer, and take the USB stick out as soon as the screen goes dark: left in, the computer may start from the stick again. Then log in with the name and password you chose.{windows}",
                        model::duration_words(*seconds)
                    )
                    .into(),
                );
                wizard.set_outcome_facts(facts(Vec::new()));
                wizard.set_next_label("Restart".into());
                wizard.set_back_label("Not now".into());
            }
            Outcome::Failed(..) => {
                wizard.set_outcome("failed".into());
                wizard.set_outcome_words(progress.failure_words(&disk).into());
                let mut rows = Vec::new();
                if !progress.disk.is_empty() {
                    rows.push(("Disk", progress.disk.clone()));
                }
                if let Some(step) = progress.failed_step() {
                    rows.push(("Stopped at", step.to_string()));
                }
                if !progress.log.is_empty() {
                    rows.push(("Full record", progress.log.clone()));
                }
                wizard.set_outcome_facts(facts(rows));
                wizard.set_next_label("Close".into());
                wizard.set_back_label("".into());
            }
        }
        drop(progress);
        self.set_can_next();
        self.window.invoke_focus_next();
        // The page has a new title and new words above the steps, and the
        // renderer has been seen to leave the old title standing. The whole
        // window is drawn once more.
        self.window.invoke_draw_everything();
    }

    fn next(self: &Rc<Self>) {
        match self.page.get() {
            "review" => self.ask_to_erase(),
            "progress" => {
                let done = matches!(self.progress.borrow().outcome, Outcome::Done(_));
                if done && self.demo == Demo::Off {
                    if let Err(error) = Command::new("systemctl").arg("reboot").status() {
                        eprintln!("sengi-installer: could not restart: {error}");
                    }
                }
                if !self.installing.get() {
                    let _ = slint::quit_event_loop();
                }
            }
            page => {
                let ready = match page {
                    "region" => !self.zone.borrow().is_empty(),
                    "disk" => self.disk_ready(),
                    "you" => self.form_ready(),
                    _ => true,
                };
                if let (true, Some(after)) = (ready, model::page_after(page)) {
                    self.show(after);
                }
            }
        }
    }

    fn back(&self) {
        match self.page.get() {
            // "Not now", once it is installed.
            "progress" if !self.installing.get() => {
                let _ = slint::quit_event_loop();
            }
            page => {
                if let Some(before) = model::page_before(page) {
                    self.show(before);
                }
            }
        }
    }

    fn wire(self: &Rc<Self>) {
        let wizard = self.wizard();
        let weak = Rc::downgrade(self);
        macro_rules! on {
            ($setter:ident, |$app:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = weak.clone();
                wizard.$setter(move |$($arg),*| {
                    if let Some($app) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        on!(on_next, |app| app.next());
        on!(on_back, |app| app.back());
        on!(on_choose_keyboard, |app| app.choose_keyboard());
        on!(on_choose_zone, |app| app.choose_zone());
        on!(on_disk_picked, |app, id| app.pick_disk(&id));
        on!(on_mode_picked, |app, id| app.pick_mode(&id));
        on!(on_look_again, |app| app.look_again());
        on!(on_shrink_picked, |app, gb| app.size_picked(gb));
        on!(on_backup_ticked, |app, ticked| app.backup_ticked(ticked));
        on!(on_full_name_edited, |app, text| {
            app.form.borrow_mut().set_full_name(&text);
            app.render_form();
        });
        on!(on_user_name_edited, |app, text| {
            app.form.borrow_mut().set_user_name(&text);
            app.render_form();
        });
        on!(on_host_name_edited, |app, text| {
            app.form.borrow_mut().host_name = text.to_string();
            app.render_form();
        });
        on!(on_password_typed, |app, field, text| {
            app.entry(&field).borrow_mut().push(&text);
            app.render_form();
        });
        on!(on_password_erased, |app, field| {
            app.entry(&field).borrow_mut().erase();
            app.render_form();
        });
        on!(on_password_cleared, |app, field| {
            app.entry(&field).borrow_mut().clear();
            app.render_form();
        });

        let chooser = self.window.global::<ChooserState>();
        let app = weak.clone();
        chooser.on_chosen(move |id| {
            if let Some(app) = app.upgrade() {
                app.chosen(id.to_string());
            }
        });
        let app = weak.clone();
        chooser.on_cancelled(move || {
            if let Some(app) = app.upgrade() {
                app.close_chooser();
            }
        });
        let app = weak.clone();
        chooser.on_filtered(move |typed| {
            if let Some(app) = app.upgrade() {
                app.fill_chooser(&typed, false);
            }
        });

        let dialog = self.window.global::<DialogState>();
        let app = weak.clone();
        dialog.on_confirmed(move |_| {
            if let Some(app) = app.upgrade() {
                app.window.global::<DialogState>().set_open(false);
                app.start();
            }
        });
        let app = weak.clone();
        dialog.on_cancelled(move || {
            if let Some(app) = app.upgrade() {
                app.close_dialog();
            }
        });

        // A window that could be closed in the middle would leave the
        // installation running with nobody to tell how it went.
        let app = weak;
        self.window
            .window()
            .on_close_requested(move || match app.upgrade() {
                Some(app) if app.installing.get() => slint::CloseRequestResponse::KeepWindowShown,
                _ => slint::CloseRequestResponse::HideWindow,
            });
    }
}

fn main() -> ExitCode {
    sengi_system::set_program_name("sengi-installer");
    // First, before a password can be anywhere in memory.
    secret::forbid_core_dumps();
    #[allow(unused_mut)]
    let mut demo = Demo::Off;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("sengi-installer {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            #[cfg(debug_assertions)]
            "--demo" => demo = Demo::Works,
            #[cfg(debug_assertions)]
            "--demo-fail" => demo = Demo::Fails,
            other => {
                eprintln!("sengi-installer: unknown option {other}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    let window = match InstallerWindow::new() {
        Ok(window) => window,
        Err(error) => {
            eprintln!("sengi-installer: cannot open a window: {error}");
            return ExitCode::FAILURE;
        }
    };
    sengi_config::apply_theme!(slint::ComponentHandle::global::<
        sengi_installer::ui::ThemeSwitch,
    >(&window));
    // After the first component, which is what brings the backend up, and
    // before the window is shown.
    if let Err(error) = slint::set_xdg_app_id(APP_ID) {
        eprintln!("sengi-installer: could not set the application id: {error}");
    }

    let dirs = Dirs::from_env();
    let (settings, _) = Settings::load(&dirs);
    let layout = settings
        .layouts()
        .into_iter()
        .next()
        .or_else(|| Layout::parse("us"))
        .expect("us is a layout");
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    let app = Rc::new(App {
        window,
        demo,
        page: Cell::new("welcome"),
        disks: RefCell::new(Vec::new()),
        chosen: RefCell::new(None),
        way: Cell::new(Way::Erase),
        look: RefCell::new(Look::NotYet),
        looks: Cell::new(0),
        room_gb: Cell::new(0),
        backup: Cell::new(false),
        form: RefCell::new(Form {
            host_name: model::DEFAULT_HOST_NAME.to_string(),
            ..Form::default()
        }),
        password: RefCell::new(PasswordEntry::new()),
        again: RefCell::new(PasswordEntry::new()),
        layout: RefCell::new(layout),
        // Not chosen yet: nothing is assumed about where the computer is.
        zone: RefCell::new(String::new()),
        database: RefCell::new(None),
        chooser: RefCell::new(None),
        progress: RefCell::new(Progress::default()),
        installing: Cell::new(false),
        passwd: read("/etc/passwd"),
        group: read("/etc/group"),
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    app.wire();
    let wizard = app.wizard();
    wizard.set_version(env!("CARGO_PKG_VERSION").into());
    wizard.set_host_name(model::DEFAULT_HOST_NAME.into());
    app.render_region();
    // An installation may be running already, started by a window that
    // has since gone. Then this one takes its place.
    if !app.reattach() {
        app.show("welcome");
    }

    // The names of the layouts are a few hundred kilobytes to read and
    // sort, which is not for the UI thread and not needed for the first
    // page.
    let spawned = std::thread::Builder::new()
        .name("installer-xkb".to_string())
        .spawn(|| {
            let database = Database::load();
            post(move |app| {
                *app.database.borrow_mut() = Some(database);
                app.render_region();
            });
        });
    if let Err(error) = spawned {
        eprintln!("sengi-installer: could not read the keyboard layouts: {error}");
    }

    let result = app.window.run();
    APP.with(|slot| *slot.borrow_mut() = None);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-installer: {error}");
            ExitCode::FAILURE
        }
    }
}
