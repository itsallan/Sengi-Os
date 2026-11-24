# Sengi OS

A light, fast desktop operating system on a minimal Debian base, with its own
interface. Named after the sengi, a tiny and very fast East African mammal.

Sengi OS is in early development. There is no stable release yet.

## Goals

- Under 400 MB of RAM at idle.
- Under 10 seconds from power-on to desktop.
- A familiar layout: top bar, centred dock, window buttons on the left,
  centred search launcher, quick settings and notifications at the top right.
- Its own look, driven by one design tokens file.

Both budgets are measured on every release. A release that misses them does
not ship.

## What is inside

- Debian stable, systemd, Wayland, PipeWire, iwd, BlueZ, greetd.
- zram, systemd-oomd and cgroups v2 so the desktop stays responsive under load.
- Flatpak and apt for apps.
- Our own programs are written in Rust with Slint for the UI.

The shell is our own: sengi-bar (top bar with a system tray), sengi-dock,
sengi-launcher, sengi-notify (notifications) and sengi-quick (quick
settings), all drawn through a shared library, sengi-layer. sengi-focus gives
the focused app priority. sengi-settings is the settings application: network,
Bluetooth, sound, display, power, date and time, keyboard, mouse and touchpad,
notifications, and a page about the machine. It is an ordinary window that
runs only while it is open. sengi-lock is the lock screen: the time, the
date and a password field on every output, started when the screen locks
and gone when it unlocks. It is unlocked with the user's password.
sengi-greeter is the login screen, shown whenever nobody is logged in. It
looks like the lock screen: the time, the date, who is logging in and a
password field. With several accounts the arrow keys or the buttons beside
the name step through them, and Restart and Shut down are in its corner.
greetd checks the password and starts the session.

sengi-installer puts Sengi OS on a disk from the live session; see
[Installing](#installing). The writing is done by sengi-install, a program
without a window that the installer starts as root.

The compositor is sengi-compositor, our own, from 0.8 on. labwc, configured to
the Sengi OS look, stays in the image as the fallback if it fails and as the
way back: a boot menu entry, or the word `sengi.compositor=labwc` on the
kernel command line. See [ROADMAP.md](ROADMAP.md) and
[its README](crates/sengi-compositor/README.md).

## Building the ISO

You need podman. The build runs in a container and uses a pinned Debian
snapshot, so the same commit gives the same ISO.

    scripts/build-iso.sh

The ISO lands in `out/`. If podman cannot run on your machine, the same build
works inside a throwaway QEMU virtual machine:

    scripts/vm.sh up
    scripts/vm.sh run scripts/build-iso.sh
    scripts/vm.sh fetch
    scripts/vm.sh down

## Trying it

Boot the ISO in QEMU:

    qemu-system-x86_64 -enable-kvm -cpu host -smp 2 -m 4G \
      -device virtio-vga -display gtk \
      -drive file=out/sengi-os-0.8.0-amd64.iso,format=raw,if=virtio,readonly=on

Or flash it to a USB stick and boot from that. The live session logs in on its
own; the user is `live` and the password is `live`. To see the login screen,
choose Log out from the session menu, at the power icon in the bar.

| Keys | Action |
|---|---|
| Super+Space | Search and launch |
| Super+Return | Terminal |
| Super+L | Lock |
| Super+Q | Close window |

## Installing

Start the computer from the USB stick and open **Install Sengi OS**, the
first icon in the dock (or search for "install"). It asks, in this order:

1. The keyboard layout and the time zone. The layout takes effect at once.
2. The disk.
3. Your name, a user name, a password and a name for the computer.
4. To look over all of it. Nothing on the computer has been changed until
   you press "Erase disk and install" (or "Install Sengi OS", beside
   another system) there and answer the question that follows.

Then it installs, which takes a few minutes, and offers to restart. Take the
stick out when the screen goes dark, or the computer may start from the stick
again. Log in with the password you chose.

**Erase the whole disk, or install beside.** On the disk page you say which.
"Erase the whole disk" is described next. "Install beside the existing
system" is offered when the computer started with UEFI and the disk has a
GPT, an EFI system partition and at least 10 GB of free (unallocated) space:
it adds one partition in that free space, adds a Sengi OS folder
(`EFI/sengios`) to the EFI system partition, and puts a Sengi OS entry first in
the firmware's boot list, in front of the other system's. Your other files
are not touched: nothing is shrunk, moved, formatted or overwritten, and the
other system's own files on the EFI system partition, and its entry in the
firmware's list, stay as they are. (The partition table itself is
rewritten to add the partition, and the firmware's list changes, which is why
BitLocker or a TPM may ask the other system for a recovery key once: have it
at hand.) It is not offered while Secure Boot is on, because the boot loader
is not signed: turn Secure Boot off in the firmware settings first. Sengi OS's boot menu then lists Windows or
whatever else GRUB's os-prober finds, and is shown for three seconds when
there is something to choose from; with nothing else on the disk it stays
hidden, as after a whole-disk install. When the option is not offered the
row says why; to make room, shrink the other system first, from inside it.
Back up what you want to keep anyway: this part is new, and it has been
tried on virtual disks only. If an installation fails after the partition
was made, the installer says what exists and how to remove the partition
(`sudo sfdisk --delete DISK NUMBER` from the live session, after looking at
`lsblk`); it does not delete partitions itself.

**What it erases (whole-disk install).** The whole disk you choose: every partition and
everything on them, with no way back. The installer shows what is on each
disk before you choose, and again before it starts. It does not offer the
stick it is running from, a disk that is in use, or one smaller than 8 GB.
Back up anything you want to keep first, and if the computer has more than
one disk, read the list with care.

**What you get.** A GPT disk with an EFI system partition and one ext4
partition, no swap partition (zram does that), GRUB for both UEFI and BIOS,
one account that uses `sudo` with its password, and a locked root account.
Beside another system you get one ext4 partition in the free space, GRUB for
UEFI in `EFI/sengios` of the EFI system partition that was there, and the same
account.

**What is not offered yet.** Encrypting the disk; shrinking or moving
another system to make room (do that from inside it first); installing beside
a system on a BIOS/MBR computer or a disk without an EFI system partition;
choosing or laying out partitions by hand.
Secure Boot has to be off.

**Firmware.** Wi-Fi and Bluetooth chips usually need firmware that is not free
software. The image includes Debian's set for the common ones (Intel,
Realtek, MediaTek, Atheros, Broadcom); see `THIRD-PARTY-NOTICES.md`.

**Updates do not install themselves.** An installed Sengi OS does not look
for updates in the background: the timers Debian uses for that are switched
off. Until Sengi OS has its own way of offering updates, they come when you
ask for them, in a terminal: `sudo apt update && sudo apt upgrade`.

**Installing apps.** The Store, the last icon on the dock, finds apps on
Flathub by name and installs them for your user, with no password. From a terminal, `sudo apt install <package>` for Debian's
packages, or `sudo flatpak install flathub <app>` for Flatpak apps. Flathub is
already set up as the Flatpak source (`/etc/flatpak/remotes.d/`); the first
`sudo flatpak` command creates the Flatpak store. When Sengi OS is run from the USB stick without installing, every
change is kept in RAM, so a large Flatpak runtime may not fit and the install
stops with a "min-free-space-size" error. Install Sengi OS to the disk first.

To try the installer without a spare machine, give QEMU a blank disk beside
the ISO:

    qemu-img create -f qcow2 /tmp/sengi-disk.qcow2 16G
    qemu-system-x86_64 -enable-kvm -cpu host -smp 2 -m 4G \
      -device virtio-vga -display gtk \
      -drive file=out/sengi-os-0.8.0-amd64.iso,format=raw,if=virtio,readonly=on \
      -drive file=/tmp/sengi-disk.qcow2,format=qcow2,if=virtio

## Testing

    scripts/boot-test.sh out/sengi-os-0.8.0-amd64.iso test-output

This boots the ISO headless, saves screenshots, and records idle RAM and boot
time against the budgets. It also opens Settings, locks and unlocks the
screen, and logs out and back in through the login screen.

    scripts/install-test.sh out/sengi-os-0.8.0-amd64.iso test-output/install

This installs onto a blank disk image through the installer's window, then
starts that disk alone, logs in with the password it chose, and checks that
it is an installed system: no `live` user, `sudo` asks for a password, the
root file system is the disk. It records how long installing took and the
installed system's boot time and idle RAM. The only disk it writes is the
image file it makes in the output directory. Set `SENGI_UEFI` to an OVMF
firmware image to run either test through UEFI. Set
`SENGI_COMPOSITOR=labwc` to run either test on labwc instead of
sengi-compositor, the default, the installed system included.

## Repository layout

| Path | Contents |
|---|---|
| `tokens/` | Design tokens: colours, spacing, radius, type, motion |
| `ui/` | Shared Slint component library |
| `crates/` | Rust workspace |
| `image/` | ISO build and the root filesystem overlay |
| `scripts/` | Build, test and release scripts |

## Licence

Sengi OS's own source code is licensed under the GNU General Public License,
version 3 or (at your option) any later version. The full text is in
`LICENSE`. The Slint toolkit the programs link against is used under its
GPL-3.0 option, so the programs as built are distributed under GPL-3.0.
Work by others that is copied into this repository, and its licence, is
listed in `THIRD-PARTY-NOTICES.md`. The Rust crates the programs are built
from are listed with their licences in `THIRD-PARTY-CRATES.txt`, which is in
the image under `/usr/share/doc/sengi-os/`; `scripts/gen-notices.sh`
generates it and has to be run again when a `Cargo.lock` changes. Packages
from Debian in the image keep their own licences, which are in
`/usr/share/doc/<package>/copyright`.
