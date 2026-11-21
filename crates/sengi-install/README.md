# sengi-install

The part of the Sengi OS installer that writes to a disk. It has no window
and asks nothing: it is given a plan, does what the plan says, and reports
each step as a line on standard output. The installer's window,
[sengi-installer](../sengi-installer/README.md), starts it as root.

**It erases the disk the plan names.** It is meant to run in the live
session.

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

`mode` and `encryption` have one value each in this version. They are there
so that a later version that installs beside another system, or encrypts,
adds values and not a new format. A plan with a key or a value this version
does not know is refused whole.

`size`, `model`, `serial` and `wwn` are what the disk was when the person
chose it; the last two are empty for a disk that has none (a virtual disk,
some card readers). If `device` is no longer a disk with all four the same,
nothing is touched. Two disks of one model and size differ in their serial
numbers.

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

## Not done here

- Encryption, installing beside another system, partitions by hand.
- Reinstalling the boot loader when a GRUB package is updated. The `grub-pc`
  and `grub-efi-amd64` packages, which would do that, are not in the image;
  `update-grub` still runs when a kernel is installed.
- Secure Boot.
