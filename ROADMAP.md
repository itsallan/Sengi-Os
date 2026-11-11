# Sengi OS roadmap

Releases ship strictly one at a time. A release starts only after Allan
confirms the previous one. An item is ticked only when it is merged and present
in a released ISO. Every release must measure under 400 MB idle RAM and under
10 s from power-on to desktop.

This file is mirrored to GitHub milestones and issues by
`.github/workflows/roadmap-sync.yml`. Each `## X.Y` heading is a milestone and
each `- [ ]` line is an issue.

## 0.1

- [x] Project rules, roadmap, changelog and design tokens
- [x] Slint component library generated from the tokens
- [x] Reproducible ISO build inside a container
- [x] CI with build, clippy, tests and a headless QEMU boot test
- [x] Minimal Debian stable base with live boot
- [x] labwc compositor themed from tokens with window buttons on the left
- [x] Waybar top bar with quick settings and notifications at top right
- [x] Centred dock
- [x] Centred search launcher (fuzzel)
- [x] Notifications (mako)
- [x] Lock screen (swaylock) and idle lock
- [x] greetd login with live autologin
- [x] PipeWire, iwd, BlueZ on demand
- [x] zram and systemd-oomd
- [x] cgroups v2 priority for compositor and focused app
- [x] Boot time tuning
- [x] Measured idle RAM and boot time within budget

## 0.2

- [x] sengi-bar replaces Waybar
- [x] sengi-compositor branch opened (Rust + Smithay, long-lived)

## 0.3

- [ ] A sengi-bar on every monitor
- [ ] sengi-dock replaces the 0.1 dock
- [ ] sengi-launcher replaces fuzzel

## 0.4

- [ ] sengi-notify replaces mako
- [ ] Quick settings panel
- [ ] System tray in sengi-bar

## 0.5

- [ ] sengi-settings

## 0.6

- [ ] sengi-greeter
- [ ] Own lock screen replaces swaylock

## 0.7

- [ ] Installer

## 0.8

- [ ] sengi-compositor switched in, as stable as the compositor it replaces

## 1.0

- [ ] Polish pass across all components
- [ ] First public release
