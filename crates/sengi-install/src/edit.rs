//! The files that make a copy of the live image an installed system, as
//! text: each function here takes what a file says and returns what it
//! should say. Nothing in this module touches a disk.

use sengi_config::settings::Layout;
use sengi_config::{Document, Value};

/// The partition table: a place for GRUB on a machine started the old
/// way, an EFI system partition for one started the new way, and the
/// rest for Sengi OS. Every disk gets all three, so it starts on either
/// kind of firmware. Fed to sfdisk.
pub const PARTITIONS: &str = "\
label: gpt
size=1MiB, type=21686148-6449-6E6F-744E-656564454649, name=\"BIOS boot\"
size=512MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name=\"EFI system\"
type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name=\"Sengi OS\"
";
/// Which partition is which in that table.
pub const BIOS_PART: u32 = 1;
pub const EFI_PART: u32 = 2;
pub const ROOT_PART: u32 = 3;

/// Where that table puts the partitions, in sectors of 512 bytes: sfdisk
/// starts the first at one mebibyte and sets each against the last.
const MEBIBYTE: u64 = 2048;
const BIOS_START: u64 = MEBIBYTE;
const EFI_START: u64 = 2 * MEBIBYTE;
const EFI_SECTORS: u64 = 512 * MEBIBYTE;
const ROOT_START: u64 = EFI_START + EFI_SECTORS;

/// Whether the partitions the kernel shows (number, first sector, length)
/// are the ones [`PARTITIONS`] asks for on a disk of `disk_sectors`. The
/// last partition takes the rest, less the copy of the table at the end
/// and what alignment leaves over, so it is held to a range.
pub fn table_is_as_asked(seen: &[(u32, u64, u64)], disk_sectors: u64) -> Result<(), String> {
    let mut seen = seen.to_vec();
    seen.sort();
    if seen.len() != 3 {
        return Err(format!("it shows {} partitions, not 3", seen.len()));
    }
    let fixed = [
        (BIOS_PART, BIOS_START, MEBIBYTE),
        (EFI_PART, EFI_START, EFI_SECTORS),
    ];
    for (asked, found) in fixed.iter().zip(&seen) {
        if asked != found {
            return Err(format!(
                "partition {} starts at sector {} and has {} sectors, not {} and {}",
                found.0, found.1, found.2, asked.1, asked.2
            ));
        }
    }
    let (number, start, length) = seen[2];
    let most = disk_sectors.saturating_sub(ROOT_START);
    let least = most.saturating_sub(2 * MEBIBYTE);
    if number != ROOT_PART || start != ROOT_START || length > most || length < least || length == 0
    {
        return Err(format!(
            "partition {number} starts at sector {start} and has {length} sectors, not {ROOT_START} and the rest of the disk"
        ));
    }
    Ok(())
}

/// What GRUB is told. The menu is hidden, and waits one second for Esc,
/// which shows it. With no wait at all, as on the live image, GRUB looks
/// once for a key already pressed: that was tried, and under UEFI the
/// key was never there to be found, so the menu could not be reached at
/// all. A second is what a way in costs. The kernel is as quiet as the
/// live image's. There is no recovery entry: it would ask for root's
/// password, and root has none; [`GRUB_TEXT_ENTRY`] is in its place.
pub const GRUB_DEFAULT: &str = "\
# Written by the Sengi OS installer. Run update-grub after changing it.
GRUB_DEFAULT=0
GRUB_TIMEOUT_STYLE=hidden
GRUB_TIMEOUT=1
GRUB_DISTRIBUTOR=\"Sengi OS\"
GRUB_CMDLINE_LINUX_DEFAULT=\"quiet loglevel=3 systemd.show_status=false rd.udev.log_level=3\"
GRUB_CMDLINE_LINUX=\"\"
GRUB_TERMINAL=console
GRUB_DISABLE_OS_PROBER=true
GRUB_DISABLE_RECOVERY=true
";

/// What GRUB is told on a disk that holds another system: the same, with
/// the search for other systems switched on, so that Windows and the like
/// are in the menu.
pub fn grub_default_beside() -> String {
    GRUB_DEFAULT.replace(
        "GRUB_DISABLE_OS_PROBER=true",
        "GRUB_DISABLE_OS_PROBER=false",
    )
}

/// `text` with the search for other systems told to leave out the file
/// systems with these identifiers.
pub fn with_skip_list(text: &str, uuids: &[String]) -> String {
    format!("{text}GRUB_OS_PROBER_SKIP_LIST=\"{}\"\n", uuids.join(" "))
}

/// The same with the menu shown for a few seconds, which is how a person
/// gets to the other system when there is one. Used only where one was
/// found: a disk with nothing else on it keeps its hidden menu, which is
/// quicker.
pub fn grub_with_menu(text: &str) -> String {
    text.replace("GRUB_TIMEOUT_STYLE=hidden", "GRUB_TIMEOUT_STYLE=menu")
        .replace("GRUB_TIMEOUT=1\n", "GRUB_TIMEOUT=3\n")
}

/// The entries `efibootmgr -v` lists under `label` that start `loader` from
/// the partition with this partition identifier (GPT): ours, on this EFI
/// system partition, and no other entry that happens to have the name.
pub fn boot_entries_at(listing: &str, label: &str, partuuid: &str, loader: &str) -> Vec<String> {
    let partuuid = partuuid.to_ascii_lowercase();
    let loader = loader.to_ascii_lowercase();
    listing
        .lines()
        .filter(|line| {
            let line = line.to_ascii_lowercase();
            !partuuid.is_empty()
                && line.contains(&format!("gpt,{partuuid}"))
                && line.contains(&loader)
        })
        .flat_map(|line| boot_entries(line, label))
        .collect()
}

/// Whether GRUB's menu has an entry that os-prober found: something
/// between the markers of its script.
pub fn menu_has_other_systems(menu: &str) -> bool {
    let mut inside = false;
    for line in menu.lines() {
        if line.starts_with("### BEGIN") && line.contains("30_os-prober") {
            inside = true;
        } else if line.starts_with("### END") && line.contains("30_os-prober") {
            inside = false;
        } else if inside && line.trim_start().starts_with("menuentry ") {
            return true;
        }
    }
    false
}

/// The order the firmware tries its entries in, from what `efibootmgr`
/// prints: `BootOrder: 0003,0001,0000`.
pub fn boot_order(listing: &str) -> Vec<String> {
    listing
        .lines()
        .find_map(|line| line.strip_prefix("BootOrder:"))
        .map(|order| {
            order
                .split(',')
                .map(|entry| entry.trim().to_string())
                .filter(|entry| !entry.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The word on the kernel's command line that says "without the desktop".
pub const TEXT_MODE_FLAG: &str = "sengi.text";

/// The word that says "start the desktop on labwc, not on sengi-compositor".
pub const LABWC_FLAG: &str = "sengi.compositor=labwc";

/// A script for /etc/grub.d: two more entries in GRUB's menu, written anew
/// whenever update-grub runs, for the newest kernel there is. The first
/// starts the desktop on labwc, the way back when sengi-compositor, the
/// default, does not work with the graphics. The second starts
/// the system as far as multi-user and no further: no login screen, no
/// desktop, and a text login on the first console, where the person can
/// log in with their own name and password and use sudo. Root is locked,
/// so the usual recovery entry, which asks for root's password, would
/// let nobody in.
pub const GRUB_TEXT_ENTRY: &str = r#"#!/bin/sh
# Written by the Sengi OS installer. Adds two entries to the boot menu.
# "Sengi OS (labwc compositor)" is the desktop on labwc instead of
# sengi-compositor, for when the graphics do not work with the default.
# "Sengi OS, text mode" is the same kernel started without the login screen
# and the desktop, with a text login on the first console, for when the
# desktop does not start at all. Log in there with your own name and
# password.
set -e
. "$pkgdatadir/grub-mkconfig_lib"

kernel=$(ls -1 /boot/vmlinuz-* 2>/dev/null | sort -V | tail -n 1)
[ -n "$kernel" ] || exit 0
version=${kernel#/boot/vmlinuz-}
[ -e "/boot/initrd.img-$version" ] || exit 0

echo "Found labwc and text mode entries for: $kernel" >&2
echo "menuentry 'Sengi OS (labwc compositor)' --class sengi --class gnu-linux --class os {"
prepare_grub_to_access_device "$GRUB_DEVICE" | sed 's/^/	/'
printf '\tlinux\t/boot/vmlinuz-%s root=UUID=%s ro %s %s sengi.compositor=labwc\n' "$version" "$GRUB_DEVICE_UUID" "$GRUB_CMDLINE_LINUX_DEFAULT" "$GRUB_CMDLINE_LINUX"
printf '\tinitrd\t/boot/initrd.img-%s\n' "$version"
echo "}"
echo "menuentry 'Sengi OS, text mode (if the desktop does not start)' --class sengi --class gnu-linux --class os {"
prepare_grub_to_access_device "$GRUB_DEVICE" | sed 's/^/	/'
printf '\tlinux\t/boot/vmlinuz-%s root=UUID=%s ro sengi.text systemd.unit=multi-user.target\n' "$version" "$GRUB_DEVICE_UUID"
printf '\tinitrd\t/boot/initrd.img-%s\n' "$version"
echo "}"
"#;

/// A drop-in for getty@tty1: the text login on the first console runs
/// only when the kernel was started with [`TEXT_MODE_FLAG`]. On every
/// other start it costs one condition that is not met.
pub const TEXT_LOGIN: &str = "\
# Written by the Sengi OS installer. The text login on the first console
# is for the \"text mode\" entry of the boot menu and nothing else.
[Unit]
ConditionKernelCommandLine=sengi.text
";

/// The directory on the EFI system partition, and the name in the
/// firmware's list.
pub const EFI_ID: &str = "sengios";
pub const EFI_LABEL: &str = "Sengi OS";

/// The file systems to mount, by their own identifiers: a disk's name
/// can change from one start to the next.
pub fn fstab(root_uuid: &str, efi_uuid: &str) -> String {
    format!(
        "# Written by the Sengi OS installer.\n\
         UUID={root_uuid} / ext4 errors=remount-ro,noatime 0 1\n\
         UUID={efi_uuid} /boot/efi vfat umask=0077 0 2\n"
    )
}

pub fn hosts(host_name: &str) -> String {
    format!(
        "127.0.0.1\tlocalhost\n127.0.1.1\t{host_name}\n::1\tlocalhost ip6-localhost ip6-loopback\n"
    )
}

/// greetd's configuration without the section that logs the live user
/// in, and without the comment that introduces it. `None` if there is no
/// such section: then the file is not the one this was written for, and
/// leaving it alone could leave a machine that logs somebody in.
pub fn greetd_without_autologin(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.trim() == "[initial_session]")?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .map_or(lines.len(), |offset| start + 1 + offset);
    // The comment lines straight above the section belong to it.
    let mut from = start;
    while from > 0 && lines[from - 1].trim_start().starts_with('#') {
        from -= 1;
    }
    let mut kept: Vec<&str> = lines[..from].to_vec();
    kept.extend_from_slice(&lines[end..]);
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    let mut out = kept.join("\n");
    out.push('\n');
    Some(out)
}

/// The dock's list of pinned apps without `app`. Other lines are kept
/// as they are.
pub fn dock_without(text: &str, app: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let pinned = line
            .split_once('=')
            .filter(|(key, _)| key.trim() == "pinned")
            .and_then(|(_, list)| list.trim().strip_prefix('[')?.strip_suffix(']'));
        match pinned {
            Some(list) => {
                let kept: Vec<&str> = list
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty() && item.trim_matches('"') != app)
                    .collect();
                out.push_str(&format!("pinned = [{}]", kept.join(", ")));
            }
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// The environment of the login screen's compositor with the keyboard
/// layout in it. The login screen follows the system's layout, and this
/// is where its compositor is told what that is.
pub fn greeter_environment(text: &str, layout: &Layout) -> String {
    let mut out: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("XKB_DEFAULT_"))
        .map(|line| format!("{line}\n"))
        .collect();
    out.push_str(&format!("XKB_DEFAULT_LAYOUT={}\n", layout.layout));
    if !layout.variant.is_empty() {
        out.push_str(&format!("XKB_DEFAULT_VARIANT={}\n", layout.variant));
    }
    out
}

/// The system-wide settings file with `layout` as the keyboard layout of
/// everyone who has not chosen their own. What else the file says stays.
pub fn system_settings(text: &str, layout: &Layout) -> String {
    let (mut document, _) = Document::parse(text);
    document.set("keyboard", "layouts", Value::List(vec![layout.render()]));
    document.render()
}

/// The groups in a file of the form of /etc/group that `user` is a
/// member of, in the file's order.
pub fn groups_of(group: &str, user: &str) -> Vec<String> {
    group
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let members = fields.nth(2)?;
            members
                .split(',')
                .any(|member| member.trim() == user)
                .then(|| name.to_string())
        })
        .collect()
}

/// The password field of `user` in a file of the form of /etc/shadow.
pub fn shadow_field<'a>(shadow: &'a str, user: &str) -> Option<&'a str> {
    shadow.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next() == Some(user))
            .then(|| fields.next())
            .flatten()
    })
}

/// Whether a shadow password field lets nobody in by password.
pub fn is_locked(field: &str) -> bool {
    field.is_empty() || field.starts_with('!') || field.starts_with('*')
}

/// The boot entries in what `efibootmgr` prints that carry `label`:
/// their four hexadecimal digits.
pub fn boot_entries(listing: &str, label: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("Boot")?;
            let (number, name) = rest.split_once(char::is_whitespace)?;
            let number = number.trim_end_matches('*');
            let fine = number.len() == 4 && number.chars().all(|c| c.is_ascii_hexdigit());
            // Newer versions print the device path after a tab.
            let name = name.split('\t').next().unwrap_or_default().trim();
            (fine && name == label).then(|| number.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GREETD: &str = "\
[terminal]
vt = 7

# The login screen, sengi-greeter, in a compositor of its own. It is shown
# whenever nobody is logged in.
[default_session]
command = \"/usr/libexec/sengi-greeter-session\"
user = \"_greetd\"

# The live image logs straight in. The installer will drop this section.
[initial_session]
command = \"/usr/bin/sengi-session\"
user = \"live\"
";

    #[test]
    fn the_automatic_login_is_taken_out_of_greetds_configuration() {
        let without = greetd_without_autologin(GREETD).unwrap();
        assert!(!without.contains("initial_session"));
        assert!(!without.contains("live"));
        assert!(!without.contains("installer will drop"));
        assert!(without.contains("[default_session]"));
        assert!(without.contains("user = \"_greetd\""));
        assert!(without.contains("# The login screen"));
        assert!(without.ends_with("user = \"_greetd\"\n"));
        // What is left is still read as it was.
        let (document, warnings) = Document::parse(&without);
        assert!(warnings.is_empty());
        assert_eq!(document.get("terminal", "vt"), Some(&Value::Number(7)));
    }

    #[test]
    fn a_section_after_the_automatic_login_is_kept() {
        let text = "[initial_session]\nuser = \"live\"\n\n[terminal]\nvt = 7\n";
        assert_eq!(
            greetd_without_autologin(text).unwrap(),
            "[terminal]\nvt = 7\n"
        );
    }

    #[test]
    fn a_configuration_without_the_section_is_not_guessed_at() {
        assert_eq!(greetd_without_autologin("[terminal]\nvt = 7\n"), None);
        assert_eq!(greetd_without_autologin(""), None);
        assert_eq!(greetd_without_autologin("# [initial_session]\n"), None);
    }

    #[test]
    fn the_installer_is_taken_off_the_dock() {
        let dock =
            "# Sengi OS dock.\n\npinned = [\"sengi-installer\", \"foot\", \"sengi-settings\"]\n";
        assert_eq!(
            dock_without(dock, "sengi-installer"),
            "# Sengi OS dock.\n\npinned = [\"foot\", \"sengi-settings\"]\n"
        );
        let without = "pinned = [\"foot\"]\n";
        assert_eq!(dock_without(without, "sengi-installer"), without);
        assert_eq!(
            dock_without("pinned = [\"sengi-installer\"]\n", "sengi-installer"),
            "pinned = []\n"
        );
        // A name that only starts the same is another app.
        let other = "pinned = [\"sengi-installer-extras\"]\n";
        assert_eq!(dock_without(other, "sengi-installer"), other);
    }

    #[test]
    fn file_systems_are_mounted_by_their_identifiers() {
        let table = fstab("1111-aaaa", "ABCD-1234");
        assert!(table.contains("UUID=1111-aaaa / ext4 "));
        assert!(table.contains("UUID=ABCD-1234 /boot/efi vfat "));
        assert!(!table.contains("/dev/"));
        assert!(!table.contains("swap"));
    }

    #[test]
    fn the_computer_knows_its_own_name() {
        assert!(hosts("zenbook").contains("127.0.1.1\tzenbook\n"));
    }

    #[test]
    fn the_login_screen_is_told_the_keyboard_layout() {
        let layout = Layout::parse("fr(azerty)").unwrap();
        assert_eq!(
            greeter_environment("XCURSOR_SIZE=24\n", &layout),
            "XCURSOR_SIZE=24\nXKB_DEFAULT_LAYOUT=fr\nXKB_DEFAULT_VARIANT=azerty\n"
        );
        // Told again, it is told once.
        let again = greeter_environment(
            "XCURSOR_SIZE=24\nXKB_DEFAULT_LAYOUT=fr\nXKB_DEFAULT_VARIANT=azerty\n",
            &Layout::parse("us").unwrap(),
        );
        assert_eq!(again, "XCURSOR_SIZE=24\nXKB_DEFAULT_LAYOUT=us\n");
    }

    #[test]
    fn the_system_wide_settings_get_the_keyboard_layout() {
        let layout = Layout::parse("de(nodeadkeys)").unwrap();
        assert_eq!(
            system_settings("", &layout),
            "[keyboard]\nlayouts = [\"de(nodeadkeys)\"]\n"
        );
        let kept = system_settings("[clock]\nformat = \"12h\"\n", &layout);
        assert!(kept.contains("format = \"12h\""));
        let (document, _) = Document::parse(&kept);
        assert_eq!(
            document.get("keyboard", "layouts"),
            Some(&Value::List(vec!["de(nodeadkeys)".to_string()]))
        );
    }

    #[test]
    fn the_new_account_gets_the_live_users_groups() {
        let group = "root:x:0:\nsudo:x:27:live\naudio:x:29:pulse,live\nvideo:x:44:live\nlive:x:1000:\nusers:x:100:alive,liver\nbroken\n";
        assert_eq!(groups_of(group, "live"), ["sudo", "audio", "video"]);
        assert!(groups_of(group, "nobody").is_empty());
    }

    #[test]
    fn shadow_says_who_has_a_password() {
        let shadow = "root:!:20000:0:99999:7:::\nallan:$y$j9T$abc:20000:0:99999:7:::\n_greetd:*:20000::::::\n";
        assert!(is_locked(shadow_field(shadow, "root").unwrap()));
        assert!(is_locked(shadow_field(shadow, "_greetd").unwrap()));
        assert!(!is_locked(shadow_field(shadow, "allan").unwrap()));
        assert_eq!(shadow_field(shadow, "live"), None);
        assert!(is_locked(""));
        assert!(is_locked("!$y$j9T$abc"));
    }

    #[test]
    fn the_firmwares_entries_for_sengi_os_are_found() {
        let listing = "BootCurrent: 0001\nTimeout: 0 seconds\nBootOrder: 0003,0001,0000\n\
            Boot0000* Windows Boot Manager\tHD(1,GPT,...)\n\
            Boot0001* Sengi OS\tHD(2,GPT,...)/File(\\EFI\\sengios\\grubx64.efi)\n\
            Boot0003  Sengi OS\n\
            Boot0004* Sengi OS old\n\
            BootFFFZ* Sengi OS\n";
        assert_eq!(boot_entries(listing, "Sengi OS"), ["0001", "0003"]);
        assert!(boot_entries("", "Sengi OS").is_empty());
    }

    #[test]
    fn the_kernels_view_of_the_table_is_held_to_what_was_asked() {
        // A 12 GiB disk, as sfdisk lays it out.
        let disk = 25_165_824;
        let good = [
            (1, 2048, 2048),
            (2, 4096, 1_048_576),
            (3, 1_052_672, 24_111_104),
        ];
        assert_eq!(table_is_as_asked(&good, disk), Ok(()));
        // In any order.
        assert_eq!(
            table_is_as_asked(&[good[2], good[0], good[1]], disk),
            Ok(())
        );
        // The old table: two partitions somewhere else.
        assert!(
            table_is_as_asked(&[(1, 2048, 1_000_000), (2, 1_002_048, 9_000_000)], disk).is_err()
        );
        assert!(table_is_as_asked(&[], disk).is_err());
        // Three, but not ours.
        let moved = [
            (1, 2048, 2048),
            (2, 4096, 2_097_152),
            (3, 2_101_248, 23_062_528),
        ];
        assert!(table_is_as_asked(&moved, disk)
            .unwrap_err()
            .contains("partition 2"));
        let short = [good[0], good[1], (3, 1_052_672, 10_000_000)];
        assert!(table_is_as_asked(&short, disk)
            .unwrap_err()
            .contains("partition 3"));
        let long = [good[0], good[1], (3, 1_052_672, disk)];
        assert!(table_is_as_asked(&long, disk).is_err());
        let four = [good[0], good[1], good[2], (4, 25_163_776, 2048)];
        assert!(table_is_as_asked(&four, disk).is_err());
        let numbered = [
            (1, 2048, 2048),
            (2, 4096, 1_048_576),
            (5, 1_052_672, 24_111_104),
        ];
        assert!(table_is_as_asked(&numbered, disk).is_err());
    }

    #[test]
    fn the_partition_table_has_the_three_partitions_in_order() {
        let lines: Vec<&str> = PARTITIONS.lines().collect();
        assert_eq!(lines[0], "label: gpt");
        assert!(lines[BIOS_PART as usize].contains("BIOS boot"));
        assert!(lines[EFI_PART as usize].contains("EFI system"));
        assert!(lines[ROOT_PART as usize].contains("Sengi OS"));
        // The last one has no size: it takes the rest.
        assert!(!lines[ROOT_PART as usize].contains("size="));
    }

    #[test]
    fn the_menu_has_a_way_back_to_labwc_and_a_text_mode() {
        let entry = GRUB_TEXT_ENTRY;
        assert!(entry.starts_with("#!/bin/sh\n"));
        // Both entries, labwc first, each with its own word.
        let labwc = entry
            .find("menuentry 'Sengi OS (labwc compositor)'")
            .unwrap();
        let text = entry.find("menuentry 'Sengi OS, text mode").unwrap();
        assert!(labwc < text);
        let labwc_line = entry
            .lines()
            .find(|line| line.contains(LABWC_FLAG))
            .expect("a kernel line with the labwc word");
        assert!(labwc_line.contains("$GRUB_CMDLINE_LINUX_DEFAULT"));
        assert!(!labwc_line.contains(TEXT_MODE_FLAG));
        // Only the text entry stops at multi-user, and only it has the
        // word the text login looks for.
        let text_line = entry
            .lines()
            .find(|line| line.contains("systemd.unit=multi-user.target"))
            .unwrap();
        assert!(text_line.contains(TEXT_MODE_FLAG));
        assert!(!text_line.contains(LABWC_FLAG));
        assert_eq!(entry.matches("menuentry '").count(), 2);
    }

    #[test]
    fn the_installed_default_is_the_first_entry_and_names_no_compositor() {
        assert!(GRUB_DEFAULT.contains("GRUB_DEFAULT=0\n"));
        assert!(!GRUB_DEFAULT.contains("sengi.compositor"));
    }

    #[test]
    fn grub_is_hidden_and_waits_a_second_for_esc() {
        assert!(GRUB_DEFAULT.contains("GRUB_TIMEOUT=1\n"));
        assert!(GRUB_DEFAULT.contains("GRUB_TIMEOUT_STYLE=hidden\n"));
    }
    #[test]
    fn beside_another_system_grub_looks_for_it_and_shows_a_menu_only_if_found() {
        let beside = grub_default_beside();
        assert!(beside.contains("GRUB_DISABLE_OS_PROBER=false\n"));
        assert!(!beside.contains("GRUB_DISABLE_OS_PROBER=true"));
        // Nothing else differs from the default.
        assert_eq!(
            beside.replace("OS_PROBER=false", "OS_PROBER=true"),
            GRUB_DEFAULT
        );
        // Whole-disk installs never look at other disks.
        assert!(GRUB_DEFAULT.contains("GRUB_DISABLE_OS_PROBER=true\n"));
        let shown = grub_with_menu(&beside);
        assert!(shown.contains("GRUB_TIMEOUT_STYLE=menu\n"));
        assert!(shown.contains("GRUB_TIMEOUT=3\n"));
        assert!(shown.contains("GRUB_DISABLE_OS_PROBER=false\n"));
        assert!(shown.contains("GRUB_DEFAULT=0\n"));
    }

    #[test]
    fn the_stick_is_left_out_of_the_search_for_other_systems() {
        let text = with_skip_list(
            &grub_default_beside(),
            &[
                "0000000000000000".to_string(),
                "2024-01-01-00-00-00-00".to_string(),
            ],
        );
        assert!(text
            .ends_with("GRUB_OS_PROBER_SKIP_LIST=\"0000000000000000 2024-01-01-00-00-00-00\"\n"));
        // Still one setting each, and the menu variant keeps it.
        assert_eq!(text.matches("GRUB_DISABLE_OS_PROBER=").count(), 1);
        assert!(grub_with_menu(&text).contains("GRUB_OS_PROBER_SKIP_LIST="));
    }

    #[test]
    fn other_systems_in_the_menu_are_told_from_our_own_entries() {
        let ours = "### BEGIN /etc/grub.d/10_linux ###\nmenuentry 'Sengi OS' {\n}\n### END /etc/grub.d/10_linux ###\n### BEGIN /etc/grub.d/30_os-prober ###\n### END /etc/grub.d/30_os-prober ###\n";
        assert!(!menu_has_other_systems(ours));
        let windows = "### BEGIN /etc/grub.d/10_linux ###\nmenuentry 'Sengi OS' {\n}\n### END /etc/grub.d/10_linux ###\n### BEGIN /etc/grub.d/30_os-prober ###\nmenuentry 'Windows Boot Manager (on /dev/sda1)' --class windows {\n\tchainloader /EFI/Microsoft/Boot/bootmgfw.efi\n}\n### END /etc/grub.d/30_os-prober ###\n";
        assert!(menu_has_other_systems(windows));
        assert!(!menu_has_other_systems(""));
        // An entry outside that script is not another system.
        assert!(!menu_has_other_systems("menuentry 'x' {\n}\n"));
    }

    #[test]
    fn only_our_own_entry_on_this_partition_may_be_replaced() {
        let listing = "BootOrder: 0006,0005,0000\nBoot0000* Windows Boot Manager\tHD(1,GPT,62516844-55d2-4917-a468-c8a0fbb8b6cd,0x800,0x32000)/File(\\EFI\\Microsoft\\Boot\\bootmgfw.efi)\nBoot0005* Sengi OS\tHD(1,GPT,62516844-55D2-4917-A468-C8A0FBB8B6CD,0x800,0x32000)/File(\\EFI\\sengios\\grubx64.efi)\nBoot0006* Sengi OS\tHD(1,GPT,11111111-2222-4333-8444-555555555555,0x800,0x32000)/File(\\EFI\\sengios\\grubx64.efi)\nBoot0007* Sengi OS\tHD(1,GPT,62516844-55d2-4917-a468-c8a0fbb8b6cd,0x800,0x32000)/File(\\EFI\\other\\grubx64.efi)\n";
        let ours = boot_entries_at(
            listing,
            "Sengi OS",
            "62516844-55d2-4917-a468-c8a0fbb8b6cd",
            "\\EFI\\sengios\\grubx64.efi",
        );
        // Not Windows, not the one on another disk, not one that starts
        // something else.
        assert_eq!(ours, ["0005"]);
        assert!(boot_entries_at(listing, "Sengi OS", "", "\\EFI\\sengios\\grubx64.efi").is_empty());
    }

    #[test]
    fn the_firmwares_order_is_read() {
        let listing = "BootCurrent: 0001\nTimeout: 0 seconds\nBootOrder: 0003,0001,0000\nBoot0000* Windows Boot Manager\n";
        assert_eq!(boot_order(listing), ["0003", "0001", "0000"]);
        assert!(boot_order("BootOrder: \n").is_empty());
        assert!(boot_order("").is_empty());
    }
}
