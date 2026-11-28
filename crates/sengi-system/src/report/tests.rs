//! The whole report over a made-up machine that has everything personal a
//! real one could, in every place it could show up.

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::Instant;

use super::*;
use crate::bus::testing::{object, text};
use crate::bus::Prop;
use crate::testutil::TempDir;

/// A program on the made-up machine's path. A shell writes it, so that
/// this process never holds it open for writing while another test starts
/// a program of its own.
fn program(machine: &TempDir, name: &str, script: &str) {
    let path = machine.path().join("bin").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let status = Command::new("/bin/sh")
        .args([
            "-c",
            "printf '#!/bin/sh\\n%s\\n' \"$2\" >\"$1\" && chmod 755 \"$1\"",
            "sh",
        ])
        .arg(&path)
        .arg(script)
        .status()
        .unwrap();
    assert!(status.success());
}

/// A program that prints `text` as it stands.
fn prints(machine: &TempDir, name: &str, text: &str) {
    machine.write(&format!("says/{name}"), text);
    let said = machine.path().join("says").join(name);
    program(machine, name, &format!("/bin/cat '{}'", said.display()));
}

fn sources(machine: &TempDir) -> Sources {
    let at = |name: &str| machine.path().join(name);
    Sources {
        proc: at("proc"),
        sys: at("sys"),
        etc: at("etc"),
        run: at("run"),
        path: at("bin").display().to_string(),
        user: Some("wanjiru".to_string()),
        home: "/home/wanjiru".into(),
    }
}

fn machine() -> TempDir {
    let machine = TempDir::new();
    for (path, contents) in [
        (
            "etc/passwd",
            "root:x:0:0:root:/root:/bin/bash\n\
             wanjiru:x:1000:1000:Wanjiru Otieno,,,:/home/wanjiru:/bin/bash\n",
        ),
        ("etc/os-release", "PRETTY_NAME=\"Sengi OS 1.0.0\"\n"),
        ("proc/sys/kernel/hostname", "otieno-zenbook\n"),
        (
            "proc/cmdline",
            "BOOT_IMAGE=/boot/vmlinuz root=UUID=0b6f3a1e-9d2c-4e7a-8f51-2c9a7e4d1b30 ro quiet \
             systemd.hostname=otieno-zenbook\n",
        ),
        (
            "proc/meminfo",
            "MemTotal: 16282280 kB\nMemAvailable: 15900000 kB\n",
        ),
        (
            "proc/mounts",
            "/dev/sda2 / ext4 rw,relatime 0 0\n\
             /dev/sdb1 /media/wanjiru/HOLIDAY\\040PICS vfat rw 0 0\n\
             //192.168.1.9/Otieno-Share /mnt/share cifs rw 0 0\n",
        ),
        ("sys/class/dmi/id/product_name", "ZenBook UX410UQK\n"),
        ("sys/class/dmi/id/product_serial", "J8N0CV12345\n"),
        ("sys/bus/usb/devices/1-1/idVendor", "18d1\n"),
        ("sys/bus/usb/devices/1-1/idProduct", "4ee1\n"),
        ("sys/bus/usb/devices/1-1/product", "Wanjiru's Phone\n"),
        ("sys/bus/usb/devices/1-1/serial", "R58M123ABCD\n"),
        (
            "proc/asound/cards",
            " 0 [PCH            ]: HDA-Intel - HDA Intel PCH\n",
        ),
        ("sys/class/net/enx3c58c29a01fe/device/uevent", ""),
        ("sys/class/net/enx3c58c29a01fe/operstate", "up\n"),
        (
            "sys/class/net/enx3c58c29a01fe/address",
            "3c:58:c2:9a:01:fe\n",
        ),
        (
            "sys/class/power_supply/hid-3c:58:c2:9a:01:fe-battery/type",
            "Battery\n",
        ),
        (
            "sys/class/power_supply/hid-3c:58:c2:9a:01:fe-battery/model_name",
            "Njeri Speaker\n",
        ),
        (
            "sys/class/power_supply/hid-3c:58:c2:9a:01:fe-battery/serial_number",
            "BATSER99821\n",
        ),
        ("sys/block/sda/size", "500118192\n"),
        ("sys/block/sda/device/model", "SanDisk SD8SN8U2\n"),
        ("sys/block/sda/device/serial", "172345800123\n"),
    ] {
        machine.write(path, contents);
    }
    prints(
        &machine,
        "journalctl",
        "[    1.000000] kernel: iwlwifi 0000:01:00.0: Direct firmware load for iwlwifi-8265-36.ucode failed with error -2\n\
         [    1.100000] kernel: usb 1-1: SerialNumber: R58M123ABCD\n\
         [    1.200000] kernel: EXT4-fs (sda2): mounted filesystem 0b6f3a1e-9d2c-4e7a-8f51-2c9a7e4d1b30 firmware\n\
         [    2.000000] kernel: wlan0: authenticate with 3c:58:c2:9a:01:fe\n\
         [    3.000000] systemd-networkd[301]: wlan0: Connected WiFi access point: Otieno Family WiFi (3c:58:c2:9a:01:fe)\n\
         [    3.100000] iwd[400]: could not join Cafe Nyumbani\n\
         [    3.200000] systemd-networkd[301]: wlan0: DHCPv4 address 192.168.1.23/24, gateway 192.168.1.1\n\
         [    3.300000] systemd-networkd[301]: wlan0: Gained fe80::3e58:c2ff:fe9a:1fe\n\
         [    4.000000] bluetoothd[500]: WH-Wanjiru: profile failed for /org/bluez/hci0/dev_3C_58_C2_9A_01_FE\n\
         [    5.000000] systemd[1]: Started session of user wanjiru (Wanjiru Otieno) on otieno-zenbook.\n\
         [    5.100000] files[9]: cannot read /home/wanjiru/Documents/medical letter.pdf: denied\n\
         [    5.200000] udisksd[600]: Mounted /dev/sdb1 at /media/wanjiru/HOLIDAY PICS on behalf of uid 1000\n",
    );
    prints(
        &machine,
        "wpctl",
        "PipeWire 'pipewire-0' [1.4.2, wanjiru@otieno-zenbook, cookie:907041567]\n \
         └─ Clients:\n        33. WirePlumber    [1.4.2, wanjiru@otieno-zenbook, pid:492]\n\n\
         Audio\n ├─ Devices:\n │      42. Built-in Audio    [alsa]\n \
         │      60. Baraka's Buds     [bluez5]\n │  \n ├─ Sinks:\n \
         │  *   61. Baraka's Buds     [vol: 0.40]\n │  \n ├─ Sources:\n │  \n \
         └─ Streams:\n        70. Secret Song - Player\n\n\
         Settings\n └─ Default Configured Devices:\n         \
         0. Audio/Sink    alsa_output.usb-Jabra_Link_370_305075A1B2C3-00.analog-stereo\n         \
         1. Audio/Source  bluez_input.3C:58:C2:9A:01:FE\n",
    );
    program(
        &machine,
        "systemctl",
        "case \"$*\" in\n\
         is-active*) printf 'active\\ninactive\\nactive\\nactive\\n'; exit 3 ;;\n\
         *) printf '%s\\n' 'media-wanjiru-HOLIDAY\\x20PICS.mount loaded failed failed /media/wanjiru/HOLIDAY PICS' \
         'systemd-fsck@dev-disk-by\\x2duuid-9A3C\\x2d11FE.service loaded failed failed check' ;;\n\
         esac",
    );
    machine
}

fn radios() -> Radios {
    Radios {
        wifi: Listing::Objects(vec![
            object(
                "/net/connman/iwd/0/3",
                IWD_DEVICE,
                &[
                    ("Name", text("wlan0")),
                    ("Powered", Prop::Bool(true)),
                    ("Mode", text("station")),
                ],
            ),
            object(
                "/net/connman/iwd/0/3/a",
                IWD_NETWORK,
                &[("Name", text("Otieno Family WiFi"))],
            ),
            object(
                "/net/connman/iwd/b",
                IWD_KNOWN,
                &[("Name", text("Cafe Nyumbani"))],
            ),
        ]),
        bluetooth: Listing::Objects(vec![
            object(
                "/org/bluez/hci0",
                crate::bluez::ADAPTER,
                &[
                    ("Alias", text("otieno-zenbook")),
                    ("Powered", Prop::Bool(true)),
                ],
            ),
            object(
                "/org/bluez/hci0/dev_3C_58_C2_9A_01_FE",
                crate::bluez::DEVICE,
                &[
                    ("Name", text("WH-Wanjiru")),
                    ("Alias", text("Njeri Speaker")),
                    ("Address", text("3C:58:C2:9A:01:FE")),
                    ("Icon", text("audio-headset")),
                    ("Paired", Prop::Bool(true)),
                    ("Connected", Prop::Bool(true)),
                ],
            ),
        ]),
    }
}

const SECRETS: &[&str] = &[
    // The account and the person.
    "wanjiru",
    "otieno",
    // Files, drives and shares.
    "medical",
    "holiday",
    // Networks and devices.
    "nyumbani",
    "family wifi",
    "njeri",
    "baraka",
    // Addresses.
    "3c:58",
    "3c_58",
    "3c58c2",
    "192.168",
    "fe80",
    // Serial numbers and identifiers.
    "R58M123",
    "J8N0CV",
    "BATSER",
    "172345800123",
    "305075A1",
    "0b6f3a1e",
    "2c9a7e4d",
    "9A3C",
    // What is playing.
    "Secret Song",
];

#[test]
fn nothing_personal_reaches_the_file() {
    let machine = machine();
    let report = collect(&sources(&machine), radios, "2026-11-28 14:03");
    let low = report.to_lowercase();
    for secret in SECRETS {
        let secret = secret.to_lowercase();
        let line = low.lines().find(|line| line.contains(&secret));
        assert_eq!(line, None, "{secret:?} is in the report");
    }
    // And what helps is there.
    for wanted in [
        "Made 2026-11-28 14:03",
        "Operating system: Sengi OS 1.0.0",
        "Running: an installed system",
        "ZenBook UX410UQK",
        "Direct firmware load for iwlwifi-8265-36.ucode failed with error -2",
        "SanDisk SD8SN8U2",
        "18d1:4ee1",
        "root=UUID=<uuid>",
        "SerialNumber: <serial>",
        "Connected WiFi access point: <network> (<mac>)",
        "could not join <network>",
        "DHCPv4 address <ip>/24",
        "cannot read ~/<file>: denied",
        "<bluetooth-device>: profile failed",
        "enx<mac>: wired or modem",
        "alsa_output.usb-<usb-device>-00.analog-stereo",
        "connected: a device of the kind audio-headset",
        "networks in range: 1, networks remembered: 1",
        "iwd.service: active",
        "bluetooth.service: inactive",
        "a network or user file system (cifs, rw), not measured",
        "Everything asked for was read.",
    ] {
        assert!(report.contains(wanted), "no {wanted:?} in:\n{report}");
    }
}

#[test]
fn what_cannot_be_read_is_said_to_be() {
    let machine = TempDir::new();
    program(
        &machine,
        "journalctl",
        "echo 'Hint: You are currently not seeing messages from other users and the system.' >&2\n\
         echo 'No journal files were opened due to insufficient permissions.' >&2\n\
         exit 1",
    );
    program(
        &machine,
        "dmesg",
        "echo 'dmesg: read kernel buffer failed: Operation not permitted' >&2; exit 1",
    );
    let report = collect(&sources(&machine), Radios::silent, "today");
    let top = report.split("\n== System ==").next().unwrap();
    for wanted in [
        "Errors and warnings, from the journal: refused",
        "Errors and warnings, from the kernel (dmesg): refused",
        "Kernel messages about firmware, from the journal: refused",
        "PipeWire's devices: wpctl is not installed",
        "System services: systemctl is not installed",
        "Sound cards: there is no /proc/asound/cards",
    ] {
        assert!(top.contains(wanted), "no {wanted:?} in:\n{report}");
    }
    assert!(report.contains("this account is not allowed to read the system log"));
    assert!(report.contains("Operating system: not available"));
}

#[test]
fn the_kernel_buffer_stands_in_for_a_closed_journal() {
    let machine = TempDir::new();
    program(
        &machine,
        "journalctl",
        "echo 'No journal files were opened due to insufficient permissions.' >&2; exit 1",
    );
    prints(
        &machine,
        "dmesg",
        "[    0.5] microcode: updated early\n[    0.9] ata1: link is slow\n\
         [    1.0] systemd[1]: skipped /sys/firmware/efi/efivars/Thing\n",
    );
    let report = collect(&sources(&machine), Radios::silent, "today");
    assert!(report.contains("From the kernel's buffer (dmesg) instead:"));
    assert!(report.contains("ata1: link is slow"), "{report}");
    // Once, among the errors, and not as news about firmware.
    assert_eq!(report.matches("efivars/Thing").count(), 1, "{report}");
    assert_eq!(report.matches("microcode: updated early").count(), 2);
}

#[test]
fn messages_with_names_nobody_could_list_are_left_out() {
    let machine = TempDir::new();
    prints(
        &machine,
        "journalctl",
        "[ 1.0] iwd[1]: joined Unheard Of Cafe\n\
         [ 2.0] bluetoothd[2]: Somebody Earbuds gone\n\
         [ 3.0] kernel: something broke\n",
    );
    let report = collect(&sources(&machine), Radios::silent, "today");
    assert!(!report.contains("Unheard Of"), "{report}");
    assert!(!report.contains("Somebody Earbuds"), "{report}");
    assert!(report.contains("something broke"));
    assert!(report.contains("[2 messages of Wi-Fi or Bluetooth services left out"));
}

#[test]
fn a_section_or_a_service_that_hangs_does_not_hold_the_rest() {
    let started = Instant::now();
    let out = within(Duration::from_millis(100), || {
        thread::sleep(Duration::from_secs(30));
        Out::default()
    });
    assert!(out.text.contains("did not finish"), "{}", out.text);
    assert_eq!(out.unread.len(), 1);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn only_the_sound_devices_are_taken_from_wpctl() {
    let (tree, names) = sections::sound_tree(
        "PipeWire 'pipewire-0' [1.4.2, a@b, cookie:1]\n └─ Clients:\n        33. x\n\n\
         Audio\n ├─ Devices:\n │      60. Some [Odd] Buds   [bluez5]\n │  \n └─ Streams:\n        \
         70. a song\n\nVideo\n ├─ Devices:\n │      80. Camera [v4l2]\n",
    );
    assert!(tree.starts_with("Audio\n"));
    assert!(tree.contains("Camera"), "{tree}");
    assert!(!tree.contains("a@b") && !tree.contains("a song"), "{tree}");
    assert_eq!(names, ["Some [Odd] Buds"]);
    assert_eq!(sections::sound_tree("Could not connect\n").0, "");
}

#[test]
fn a_report_is_never_written_over() {
    let dir = TempDir::new();
    let documents = folder(dir.path());
    let first = save("one", &documents, "2026-11-28").unwrap();
    assert_eq!(first, documents.join("sengi-os-report-2026-11-28.txt"));
    let second = save("two", &documents, "2026-11-28").unwrap();
    assert_eq!(second, documents.join("sengi-os-report-2026-11-28-2.txt"));
    // A link that leads nowhere is in the way like any file.
    std::os::unix::fs::symlink(
        "/nonexistent/target",
        documents.join("sengi-os-report-2026-11-28-3.txt"),
    )
    .unwrap();
    let fourth = save("four", &documents, "2026-11-28").unwrap();
    assert_eq!(fourth, documents.join("sengi-os-report-2026-11-28-4.txt"));
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "one");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "two");
    let mode = std::fs::metadata(&first).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    // A folder that cannot be made is an error and not a crash.
    dir.write("file", "");
    assert!(save("x", &dir.path().join("file/Documents"), "d").is_err());
}

/// The report of the machine the tests run on, saved in `$HOME/Documents`
/// and printed, to be read through by a person:
/// `cargo test -p sengi-system --features settings -- --ignored --nocapture this_machine`
#[test]
#[ignore]
fn the_report_of_this_machine() {
    let started = Instant::now();
    let path = make().unwrap();
    println!("saved {} in {:?}", path.display(), started.elapsed());
    println!("{}", std::fs::read_to_string(path).unwrap());
}
