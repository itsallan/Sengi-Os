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

/// What GRUB is told. The menu is hidden and waits for nobody, as the
/// live image's is; holding Shift or pressing Esc shows it. The kernel
/// is as quiet as the live image's. There is no recovery entry: it would
/// ask for root's password, and root has none.
pub const GRUB_DEFAULT: &str = "\
# Written by the Sengi OS installer. Run update-grub after changing it.
GRUB_DEFAULT=0
GRUB_TIMEOUT_STYLE=hidden
GRUB_TIMEOUT=0
GRUB_DISTRIBUTOR=\"Sengi OS\"
GRUB_CMDLINE_LINUX_DEFAULT=\"quiet loglevel=3 systemd.show_status=false rd.udev.log_level=3\"
GRUB_CMDLINE_LINUX=\"\"
GRUB_TERMINAL=console
GRUB_DISABLE_OS_PROBER=true
GRUB_DISABLE_RECOVERY=true
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
    fn grub_is_hidden_and_does_not_wait() {
        assert!(GRUB_DEFAULT.contains("GRUB_TIMEOUT=0\n"));
        assert!(GRUB_DEFAULT.contains("GRUB_TIMEOUT_STYLE=hidden\n"));
    }
}
