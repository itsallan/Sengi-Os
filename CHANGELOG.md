# Changelog

All notable changes to Sengi OS. Each release section lists what is in its ISO
and the measured idle RAM and boot time.

## Unreleased

### Added

- Project rules, roadmap and design tokens.
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
