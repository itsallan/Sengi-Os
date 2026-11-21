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

The compositor (labwc) is still an existing component configured to the
Sengi OS look; a later release replaces it with sengi-compositor, which is
being developed on its own branch. See [ROADMAP.md](ROADMAP.md).

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
      -drive file=out/sengi-os-0.7.0-amd64.iso,format=raw,if=virtio,readonly=on

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
   you press "Erase disk and install" there and answer the question that
   follows.

Then it installs, which takes a few minutes, and offers to restart. Take the
stick out when the screen goes dark, or the computer may start from the stick
again. Log in with the password you chose.

**What it erases.** The whole disk you choose: every partition and
everything on them, with no way back. The installer shows what is on each
disk before you choose, and again before it starts. It does not offer the
stick it is running from, a disk that is in use, or one smaller than 8 GB.
Back up anything you want to keep first, and if the computer has more than
one disk, read the list with care.

**What you get.** A GPT disk with an EFI system partition and one ext4
partition, no swap partition (zram does that), GRUB for both UEFI and BIOS,
one account that uses `sudo` with its password, and a locked root account.

**What is not offered yet.** Encrypting the disk; installing beside Windows
or another system (dual boot); choosing or laying out partitions by hand.
Secure Boot has to be off.

To try the installer without a spare machine, give QEMU a blank disk beside
the ISO:

    qemu-img create -f qcow2 /tmp/sengi-disk.qcow2 16G
    qemu-system-x86_64 -enable-kvm -cpu host -smp 2 -m 4G \
      -device virtio-vga -display gtk \
      -drive file=out/sengi-os-0.7.0-amd64.iso,format=raw,if=virtio,readonly=on \
      -drive file=/tmp/sengi-disk.qcow2,format=qcow2,if=virtio

## Testing

    scripts/boot-test.sh out/sengi-os-0.7.0-amd64.iso test-output

This boots the ISO headless, saves screenshots, and records idle RAM and boot
time against the budgets. It also opens Settings, locks and unlocks the
screen, and logs out and back in through the login screen.

    scripts/install-test.sh out/sengi-os-0.7.0-amd64.iso test-output/install

This installs onto a blank disk image through the installer's window, then
starts that disk alone, logs in with the password it chose, and checks that
it is an installed system: no `live` user, `sudo` asks for a password, the
root file system is the disk. It records how long installing took and the
installed system's boot time and idle RAM. The only disk it writes is the
image file it makes in the output directory. Set `SENGI_UEFI` to an OVMF
firmware image to run either test through UEFI.

## Repository layout

| Path | Contents |
|---|---|
| `tokens/` | Design tokens: colours, spacing, radius, type, motion |
| `ui/` | Shared Slint component library |
| `crates/` | Rust workspace |
| `image/` | ISO build and the root filesystem overlay |
| `scripts/` | Build, test and release scripts |
