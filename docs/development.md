# Development

## What is inside

- Debian stable, systemd, Wayland, PipeWire, iwd, BlueZ, greetd.
- zram, systemd-oomd and cgroups v2 so the desktop stays responsive under load.
- Flatpak and apt for apps.
- Our own programs are written in Rust with Slint for the UI.

The shell is our own: sengi-bar (top bar with a system tray), sengi-dock,
sengi-desktop (the folders and files of ~/Desktop as icons, with a right-click
menu), sengi-launcher, sengi-notify (notifications) and sengi-quick (quick
settings), all drawn through a shared library, sengi-layer. sengi-focus gives
the focused app priority. sengi-settings is the settings application: network,
Bluetooth, sound, display, power, date and time, keyboard, mouse and touchpad,
notifications, printers, and a page about the machine. It is an ordinary window that
runs only while it is open. sengi-files is the file manager, an ordinary
window too: your folders, USB and external drives, the Trash, copy, move and rename.
sengi-open opens a file in the program for it: sengi-edit (a plain text
editor) for text, sengi-view (an image viewer) for PNG and JPEG, Files for
folders. sengi-calc is the calculator, a window too. sengi-welcome is the first-run welcome, shown once on an installed system. sengi-lock is the lock screen: the time, the
date and a password field on every output, started when the screen locks
and gone when it unlocks. It is unlocked with the user's password.
The laptop keys are handled by the compositor: volume and brightness (also
the keyboard light) show a level for a moment through sengi-osd, and Print saves
a PNG of the screen to ~/Pictures through sengi-shot (Alt+Print: the window).
sengi-greeter is the login screen, shown whenever nobody is logged in. It
looks like the lock screen: the time, the date, who is logging in and a
password field. With several accounts the arrow keys or the buttons beside
the name step through them, and Restart and Shut down are in its corner.
greetd checks the password and starts the session.

sengi-installer puts Sengi OS on a disk from the live session; see
[Installing](guide.md#installing). The writing is done by sengi-install, a program
without a window that the installer starts as root.

The compositor is sengi-compositor, our own, from 0.8 on. labwc, configured to
the Sengi OS look, stays in the image as the fallback if it fails and as the
way back: a boot menu entry, or the word `sengi.compositor=labwc` on the
kernel command line. See [ROADMAP.md](../ROADMAP.md) and
[its README](../crates/sengi-compositor/README.md).

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
      -drive file=out/sengi-os-1.0.0-amd64.iso,format=raw,if=virtio,readonly=on

Or flash it to a USB stick and boot from that. The live session logs in on its
own; the user is `live` and the password is `live`. To see the login screen,
choose Log out from the session menu, at the power icon in the bar.


## Testing

    scripts/boot-test.sh out/sengi-os-1.0.0-amd64.iso test-output

This boots the ISO headless, saves screenshots, and records idle RAM and boot
time against the budgets. It also opens Settings, locks and unlocks the
screen, and logs out and back in through the login screen.

    scripts/install-test.sh out/sengi-os-1.0.0-amd64.iso test-output/install

This installs onto a blank disk image through the installer's window, then
starts that disk alone, logs in with the password it chose, and checks that
it is an installed system: no `live` user, `sudo` asks for a password, the
root file system is the disk. It records how long installing took and the
installed system's boot time and idle RAM. The only disk it writes is the
image file it makes in the output directory. Set `SENGI_UEFI` to an OVMF
firmware image to run either test through UEFI. Set
`SENGI_COMPOSITOR=labwc` to run either test on labwc instead of
sengi-compositor, the default, the installed system included.

## Release notes

A release publishes the ISO, its checksum and the measurements as files. Its
pictures show only what changed or was added, inside the notes: put them in
`docs/releases/VERSION/` and link them from that version's section of
`CHANGELOG.md`, for example `![Store](docs/releases/1.1.0/store.png)`, before
tagging.

## Repository layout

| Path | Contents |
|---|---|
| `tokens/` | Design tokens: colours, spacing, radius, type, motion |
| `ui/` | Shared Slint component library |
| `crates/` | Rust workspace |
| `image/` | ISO build and the root filesystem overlay |
| `scripts/` | Build, test and release scripts |

