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
runs only while it is open.

The compositor (labwc) and lock screen (swaylock) are still existing
components configured to the Sengi OS look; later releases replace them with
sengi-greeter and finally sengi-compositor, which is being developed on its
own branch. See [ROADMAP.md](ROADMAP.md).

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
      -drive file=out/sengi-os-0.5.0-amd64.iso,format=raw,if=virtio,readonly=on

Or flash it to a USB stick and boot from that. The live session logs in on its
own; the user is `live` and the password is `live`.

| Keys | Action |
|---|---|
| Super+Space | Search and launch |
| Super+Return | Terminal |
| Super+L | Lock |
| Super+Q | Close window |

## Testing

    scripts/boot-test.sh out/sengi-os-0.5.0-amd64.iso test-output

This boots the ISO headless, saves a screenshot, and records idle RAM and boot
time against the budgets.

## Repository layout

| Path | Contents |
|---|---|
| `tokens/` | Design tokens: colours, spacing, radius, type, motion |
| `ui/` | Shared Slint component library |
| `crates/` | Rust workspace |
| `image/` | ISO build and the root filesystem overlay |
| `scripts/` | Build, test and release scripts |
