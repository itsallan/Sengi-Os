# Changelog

All notable changes to Sengi OS. Each release section lists what is in its ISO
and the measured idle RAM and boot time.

## 0.7.0

An installer: Sengi OS can be put on a computer's disk.

### Added

- Install Sengi OS, in the dock and the launcher of the live session. It
  asks for a keyboard layout and a time zone, a disk, a name, a password
  and a name for the computer, shows all of it once more, and installs.
  Each step is shown as it runs, and the copy says how far it is.
- The installer takes one whole disk and erases it. It lists every disk
  with what is on it now, and does not offer the stick Sengi OS is running
  from, a disk with anything mounted or in use, or one under 8 GB.
  Nothing is changed before the last page. The button that erases never
  has the keyboard to begin with, and asks once more with Cancel as the
  answer Return gives.
- The disk gets a GPT partition table with an EFI system partition and
  one ext4 partition for everything else. There is no swap partition:
  zram is the swap, as on the live image. GRUB is installed for UEFI and
  for BIOS firmware both, so the disk starts either way, and for UEFI
  also at the path firmware falls back to when it has no entry for the
  disk. Its menu is hidden and does not wait, like the live image's.
- The installed system starts at the login screen. The account made in
  the installer is the one person on it; it uses sudo with its password.
  The root account is locked. The `live` user, its right to be root
  without a password and the installer itself are not on the installed
  system.
- The keyboard layout chosen in the installer takes effect at once, so
  that the password is typed with the layout it will be typed with later,
  and is the layout of the login screen and of new accounts on the
  installed system.
- sengi-install, the program that does the installing. The window starts
  it; it can also be run by hand with a plan in a file. It leaves a
  record in /var/log/sengi-install.log, on the live session and on the
  installed disk.
- scripts/install-test.sh installs onto a blank disk image in QEMU
  through the installer's window, starts that disk alone, logs in with
  the password that was chosen and checks what it finds. CI runs it after
  the boot test, through BIOS, and through UEFI without failing the build
  on it yet.

### Changed

- The image has what the installer needs: fdisk, dosfstools, e2fsprogs,
  efibootmgr, and GRUB's tools and its files for both kinds of firmware.
  None of it runs at boot or at idle.

### Known limits

- No installing beside another system, no encryption, and no laying out
  of partitions by hand. The installer erases one whole disk.
- Secure Boot has to be off, for the installed system as for the ISO.
- Installed, the boot loader is put in place once. A later GRUB package
  update refreshes the menu when a kernel is installed but does not write
  the boot loader itself again.
- One language, English, and no choice of locale.
- The time zone starts at UTC and has to be chosen from the list; nothing
  guesses it.
- The installer has been run in QEMU only, on virtio disks, with BIOS and
  with UEFI firmware. It has not yet been run on a real machine.

## 0.6.0

A lock screen and a login screen of our own.

### Added

- sengi-greeter, a login screen of our own, in place of greetd's text
  prompt. It looks like the lock screen: the time and the date, who is
  logging in, and a password field, on every screen. With more than one
  account the arrow keys, or the buttons beside the name, step through
  them; with none the name is typed. Restart and Shut down are in the
  bottom right corner. Wrong passwords wait as they do on the lock screen.
  The live image still logs in by itself, so the login screen is first
  seen after Log out.

- sengi-lock, a lock screen of our own: the time and the date, the name of
  the user and a password field, on every screen. It shows when Caps Lock
  is on and which keyboard layout is active when there is more than one.
  After half a minute untouched only the clock is left; any key or a move
  of the pointer brings the field back. Repeated wrong passwords have to
  wait a little longer each time, up to half a minute, and never lock the
  user out.
- The lock screen's clock follows the 12 or 24 hour setting and the time
  zone chosen in Settings, and is right the moment the machine wakes from
  sleep.
- If the lock screen is killed or crashes, the session stays locked and the
  lock screen comes back by itself.

### Changed

- swaylock is no longer in the image. The idle timer, Super+L, the session
  menu and the Lock button in quick settings start sengi-lock.
- The machine does not go to sleep until every screen is covered by the
  lock screen.
- polkitd is in the image. Nothing starts it at boot; logind calls for it
  with its first question, which comes when the session starts, so it is
  running from then on. It takes about 3 MB.

### Fixed

- Restart and Shut down in the session menu did not work in 0.5.0. logind
  asks polkit whether the user may, there was no polkit in the image, and
  without it logind refuses everyone but root. The boot test now asks
  logind from the live session and from the login screen's, and fails
  unless both may restart and shut down.
- The session could not make the machine wait before sleeping. swayidle
  asks logind to hold sleep off until the screen is locked, and for the
  same want of polkit logind refused ("Failed to send sleep inhibit
  signal: Permission denied" in the journal of 0.5.0). It is granted now.

- The image build took the setuid and setgid bits off every program in
  /usr and gave /etc/shadow to the wrong group. sudo, su and passwd did
  not work for the live user, and no password could be checked, so a
  locked screen could not be unlocked by typing it. The 0.5.0 image has
  this fault. The boot test now unlocks the lock screen with the live
  user's password and fails if that does not work.

## 0.5.0

A settings application of our own.

### Added

- sengi-settings, the settings application, in the dock, the launcher and the
  session menu. Its ten pages are network, Bluetooth, sound, display, power,
  date and time, keyboard, mouse and touchpad, notifications, and about. It
  is an ordinary window that runs only while it is open: nothing of it is
  left running, and it costs nothing at idle.
- Settings that Sengi OS keeps itself live in one plain-text file,
  ~/.config/sengi-os/settings.toml, with system-wide defaults in
  /etc/xdg/sengi-os/settings.toml.

### Changed

- When the screen switches off and when it locks follow Settings, as does
  locking on sleep. A change takes effect at once.
- The clock in the bar follows the 12 or 24 hour setting and the time zone
  chosen in Settings, within a minute of the change.

### Known limits

- The time zone chosen in Settings is the user's own. The system-wide zone
  cannot be changed from Settings, and programs that were already running
  keep the old zone until the next login.
- Until a time zone is chosen in Settings the clock shows UTC.
- Starting Settings while it is open shows the page asked for in the open
  window but does not raise that window.
- Switching keyboard layouts and the mouse and touchpad settings have not
  been verified on real hardware.
- The network, Bluetooth, brightness and battery parts of Settings and of
  quick settings have only been tested against stand-in services, not real
  hardware.
- Wi-Fi: no hidden or enterprise networks. Display: no arranging of several
  monitors, and a chosen mode is not kept across sessions.
- Tray menus do not draw icons in their rows, and only the hicolor icon theme
  is searched.
- Clicking a notification does not raise the app's window.
- The dock is on one monitor.
- Wi-Fi cards that need non-free firmware (most Intel cards) do not work yet.
- Secure Boot has to be turned off to boot the ISO.

## 0.4.0

Notifications, quick settings and a system tray, all our own.

### Added

- sengi-notify, the notification service. Cards appear at the top right with
  their actions; the bell in the bar opens a notification centre with history
  and do-not-disturb. It starts the first time something needs it, so it
  costs nothing until then. A notification belongs to the program that sent
  it, new cards ignore clicks for a moment, and no one program can fill the
  screen.
- sengi-quick, the quick settings panel, opened from the network, volume and
  battery items in the bar: Wi-Fi with a network list, Bluetooth, volume,
  brightness, do-not-disturb, battery, lock and session.
- A system tray in sengi-bar, with the apps' own menus drawn in our style.
- The shared button and toggle take keyboard focus.

### Changed

- mako is no longer in the image.
- Programs drawn through sengi-layer repaint only what changed and give
  their buffers back when hidden.
- The live user is in the bluetooth group.
- After logging out, the console login works.

### Known limits

- Quick settings' Wi-Fi, Bluetooth, brightness and battery tiles have only
  been tested against stand-in services, not real hardware.
- Wi-Fi: no hidden or enterprise networks, and no forgetting a network from
  the panel. Bluetooth: no pairing from the panel.
- Tray menus do not draw icons in their rows, and only the hicolor icon theme
  is searched.
- Clicking a notification does not raise the app's window.
- The dock is on one monitor.
- Wi-Fi cards that need non-free firmware (most Intel cards) do not work yet.
- Secure Boot has to be turned off to boot the ISO.
- The clock shows UTC.

## 0.3.0

The whole shell is now our own: bar, dock and launcher.

### Added

- sengi-dock, the dock. Running windows attach to their pinned icon instead
  of adding a second one, the focused app is marked, and a click launches,
  focuses, minimises or cycles through an app's windows. Hovering shows the
  app's name. Pins live in /etc/xdg/sengi-os/dock.toml, with a per-user
  override in ~/.config/sengi-os/dock.toml.
- sengi-launcher, the search launcher. It ranks apps by how well they match
  and how often they are used, shows a message when an app cannot be
  started, and closes on Escape or a click outside. Its menu mode draws the
  session menu.
- sengi-layer, the library the bar, dock and launcher are drawn with: Slint
  on wlr-layer-shell, with keyboard input, one surface per monitor and
  per-surface control.
- A bar on every monitor.

### Changed

- Waybar and fuzzel are no longer in the image.
- The bar and dock are restarted if they exit, more slowly if they keep
  exiting, and what they print goes to the journal.

### Known limits

- There is no system tray yet. Planned for 0.4.
- The dock is on one monitor. Ctrl- and Shift-click do not start a new
  instance; middle-click does.
- The launcher lists apps without icons, and long right-to-left names can be
  highlighted in the wrong order.
- Wi-Fi cards that need non-free firmware (most Intel cards) do not work yet.
- Secure Boot has to be turned off to boot the ISO.
- The clock shows UTC.

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
