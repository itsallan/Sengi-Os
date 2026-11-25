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
        ("progress", "running"),
        ("progress", "done"),
        ("progress", "failed"),
    ];
    for (page, variant) in scenes {
        for (width, height) in [(720u32, 480u32), (888, 600), (1280, 800)] {
            let w = InstallerWindow::new().unwrap();
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
