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
- Performance tuning: zram, systemd-oomd, cgroup weights for the compositor
  session, trimmed services and boot parameters.
