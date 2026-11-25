# sengi-install

The part of the Sengi OS installer that writes to a disk. It has no window
and asks nothing: it is given a plan, does what the plan says, and reports
each step as a line on standard output. The installer's window,
[sengi-installer](../sengi-installer/README.md), starts it as root.

**It erases the disk the plan names**, or, when the plan says `beside`, adds one
partition in free space of it, adds a Sengi OS folder to its EFI system partition and a
boot entry to the firmware, and changes nothing else. It is meant to run in the live session.

    sengi-install start                 install in a service of its own, as root
    sengi-install run [--plan FILE]     install here and now, as root
    sengi-install disks                 list the disks and why some are refused

For `start` and `run` the first line of standard input is the password of
the account to create and the plan follows it; `run` can read the plan from
FILE instead. The password is never taken from an argument or the
environment and never written to the log.

## How the window starts it

`start` checks that the plan can be read, then runs

    systemd-run --unit=sengi-install.service --collect --no-block \
        --slice=system.slice --property=ManagedOOMPreference=omit \
        -- systemd-inhibit --what=sleep:idle:shutdown --mode=block \
        /usr/bin/sengi-install run --request /run/sengi-install/request

and returns. The installation is then a transient system service: no child
of `start`, of sudo or of the window, and no part of anyone's session. It
goes on when the window, the terminal or the session that asked for it has
gone. `systemd-inhibit` holds off sleep, the idle action and shutdown until
it is over. `systemctl stop sengi-install.service` stops it, and it
unmounts as it goes.

The password and the plan reach the service through one file,
`/run/sengi-install/request`. A pipe would not do: nothing that holds one
outlives `start`. A unit property or the command line would be readable by
every user through `systemctl show`. The file is in `/run`, which is memory
and not a disk; it is created for root alone (mode 0600) and the service
takes it out of the directory as it opens it, before reading it. It exists
for as long as the service takes to start.

What the installation reports is written to `/run/sengi-install/progress`
as well as to standard output (the journal, for the service). That file is
what the window follows, and what lets a window opened later pick up an
installation that is already running.

## The plan

    [plan]
    version = 1

    [disk]
    device = "/dev/sda"
    mode = "erase"
    encryption = "none"
    size = 256060514304
    model = "SanDisk SD8SN8U"
    serial = "162345400999"
    wwn = "0x5001b448b1234567"

    [user]
    name = "allan"
    full-name = "Allan"

    [system]
    host-name = "zenbook"
    time-zone = "Europe/Paris"
    keyboard = "fr(azerty)"

`mode` is `erase` or `beside`; `encryption` has one value in this version. A plan with a
key or a value this version does not know is refused whole.

For `mode = "beside"` the plan has one more section, written by the window from what it
showed the person:

    [beside]
    start = 42182656      # first sector of the free space, of 512 bytes, on a mebibyte
    sectors = 25165824    # its length
    esp = 1               # the number of the EFI system partition already on the disk
    table = "gpt f6691c2b-...;1:2048:204800:c12a7328-...:62516844-...;2:..."

`table` says exactly which partition table that was: its identifier, and each partition's
number, start, length, type and own identifier. `erase` with a `[beside]` section, and
`beside` without one, are refused.

`size`, `model`, `serial` and `wwn` are what the disk was when the person
chose it; the last two are empty for a disk that has none (a virtual disk,
some card readers). If `device` is no longer a disk with all four the same,
nothing is touched. Two disks of one model and size differ in their serial
numbers.

## Installing beside another system

Only for a disk that a whole-disk install would be offered too, and only when this
computer was started by UEFI. The window lists a disk's free stretches of 10 GiB or more
from what the kernel and udev say (`/sys`, `lsblk`: no root needed); this program reads the
table again as root with `sfdisk -d`. Both come to the same one-line snapshot of the table.
The biggest stretch is used, all of it.

What is held to, in code (`beside.rs`), with the test that proves each:

| Rule | Test |
|---|---|
| The table must still be the one that was shown (identifier, every partition's number, start, length, type, identifier), read as root just before writing. A partition added, moved or retyped since stops the installation before anything is written. | `a_table_that_changed_since_it_was_shown_stops_everything`, `a_table_that_changed_since_it_was_shown_is_not_written_to` |
| The region must be on whole mebibytes, inside the table's usable sectors, at least 10 GiB, and overlap no partition. | `a_region_that_touches_a_partition_is_refused`, `free_space_that_is_not_free_is_not_written_to` |
| The EFI system partition named must still be one, and a FAT volume with 10 MiB free (looked at by mounting it read-only). | `beside_another_system_needs_uefi_and_a_fat_system_partition` |
| sfdisk only runs with `-d` or `--append`, with `--wipe never`, and is given one line. Nothing is deleted, resized or moved. | `nothing_that_writes_may_name_anything_but_the_new_partition`, `sfdisk_is_given_one_line_for_the_new_partition` |
| After writing, the table is read again: every old partition (number, start, length, type, identifier) and the table's identifier must be as they were, and exactly one new one, the one asked for, must be there. Else the installation stops before any format. | `what_was_written_is_checked_against_what_there_was`, `an_old_partition_that_changed_stops_everything_before_a_format` |
| Only the new partition is added to the kernel (`partx --add --nr N`, never a re-read of the whole table), and what the kernel then shows must be the old partitions and the new one. Else stop before any format. | `the_kernels_view_has_the_old_partitions_and_the_new_one`, `a_kernel_that_does_not_show_the_new_partition_stops_everything_before_a_format` |
| Every command is checked before it runs: `mkfs.ext4` may name only the new partition; no other formatter, `wipefs`, `dd`, `sgdisk`, `parted`, `fdisk` at all; nothing but looking at or mounting the old partitions (the EFI system partition may be mounted); no `grub-install` for BIOS, none with `--removable`, none naming a disk. | `nothing_that_writes_may_name_anything_but_the_new_partition`, `beside_another_system_one_partition_is_made_and_nothing_else_is_written` |
| The disk is held (`O_EXCL`) while the table is written, as in a whole-disk install, so a disk or partition that is mounted, swap or part of another volume is refused. | the whole-disk tests of the hold |
| Before the format: the disk is the one that was chosen (serial, WWN, size, model), the table is the one that was written, and the node holds the partition that was made, by the identifier chosen for it (`blkid PARTUUID`); the EFI system partition's identifier is the one it had. Else nothing is formatted. | `a_partition_that_is_not_the_one_made_is_not_formatted`, `a_disk_that_is_not_the_one_chosen_any_more_is_not_formatted`, `a_table_that_changed_after_it_was_written_is_not_formatted_on` |
| Sector 0 is a plain protective MBR, and is the same after the write; names and attributes of the old partitions are the same after the write; `sfdisk -d` may say nothing besides the table. | `only_a_plain_protective_mbr_is_written_beside`, `a_hybrid_mbr_or_a_changed_sector_zero_stops_everything`, `names_and_attributes_are_read_and_a_change_in_them_is_seen`, `a_warning_from_sfdisk_about_the_table_stops_everything` |
| Secure Boot on stops it; a whole-disk install only warns. | `secure_boot_stops_beside_and_only_warns_for_a_whole_disk` |
| `sfdisk` runs in exactly two forms, `sfdisk -d DISK` and the one append argument list the installer uses (no `--wipe=always`, no `-w`, nothing added); `efibootmgr --create` must name this disk, the EFI system partition's number, the label Sengi OS and our loader; `--bootorder` must be a permutation of the entries the firmware has; `partx --nr` must be the new partition's number exactly. The new partition is formatted and mounted by `/dev/disk/by-partuuid/<the identifier chosen for it>`, which `blkid` has just confirmed. Secure Boot is taken as on unless the variable is readable and off (or absent among others). | `nothing_that_writes_may_name_anything_but_the_new_partition`, `the_boot_entry_that_is_made_is_ours_on_the_efi_system_partition`, `the_system_partition_is_formatted_and_mounted_by_the_identifier_that_was_checked`, `secure_boot_fails_closed` |
| `blockdev` is not run at all; `partx` only as `--add --nr N DISK`; `efibootmgr` only to list, to make our entry, to put it first, or to delete an entry that starts our loader from this EFI system partition. | `nothing_that_writes_may_name_anything_but_the_new_partition`, `only_the_entry_of_ours_on_this_partition_is_replaced` |
| A failure after the boot loader went on takes the firmware entry and the folder it made out again. | `a_failure_after_the_boot_loader_went_on_takes_it_out_again` |

`scripts/install-test.sh` with `SENGI_INSTALL_MODE=beside` proves the same on a real
installation, read from outside the guest: see the top of that script.

What is done beside another system: one partition (Linux type, name "Sengi OS") and
`mkfs.ext4` on it; the new system is copied and set up as in a whole-disk install, with the
existing EFI system partition mounted at `/boot/efi`; GRUB for UEFI only, as
`EFI/sengios/grubx64.efi` (no BIOS boot sector, no fallback file at `EFI/BOOT/BOOTX64.EFI`:
those are the other system's); `os-prober` is switched on for this mode only
(`GRUB_DISABLE_OS_PROBER=false`), so Windows and others are in the menu; when it finds
one, the menu is shown for three seconds, and else stays hidden; and a firmware entry
"Sengi OS" is made and put first in the boot order, the others after it in their order.
Where whole-disk installs let the firmware refuse an entry (the fallback path starts the
disk), here it is a failure, since there is no fallback. There is no swap partition.

## What is refused

A disk is not installed on if it is the one the live system was started
from, if it or a partition of it is mounted, is swap in use, or has another
volume built on it, if it is read-only, or if it is under 8 GB. Loop
devices, optical drives, zram and RAM disks are never listed.

A mount is looked for by device number and by the device named as its
source, since a btrfs volume mounts under a number that is no device's.

Reading tables can miss something, so the check ends by opening the whole
disk exclusively (`O_EXCL`). The kernel refuses that while anything on the
disk is mounted, is swap or is part of another volume, and refuses any of
those to begin while the disk is held. It is held until sfdisk has written
the new table.

## The steps

| Step | What it does |
|---|---|
| `check` | Reads the disks again and compares the plan with them; looks for every program the later steps run and for the system to copy. Changes nothing. |
| `partition` | `sfdisk`, with the disk still held: a GPT table with a 1 MiB BIOS boot partition, a 512 MiB EFI system partition, and the rest for Sengi OS. Then the hold is let go, the kernel is told to read the table (`blockdev --rereadpt`, which fails if anything took the disk meanwhile), and what the kernel shows in sysfs is compared with what was asked, start and length of each partition. A kernel still on the old table stops the installation here, before any mkfs. |
| `format` | FAT32 on the EFI system partition, ext4 on the rest. No swap partition: zram is the swap. |
| `copy` | Mounts the new file system and copies the image as it was built (`/run/sengi/rootfs`), not the running session; then the kernel from the stick. |
| `configure` | `fstab` by UUID, host name, a machine identifier of its own, time zone, keyboard layout for the login screen and for new accounts, greetd without the automatic login, the installer, the live image's start-up scripts and the tests' reporters removed, GRUB's settings, the menu's text mode entry and the text login it switches on. |
| `account` | Removes the `live` user and its sudo rule, creates the account with the groups `live` was in, sets its password through `chpasswd`, locks root. Then reads back that all of that is so. |
| `boot-image` | An initramfs that mounts a disk, in place of the live one. |
| `boot-loader` | GRUB for BIOS in the disk's first sectors, and for UEFI twice: as `EFI/sengios/grubx64.efi` and at the fallback path `EFI/BOOT/BOOTX64.EFI`. `update-grub` writes the menu, hidden, with a wait of one second. On a machine started by UEFI, `efibootmgr` adds a "Sengi OS" entry and removes older ones of that name. |
| `finish` | Enables the weekly `fstrim`, copies the log to the disk, syncs, unmounts. |

Every disk gets both boot loaders, so it starts on either kind of firmware.
The firmware's own entry is the one thing that is allowed to fail: if the
firmware refuses it, that is a warning, and the disk starts from the
fallback path.

## The boot menu, and the way in when the desktop does not start

The installed system's menu is hidden and waits one second. Press Esc in
that second, or keep pressing it from power-on, and the menu is shown.
With no wait at all, as on the live image, GRUB looks once for a key that
is already there: tried in QEMU, Esc then showed the menu under BIOS but
not under UEFI, and Shift under neither. With the second it shows under
both. The install test presses Esc on every run and fails if the menu
does not hold the system.

The menu has an entry "Sengi OS, text mode (if the desktop does not
start)". It starts the same kernel with `sengi.text
systemd.unit=multi-user.target`: no login screen, no desktop, and a text
login on the first console. Log in there with your own name and password.
From there you have `sudo`, so you can read the journal (`journalctl -b
-1` for the start that failed), undo a change to a configuration file,
remove or reinstall a package with apt, or try the desktop with `sudo
systemctl isolate graphical.target`. The entry is written by
`/etc/grub.d/11_sengi-text`, for the newest kernel, whenever `update-grub`
runs. The text login is `getty@tty1`, which the image masks; on an
installed system it has a condition instead, and runs only when the
kernel was started by that entry.

There is no recovery entry of the usual kind. It asks for root's
password, and root is locked.

## Names, and what the tools take

Nothing typed in the window may fail an installation after the disk is
erased, so what the window lets through was put to the image's own
`useradd` and `chpasswd`, run as this program runs them (empty
environment, `LC_ALL=C`):

- Full names: `Zoë Nakato`, `Олена Шевченко`, `田中 花子`, `محمد علي`,
  `O'Neil "Bud" Jr.`, a name with an emoji, 64 ASCII characters, 64
  accented characters, shell punctuation, and the empty name. All taken,
  and stored as typed. (A colon, a comma, an equals sign and control
  characters are refused in the window: the first two would break the
  passwd file's fields.)
- User names: one character, 32 characters, with hyphen, underscore and
  digits. All taken. The window's rule is narrower than useradd's.
- Passwords: one character, 511 bytes, with colons and spaces, in several
  scripts, all digits. All taken; root sets them, so no strength rule
  applies.
- The computer's name is written to two files and passes through no tool.

An installation with the full name `Олена Шевченко` was run to the end on
a loop device; the login screen of that system shows the name. Whether a
name is *drawn* depends on the fonts: Inter has Latin, Cyrillic and Greek.
A name in a script it lacks is stored correctly and may show as boxes.

## The tests' reporters

The image carries two scripts that report to the QEMU tests over the
serial port, each started only when the machine has that test's
credential. `sengi-boot-report` also runs commands the host hands it, as
root. Neither belongs on an installed system. This program removes
`sengi-boot-report` and its unit always, and `sengi-install-report`, its
unit and the file they share unless the machine it is running on was
started with the install test's credential (`sengi.test.install`), which
only whoever controls the machine's firmware tables can set. Then that
one is left, so the test can ask the installed system what it is. It only
looks and reports: it takes no commands.

## When something fails

The first step that fails ends the installation. Every command's exit
status is checked. Whatever was mounted under `/run/sengi-install/target`
is unmounted again, first plainly and then lazily, so the live session is
left as it was. A signal (TERM, INT, HUP) ends the running command and
fails the step the same way. A second installation cannot start while one
runs.

If the window that started it goes away, or the session it was started
from ends, the installation carries on: half a system on a disk is worse
than a whole one nobody watched. What does stop it is being told to: a
signal, or `systemctl stop`. Tried on a loop device: SIGTERM during the
copy, and `systemctl stop` during the copy, both leave nothing mounted,
and the next installation runs to the end.

The record is `/var/log/sengi-install.log`: every command, what it printed
and how it ended. On success it is also on the installed disk at the same
path.

## What it reports

    log /var/log/sengi-install.log
    begin copy
    percent copy 42
    end copy
    warning The firmware did not take a boot entry ...
    done 73
    failed partition sfdisk ended with status 1: ...

## Testing

`cargo test -p sengi-install` runs the steps against a made-up machine that
records every command and runs none, with a temporary directory as the new
system. No test touches a block device.

`scripts/install-test.sh` runs the real thing in QEMU, onto a disk image.

A plan may carry a `[source]` section naming another tree to copy and
allowing a loop device as the target. That is for trying the program
against a file; the window never writes it.

## Known limits beside another system

- Only tried on virtual disks, with stand-ins for the other system's files. Not tried
  on a real Windows disk, on real firmware, on a 4Kn disk, or on a BIOS machine (where it
  is not offered).
- Not offered with Secure Boot on (the GRUB put on the EFI system partition is not
  signed), with a hybrid MBR, with more than one EFI system partition on the disk, or
  where sfdisk says anything about the table besides the table (a backup copy not at the
  end of a disk that was made larger, for one).
- A failure after the new partition was made leaves it there. The installer does not
  delete partitions: it says what exists, and how to remove it by hand
  (`sudo sfdisk --delete DISK NUMBER`). What the boot loader step put on the EFI system
  partition (the `EFI/sengios` folder, if it was not there before) and in the firmware (the
  entry it made) it does take out again, and says so.
- Before anything is formatted, the disk is identified again (serial, WWN, size, model),
  the table is read again and compared with what was written, and the new partition must
  have the identifier that was chosen for it before it was written (`blkid PARTUUID`), so a
  name that moved to another disk cannot be formatted. The disk cannot be held
  (`O_EXCL`) through the format: the kernel refuses to open a partition of a disk that is
  held.
- Sector 0 is read before and after, and must be a plain protective MBR; partition names
  and attributes (Windows marks its recovery partition with them) are compared before and
  after the write.
- os-prober runs once, at the install, and again at every kernel update (update-grub).
  It mounts other systems' partitions with GRUB's own read-only driver (`grub-mount`),
  which, as far as its code says, replays no journal and writes nothing. That is read from the code
  and **not proven**: in particular nobody has shown that a hibernated NTFS volume (Windows fast
  startup or hibernation) is left exactly as it was, and a person who cares about one should
  disable fast startup or shut Windows down fully first; the install test puts an ext4 volume that
  needs recovery and an NTFS-like volume with a hibernation signature beside the new
  partition and checks them byte for byte afterwards. Not tried with a real
  hibernated Windows, so for that case the claim stays unproven.
- Debian's grub-efi and grub-pc packages, whose upgrades run grub-install themselves, are
  not in the image (the install test checks that none is installed), so an upgrade cannot
  write to EFI/BOOT or to another system's folder; only update-grub runs, which rewrites
  `grub.cfg` on the Sengi OS partition.
- The firmware entries of the other system are not touched. Only an entry named "Sengi OS"
  that starts `\EFI\sengios\grubx64.efi` from this disk's own EFI system partition is ever
  replaced; one from a Sengi OS on another disk is left alone.

## Not done here

- Encryption, shrinking another system to make room, partitions by hand.
- Reinstalling the boot loader when a GRUB package is updated. The `grub-pc`
  and `grub-efi-amd64` packages, which would do that, are not in the image;
  `update-grub` still runs when a kernel is installed.
- Secure Boot.
