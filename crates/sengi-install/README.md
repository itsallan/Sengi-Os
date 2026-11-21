# sengi-install

The part of the Sengi OS installer that writes to a disk. It has no window
and asks nothing: it is given a plan, does what the plan says, and reports
each step as a line on standard output. The installer's window,
[sengi-installer](../sengi-installer/README.md), starts it as root.

**It erases the disk the plan names.** It is meant to run in the live
session.

    sengi-install run [--plan FILE]     install, as root
    sengi-install disks                 list the disks and why some are refused

The first line of standard input is the password of the account to create.
The plan follows it, or is read from FILE. The password is never taken from
an argument or the environment and never written to the log.

## The plan

    [plan]
    version = 1

    [disk]
    device = "/dev/sda"
    mode = "erase"
    encryption = "none"
    size = 256060514304
    model = "SanDisk SD8SN8U"

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

`size` and `model` are what the person was shown when they chose the disk.
If `device` is no longer that disk, nothing is touched.

## What is refused

A disk is not installed on if it is the one the live system was started
from, if it or a partition of it is mounted, is swap in use, or has another
volume built on it, if it is read-only, or if it is under 8 GB. Loop
devices, optical drives, zram and RAM disks are never listed.

## The steps

| Step | What it does |
|---|---|
| `check` | Reads the disks again and compares the plan with them; looks for every program the later steps run and for the system to copy. Changes nothing. |
| `partition` | `sfdisk`: a GPT table with a 1 MiB BIOS boot partition, a 512 MiB EFI system partition, and the rest for Sengi OS. |
| `format` | FAT32 on the EFI system partition, ext4 on the rest. No swap partition: zram is the swap. |
| `copy` | Mounts the new file system and copies the image as it was built (`/run/sengi/rootfs`), not the running session; then the kernel from the stick. |
| `configure` | `fstab` by UUID, host name, a machine identifier of its own, time zone, keyboard layout for the login screen and for new accounts, greetd without the automatic login, the installer and the live image's start-up scripts removed, GRUB's settings. |
| `account` | Removes the `live` user and its sudo rule, creates the account with the groups `live` was in, sets its password through `chpasswd`, locks root. Then reads back that all of that is so. |
| `boot-image` | An initramfs that mounts a disk, in place of the live one. |
| `boot-loader` | GRUB for BIOS in the disk's first sectors, and for UEFI twice: as `EFI/sengios/grubx64.efi` and at the fallback path `EFI/BOOT/BOOTX64.EFI`. `update-grub` writes the menu, hidden and with no wait. On a machine started by UEFI, `efibootmgr` adds a "Sengi OS" entry and removes older ones of that name. |
| `finish` | Enables the weekly `fstrim`, copies the log to the disk, syncs, unmounts. |

Every disk gets both boot loaders, so it starts on either kind of firmware.
The firmware's own entry is the one thing that is allowed to fail: if the
firmware refuses it, that is a warning, and the disk starts from the
fallback path.

## When something fails

The first step that fails ends the installation. Every command's exit
status is checked. Whatever was mounted under `/run/sengi-install/target`
is unmounted again, first plainly and then lazily, so the live session is
left as it was. A signal (TERM, INT, HUP) ends the running command and
fails the step the same way. A second installation cannot start while one
runs.

If the window that started it goes away, the installation carries on: half
a system on a disk is worse than a whole one nobody watched.

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
