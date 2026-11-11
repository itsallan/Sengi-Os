# Changelog

All notable changes to Sengi OS. Each release section lists what is in its ISO
and the measured idle RAM and boot time.

## Unreleased

## 0.2.0

The top bar is now our own program.

### Added

- sengi-bar, the Sengi OS top bar, written in Rust with Slint and drawn into
  a layer-shell surface. It shows the menu, network (with the Wi-Fi network
  name), volume, battery, clock, notifications and session items.
- One pinned Rust toolchain for local builds, CI and the ISO builder.

### Changed

- Waybar no longer draws the top bar. It stays only for the dock until
  sengi-dock replaces it.

### Known limits

- There is no system tray yet. Apps that close to the tray keep running with
  no icon to bring them back; start them again from search. Planned for 0.4.
- Only one monitor gets a bar.
- Wi-Fi cards that need non-free firmware (most Intel cards) do not work yet.
- Secure Boot has to be turned off to boot the ISO.
- The clock shows UTC.

## 0.1.0

The first bootable Sengi OS: a live ISO with the desktop layout in place,
built from existing components configured from one set of design tokens.

### Added

- Roadmap and design tokens.
- Slint component library generated from the tokens.
- Container-based, reproducible ISO build on Debian trixie (mmdebstrap,
  pinned snapshot mirror, fixed SOURCE_DATE_EPOCH).
- CI: shell lint, style lint, cargo build, clippy, tests, ISO build and a
  headless QEMU boot test with screenshot, idle RAM and boot time.
- Desktop for 0.1: labwc, Waybar top bar, Waybar dock, fuzzel launcher, mako
  notifications, swaylock, greetd, all themed from the tokens.
- Our own initramfs boot script for the live medium, in place of live-boot.
- Performance tuning: zram, systemd-oomd, cgroup weights that favour the
  compositor and shell over apps, on-demand Wi-Fi and Bluetooth daemons,
  trimmed first-boot services and a prebuilt font cache.
- sengi-focus, a small daemon that raises the CPU and I/O weight of the
  focused app and puts it back when focus moves on.
- The boot test opens a terminal, the launcher and the lock screen and saves
  a screenshot of each.

### Known limits

- Wi-Fi cards that need non-free firmware (most Intel cards) do not work yet.
- Secure Boot has to be turned off to boot the ISO.
- The clock shows UTC.
- Two windows of the same app share the focused-app priority boost.
- There is no installer yet; this is a live image only.
