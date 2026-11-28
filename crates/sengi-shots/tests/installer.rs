//! Every page of the installer, with the awkward data a disk list brings.

use sengi_shots::harness::{self, BACKDROP};
use sengi_shots::installer::*;
use slint::{ComponentHandle, ModelRc, VecModel};

fn list<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn info(title: &str, value: &str) -> InfoRow {
    InfoRow {
        title: title.into(),
        value: value.into(),
    }
}

fn disk(id: &str, title: &str, detail: &str) -> Item {
    Item {
        id: id.into(),
        title: title.into(),
        detail: detail.into(),
        icon: "hard-drives".into(),
        action: "Choose".into(),
        enabled: true,
        ..Default::default()
    }
}

fn step(text: &str, state: SengiStepState, percent: i32) -> StepRow {
    StepRow {
        text: text.into(),
        state,
        percent,
    }
}

fn steps(done: usize, failed: bool) -> ModelRc<StepRow> {
    let names = [
        "Preparing the disk",
        "Copying the system",
        "Setting up your account",
        "Installing the boot loader",
        "Finishing up",
    ];
    list(
        names
            .iter()
            .enumerate()
            .map(|(n, name)| {
                if n < done {
                    step(name, SengiStepState::Done, 100)
                } else if n == done && failed {
                    step(name, SengiStepState::Failed, -1)
                } else if n == done {
                    step(name, SengiStepState::Running, if n == 1 { 42 } else { -1 })
                } else {
                    step(name, SengiStepState::Waiting, -1)
                }
            })
            .collect(),
    )
}

fn review() -> ModelRc<InfoRow> {
    list(vec![
        info("Disk", "SanDisk SD8SN8U · 256 GB (/dev/sda)"),
        info("On it now", "One volume, Windows (NTFS) “Data”"),
        info("What happens to it", "All of it is erased"),
        info("Put on it", "Sengi OS on the whole disk, not encrypted"),
        info("Starts with", "UEFI, with a boot entry named Sengi OS"),
        info("Your account", "Allan Smith (allan)"),
        info("Computer name", "zenbook"),
        info("Keyboard layout", "French (AZERTY)"),
        info("Time zone", "Africa/Kigali"),
    ])
}

fn scene(page: &str, variant: &str, w: &InstallerWindow) {
    let g = w.global::<Wizard>();
    g.set_page(page.into());
    g.set_version("0.8.0".into());
    g.set_place("Step 2 of 4".into());
    g.set_back_label("Back".into());
    g.set_next_label("Continue".into());
    g.set_can_next(true);
    match (page, variant) {
        ("welcome", _) => {
            g.set_place("".into());
            g.set_back_label("".into());
            g.set_next_label("Start".into());
        }
        ("region", _) => {
            g.set_place("Step 1 of 4".into());
            g.set_keyboard("English (US)".into());
            g.set_zone("Africa/Kigali".into());
        }
        ("disk", v) => {
            g.set_place("Step 2 of 4".into());
            g.set_disk_note("Choose the disk to install on. Then say whether to erase it, or to install beside what is on it.".into());
            let mut disks = vec![
                disk(
                    "a",
                    "SanDisk SD8SN8U · 256 GB",
                    "/dev/sda · SATA · serial …400999 · One volume, Windows (NTFS) “Data”",
                ),
                disk(
                    "b",
                    "A disk whose maker chose a rather long model name for it · 1 TB",
                    "/dev/nvme0n1 · NVMe",
                ),
            ];
            if v == "many" {
                for n in 0..8 {
                    disks.push(disk(
                        &format!("x{n}"),
                        &format!("Disk · {} GB", 16 << (n % 4)),
                        "/dev/sdb",
                    ));
                }
            }
            if v == "none" {
                disks.clear();
                g.set_disk_note("No disk was found. If the computer has one, it may need a driver Sengi OS does not have.".into());
                g.set_can_next(false);
            }
            g.set_disks(list(disks));
            if v == "erase" || v == "beside" || v == "many" {
                g.set_disk_chosen(true);
                let mut erase = disk(
                    "erase",
                    "Erase the whole disk",
                    "Everything on the disk is removed and Sengi OS takes all of it.",
                );
                erase.action = "".into();
                erase.checked = v != "beside";
                let mut beside = disk("beside", "Install beside the existing system", "Adds one partition of 64 GB in free space. Nothing that is already on the disk is changed.");
                beside.checked = v == "beside";
                beside.icon = "".into();
                erase.icon = "".into();
                g.set_modes(list(vec![erase, beside]));
                g.set_note_warns(v != "beside");
                g.set_erase_warning(if v == "beside" {
                    "Sengi OS will be added beside what is on /dev/sda (SanDisk SD8SN8U · 256 GB), in 64 GB of free space. Nothing that is already on the disk is changed."
                } else {
                    "Everything on /dev/sda (SanDisk SD8SN8U · 256 GB) will be erased and cannot be brought back. Now on it: One volume, Windows (NTFS) “Data”."
                }.into());
            }
        }
        ("shrink", v) => {
            g.set_page("disk".into());
            g.set_place("Step 2 of 4".into());
            g.set_disk_note("Choose the disk to install on. Then say whether to erase it, or to install beside what is on it.".into());
            g.set_disks(list(vec![Item {
                checked: true,
                action: "".into(),
                ..disk(
                    "a",
                    "Micron 1100 512GB · 512 GB",
                    "/dev/sda · SATA · serial …2B3C4D · now on it: 2 partitions: 512 GB Windows (NTFS)",
                )
            }]));
            g.set_disk_chosen(true);
            let refused = v == "refused";
            let mut erase = disk(
                "erase",
                "Erase the whole disk",
                "Everything on the disk is removed and Sengi OS takes all of it.",
            );
            erase.icon = "".into();
            let mut beside = disk(
                "shrink",
                "Install beside Windows",
                match v {
                    "looking" => "Looking at Windows…",
                    "refused" => "Windows is hibernated or Fast Startup is on",
                    _ => "Makes Windows smaller to make room. It uses 184 GB of its 512 GB",
                },
            );
            beside.icon = "".into();
            beside.enabled = !refused;
            beside.checked = !refused;
            if beside.checked {
                beside.action = "".into();
            }
            g.set_modes(list(vec![erase, beside]));
            g.set_shrink_state(
                match v {
                    "looking" => "looking",
                    "refused" => "refused",
                    _ => "ready",
                }
                .into(),
            );
            g.set_shrink_words("Windows is hibernated, or Fast Startup saved part of it to disk. Changing the volume now would lose data. In Windows, turn off Fast Startup (Control Panel, Power Options, “Choose what the power buttons do”), then shut it down with Shut down, not Restart, Sleep or Hibernate.".into());
            g.set_shrink_uses("184 GB of 512 GB".into());
            g.set_shrink_range("From 11 GB to 290 GB".into());
            g.set_shrink_sengi("145 GB".into());
            g.set_shrink_windows("366 GB, with 182 GB free".into());
            g.set_shrink_min(11);
            g.set_shrink_max(290);
            g.set_shrink_gb(145);
            g.set_shrink_backup(v == "ticked");
            g.set_can_next(v == "ticked");
            g.set_note_warns(true);
            g.set_erase_warning(if v == "ready" || v == "ticked" {
                "Back up your files before you go on. Windows on /dev/sda will be made smaller, from 512 GB to 366 GB. If the power fails or the computer is switched off while that happens, Windows and your files can be lost."
            } else {
                ""
            }
            .into());
        }
        ("you", v) => {
            g.set_place("Step 3 of 4".into());
            g.set_full_name("Allan Smith".into());
            g.set_user_name("allan".into());
            g.set_host_name("zenbook".into());
            g.set_password_length(9);
            g.set_again_length(7);
            if v == "problems" {
                g.set_full_name("".into());
                g.set_user_name("Root".into());
                g.set_host_name("Zen Book".into());
                g.set_full_name_problem("Say what to call you".into());
                g.set_user_name_problem(
                    "Use lower-case letters, digits, - and _, and start with a letter".into(),
                );
                g.set_host_name_problem("A computer name has no spaces".into());
                g.set_password_problem("The two do not match".into());
                g.set_can_next(false);
            }
        }
        ("review", v) => {
            g.set_place("Step 4 of 4".into());
            g.set_next_label("Erase and install".into());
            g.set_next_erases(true);
            g.set_note_warns(true);
            g.set_erase_warning("This erases SanDisk SD8SN8U · 256 GB (/dev/sda · SATA · serial …400999), which now holds One volume, Windows (NTFS) “Data”, and cannot be undone.".into());
            g.set_review(review());
            if v == "beside" {
                g.set_next_label("Install".into());
                g.set_next_erases(false);
                g.set_note_warns(false);
            }
            if v == "shrink" {
                g.set_next_label("Shrink Windows and install…".into());
                g.set_erase_warning("Back up your files before you go on. Windows on /dev/sda will be made smaller, from 512 GB to 366 GB. If the power fails or the computer is switched off while that happens, Windows and your files can be lost.".into());
                g.set_review(list(vec![
                    info("Disk", "Micron 1100 512GB · 512 GB (/dev/sda · SATA · serial …2B3C4D)"),
                    info("On it now", "2 partitions: 512 GB Windows (NTFS), 104 MB FAT"),
                    info("Windows", "Partition 2 is made smaller, from 512 GB to 366 GB. It uses 184 GB and will have 182 GB free"),
                    info("New partition", "145 GB for Sengi OS, in the space Windows gives up, not encrypted"),
                    info("Not changed", "The other partitions. A Sengi OS folder, EFI/sengios, is added to the EFI system partition, and Sengi OS is put first in the firmware boot list"),
                    info("Backup", "You said you have one"),
                    info("Next Windows start", "Windows checks its disk first. That is expected: let it finish"),
                    info("Good to know", "If the other system uses BitLocker or a TPM, it may ask for its recovery key after the boot list changes. Have the key at hand."),
                    info("Your account", "Allan Smith (allan)"),
                    info("Computer name", "zenbook"),
                    info("Keyboard layout", "French (AZERTY)"),
                    info("Time zone", "Africa/Kigali"),
                ]));
            }
        }
        ("progress", "shrinking") => {
            g.set_place("".into());
            g.set_back_label("".into());
            g.set_next_label("".into());
            g.set_shrinking(true);
            g.set_steps(list(vec![
                step("Checking the disk and the plan", SengiStepState::Done, -1),
                step("Making Windows smaller", SengiStepState::Running, 37),
                step("Preparing the disk", SengiStepState::Waiting, -1),
                step("Making the file systems", SengiStepState::Waiting, -1),
                step("Copying Sengi OS", SengiStepState::Waiting, -1),
            ]));
        }
        ("progress", "shrink-failed") => {
            g.set_place("".into());
            g.set_back_label("".into());
            g.set_next_label("Close".into());
            g.set_outcome("failed".into());
            g.set_outcome_words("Installing on Micron 1100 512GB · 512 GB (/dev/sda) stopped at “Making Windows smaller”: ntfsresize ended with status 1: ERROR: Write failed. The Windows file system on partition 2 of /dev/sda was being made smaller and may be damaged. The partition table was not changed. Remove the USB stick, start Windows and let it check the disk; do not switch it off while it does. If Windows does not start or files are missing, restore your backup. Do not run this installer on this disk again before Windows has finished its check.".into());
            g.set_outcome_facts(list(vec![
                info("Disk", "/dev/sda"),
                info("Stopped at", "Making Windows smaller"),
                info("Full record", "/var/log/sengi-install.log"),
            ]));
            g.set_steps(list(vec![
                step("Checking the disk and the plan", SengiStepState::Done, -1),
                step("Making Windows smaller", SengiStepState::Failed, -1),
                step("Preparing the disk", SengiStepState::Waiting, -1),
            ]));
        }
        ("progress", v) => {
            g.set_place("".into());
            g.set_back_label("".into());
            g.set_next_label("".into());
            match v {
                "running" => g.set_steps(steps(1, false)),
                "done" => {
                    g.set_steps(steps(5, false));
                    g.set_outcome("done".into());
                    g.set_outcome_words("Sengi OS is on SanDisk SD8SN8U · 256 GB. Restart, take out the USB stick, and the computer starts Sengi OS.".into());
                    g.set_next_label("Restart".into());
                    g.set_outcome_facts(list(vec![
                        info("Your account", "allan"),
                        info("Computer name", "zenbook"),
                    ]));
                }
                _ => {
                    g.set_steps(steps(2, true));
                    g.set_outcome("failed".into());
                    g.set_outcome_words("Installing on SanDisk · 256 GB (/dev/sda) stopped at “Setting up your account”: useradd said no. The disk was being written and no longer holds what it did. This live session is as it was, and the installer can be run again.".into());
                    g.set_warnings("The boot loader could not be checked.".into());
                    g.set_next_label("Close".into());
                    g.set_back_label("Try again".into());
                }
            }
        }
        _ => {}
    }
}

#[test]
fn every_page() {
    harness::install();
    let scenes = [
        ("welcome", "a"),
        ("region", "a"),
        ("disk", "plain"),
        ("disk", "erase"),
        ("disk", "beside"),
        ("disk", "many"),
        ("disk", "none"),
        ("you", "ok"),
        ("you", "problems"),
        ("review", "erase"),
        ("review", "beside"),
        ("shrink", "looking"),
        ("shrink", "ready"),
        ("shrink", "ticked"),
        ("shrink", "refused"),
        ("review", "shrink"),
        ("progress", "shrinking"),
        ("progress", "shrink-failed"),
        ("progress", "running"),
        ("progress", "done"),
        ("progress", "failed"),
    ];
    for (page, variant) in scenes {
        for (width, height) in [(720u32, 480u32), (888, 600), (1280, 800)] {
            let w = themed(InstallerWindow::new().unwrap());
            // As the program does before it changes the page.
            w.invoke_release();
            scene(page, variant, &w);
            w.show().unwrap();
            let win = harness::window();
            harness::resize(&win, width, height);
            harness::advance(300);
            harness::draw(&win, BACKDROP)
                .save(&format!("installer-{page}-{variant}-{width}x{height}.png"));
            w.hide().unwrap();
        }
    }
}
