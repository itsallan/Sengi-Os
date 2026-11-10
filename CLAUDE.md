# Sengi OS: project rules

Sengi OS is a light, fast desktop OS on a minimal Debian base with our own
interface. One human (Allan) tests releases. The lead engineer agent plans,
writes code, tests, reviews, merges and releases.

## Goal and budget

- Target machine: ASUS ZenBook UX410UQK (i5-7200U, 2 cores, 16 GB RAM,
  GeForce 940MX, SATA SSD).
- Budget: under 400 MB RAM at idle, under 10 seconds from power-on to desktop.
- Measure both on every release. If the budget is exceeded the release fails:
  fix it or cut the feature.

## Naming

- The product is "Sengi OS". Always write it that way in user-facing text.
- Repository: `sengi-os`. System ID in `/etc/os-release`: `sengios`.
- Every binary and package we ship is prefixed `sengi-`: sengi-bar, sengi-dock,
  sengi-launcher, sengi-notify, sengi-settings, sengi-greeter, sengi-compositor.
- Never ship a package or command named plain `sengi`. An unrelated Mastodon
  client already uses that name.

## Languages

- Rust for all of our own programs, in one Cargo workspace (`crates/`).
- Slint (`.slint`) for UI (`ui/`).
- POSIX shell for build and test scripts. No Python or Node in the base image.

## Fixed decisions (do not change without asking)

- Base: minimal Debian stable. systemd with on-demand services only.
- Wayland, PipeWire, iwd, BlueZ, greetd, zram, systemd-oomd.
- cgroups v2 so the focused app and compositor get CPU and I/O priority.
- Flatpak and apt for apps. Wine and Waydroid are optional, never preinstalled.
- No GNOME, no Electron, no Qt in the base image.
- Layout follows macOS: top bar, centred dock, window buttons on the left,
  centred search launcher, quick settings and notifications top right.
- Visual identity is our own. No Apple fonts, icons, wallpapers or logos.
  Typeface is Inter. Corner radius is 6 px everywhere.
- All colours, spacing, radius, type and motion come from
  `tokens/sengi.toml` and the shared component library in `ui/`.
  No one-off styles. Config templates use `@{token}` placeholders and are
  rendered by `scripts/render-tokens.sh`; `scripts/lint-styles.sh` rejects
  literal colours in templates.

## Releases (strictly one at a time)

See ROADMAP.md. Do not start a release until Allan confirms the previous one.
The only exception is sengi-compositor, which may progress on a long-lived
branch from 0.2 onward and is switched in only when it is as stable as the
compositor it replaces.

"Done" means merged and present in a released ISO, nothing less.

## How to work

- Split independent tasks across subagents, each in its own git worktree and
  branch. At most 5 at once. One owner per component.
- No UI work starts before the tokens and component library are merged.
- Every task goes branch -> pull request -> CI -> review -> merge.
- CI must pass: build, clippy, unit tests, and a headless QEMU boot test that
  reaches the desktop, saves a screenshot, and records idle RAM and boot time.
- A reviewer subagent that did not write the code checks each pull request
  against the tokens, the budget and these rules, then merges it or sends it
  back. Look at the screenshots, not only the code.

## Releasing

When every checklist item for a version is merged and CI is green: tag it
(`vX.Y.Z`), let the release workflow build the ISO, and publish a GitHub
release with the ISO, checksums, changelog, screenshots and the measured RAM
and boot time. Tick the items in ROADMAP.md. Then stop and wait for Allan's
test result.

## Git rules

- Commit after each working change, not in batches.
- Commit messages in imperative mood, written like a human engineer: what
  changed and why.
- No Co-Authored-By trailers, no "Generated with" lines, no AI attribution and
  no bot emoji anywhere: commits, pull requests, release notes, comments.
  Attribution is turned off in `.claude/settings.json`.
- Code comments are sparse and read like a person wrote them. No emojis in code.
- Never force-push main. Never delete tags or releases.
- Never commit ISOs, disk images, `target/`, caches or secrets. Keep
  `Cargo.lock` tracked. Add a `.gitignore` line when a new kind of build output
  appears.

## Limits

- Work only inside this repository folder.
- Anything needing root, such as image builds, runs inside a podman container
  or a VM. Never use sudo on the host.
- Never write to disks, partitions, bootloaders or USB devices. Allan flashes
  the USB himself.
- Do not install packages on the host. Say what is missing.
- Stop and ask before: anything that costs money, licence questions, any change
  to the fixed decisions or the release order, and any visual choice you are
  unsure about (attach screenshots).

## Repository layout

- `tokens/sengi.toml`: the single source of design tokens.
- `ui/`: Slint component library (`tokens.slint` is generated from the tokens).
- `crates/`: Rust workspace members.
- `image/`: ISO build (mmdebstrap, squashfs, GRUB) and the root filesystem overlay in `image/rootfs/`. Files ending in `.in` are token templates.
- `scripts/`: POSIX shell build, render, lint and boot-test scripts.
- `.github/workflows/`: CI, release and roadmap sync.

## Common commands

    scripts/render-tokens.sh --check     # generated files are current
    scripts/lint-styles.sh               # no one-off styles
    scripts/build-iso.sh                 # ISO into out/, needs podman
    scripts/boot-test.sh out/*.iso test-output
    cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo test --workspace
