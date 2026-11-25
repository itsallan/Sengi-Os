# Changelog

All notable changes to Sengi OS. Each release section lists what is in its ISO
and the measured idle RAM and boot time.

## Unreleased

### Added

- Settings, Display: the displays drawn where they are, a Position choice for
  each (left of, right of, above, below or mirroring another), and a Main
  display, which is where the bar and the dock go. Positions and scales are
  tried for 15 seconds and go back unless kept, and are remembered for each
  display. Mirroring puts two displays of the same size in one place.
- Night light: Settings, Display has a switch, a start and end time (21:00 to
  07:00 unless set) and a warmth. It is a colour ramp on the display, so it
  costs nothing per frame and screenshots stay untinted. Needs a display
  driver with a colour ramp, which a virtual machine's usually lacks.
- Touchpad: Settings, Mouse & Touchpad has Click (by number of fingers or by
  the bottom corners), Scroll (two fingers or the edge) and Three-finger
  swipes. Left and right swipes switch windows, up shows the switcher, down
  picks the selected window. Pinching is ignored. None of this has been tried
  on a real touchpad yet.
- Screenshots: Quick settings has Screen, Area and In 5 s. `sengi-shot --select`
  dims the screen and you drag a rectangle (live size label, Enter or letting
  go takes it, Escape cancels); `--delay N` counts down in a corner before the
  shot; `--clipboard` copies the picture with wl-copy when it is installed and
  says so in the notification when it is not. The two screens are the new
  sengi-snip, so a plain screenshot still links no toolkit.
- Calculator (sengi-calc): decimal arithmetic by hand with the usual
  precedence, brackets and percent, so 0.1 + 0.2 is 0.3; keyboard or keys,
  the last five sums, Ctrl+C copies the answer, division by zero says so.
- Click the clock in the bar and a calendar opens (sengi-calendar): a month grid
  with today marked, previous and next month, Today, Escape or a click outside
  closes it. `appearance.week-start` in settings.toml (monday, sunday or
  saturday) sets the first column.

- The screen dims after 2 minutes without input and goes off after 5, and
  any key, click or movement brings it back. Dimming, switching off and an
  optional lock are timed by sengi-compositor itself, with no polling and
  without swayidle, which is now only started under labwc. The lock after
  idling is off unless you set it; locking before sleep stays on.
- Closing the lid puts the computer to sleep, on battery and on mains, and
  the screen is locked before it does, so the lock screen is there on
  waking. A short press of the power button sleeps and holding it switches
  off. These are logind settings in the image.
- Settings, Power: dim, switch off and lock times, and "When the lid is
  closed" (sleep or do nothing), which only shows on a computer that has a
  lid. They are kept in settings.toml and take effect at once.
- While you drag a window to a screen edge, a flat see-through outline in
  the accent colour shows where it will land, and goes when you let go.
- Super+/ (or Super+F1) shows a sheet of every keyboard shortcut, made from
  the same table the compositor binds the keys with. Settings, Keyboard has
  a Keyboard shortcuts button for it.

- A light theme next to the dark one. Settings, Appearance has Dark and Light
  with a preview of each, and it applies at once: the bar, dock and desktop
  restart in the new colours, window frames and menus are repainted, and
  anything opened later starts in it. Saved as `appearance.theme`. The
  palettes are in `tokens/sengi.toml` with their WCAG contrast numbers, which
  the tests check (4.5 for text, 3 for parts of controls).
- Settings has a Wallpaper page: the plain colour, six flat wallpapers in the
  Sengi OS colours, or a PNG or JPEG of your own, by path or from ~/Pictures.
  It shows at once and is remembered; a missing or broken file falls back to
  the desktop colour with a note.
- Files (sengi-files), a small file manager: places on the left, the folder
  in the middle, a path bar with Back and Up. Open folders and files, rename
  (F2), make folders (Ctrl+Shift+N), copy, cut and paste (never over an
  existing name: the copy becomes "name 2"), move to the Trash with Delete,
  and a Trash view with Restore and Empty Trash. It only changes things inside
  your home folder. The desktop opens folders in it, and it is at the end of
  the default dock.
- Files polish: a right-click menu on items and on empty space, the list
  follows the folder when other programs change it, natural order ("file2"
  before "file10"), drag and drop of items onto folders and places, Alt+Right
  for forward, and Phosphor Bold glyphs on the places and buttons.
- Double-click a file and it opens (sengi-open): text, code, Markdown and logs
  in Text Editor, PNG and JPEG pictures in Image Viewer, folders in Files, and
  a notification "Nothing can open this file" for anything else. Files and
  the desktop use it, and the default apps are set for Flatpak apps and the Store.
- Text Editor (sengi-edit), a plain UTF-8 editor: Ctrl+S saves, Ctrl+Z and
  Ctrl+Shift+Z undo and redo, Ctrl+F finds, and closing with unsaved changes asks
  first. Files that are not text or are over 5 MB are not opened, and it says why.
- Image Viewer (sengi-view) for PNG and JPEG: fit to the window, + - 0 to zoom,
  drag to move, Left and Right through the folder, the size in pixels and the
  size of the file in the status line. Pictures over 100 megapixels are refused.
- The Store shows real install progress read from Flatpak: a bar with the
  percentage, the step and the speed when Flatpak gives them, and an
  honest moving bar when it does not.
- The Store shows an Open button for installed apps, also right after an
  install, and it starts the app.
- Right-click a dock tile to keep the app in the dock or remove it, open it
  or quit it. Pins are saved to ~/.config/sengi-os/dock.toml and survive a
  restart; running apps that are not pinned still follow the pinned ones.
- sengi-desktop: the folders and files of ~/Desktop show as icons under the
  windows. Right-click for New Folder and New File (both start in rename), Open Terminal
  Here and Settings; select, rename, open and move to the Trash. The image has
  no file manager yet, so opening a folder says so.
- Every window gets the Sengi OS title bar with close, minimise and maximise,
  also Chromium, Firefox and other apps that ask to draw their own (they show
  their bar inside ours).
- A level indicator: the volume, mute, screen brightness and keyboard light
  keys show a rounded panel with an icon and a bar near the bottom of the
  screen for about a second (sengi-osd). It runs only then.
- Print saves a PNG of the screen to ~/Pictures (Screenshot-DATE-TIME.png) and
  Alt+Print or Shift+Print the window in front, with a notification that says
  where it went (sengi-shot).
- A Screenshot button in quick settings: the panel closes and the whole screen
  is saved to ~/Pictures, like the Print key does.
- Laptop keys: the screen brightness keys step the backlight by 5 percent and
  never go below 5 percent, and the keyboard light keys (up, down, on/off)
  work too, also on the lock screen. They run sengi-brightness, which asks
  logind, so no root is needed.
- Windows have a soft shadow, darker for the window you are working in. It
  is drawn in flat steps with no blur, costs nothing per frame, and can be
  switched off with `window-shadows = false` under `[appearance]` in
  `settings.toml`.
- The launcher shows every installed app as a grid of icons with names while
  nothing is typed; typing narrows it to the list, which now has a small icon
  in front of each name. The arrow keys move through the grid.
- sengi-store, a small app store: type the name of an app, pick it from the
  Flathub list, and it is installed for your user. It has its own icon at the
  end of the dock. Flathub is set up as the Flatpak source in the image.
- The installer can install Sengi OS beside another system: on a UEFI
  computer, with a GPT disk that has an EFI system partition and 10 GB or more
  of free space, it adds one partition there, adds a Sengi OS folder to the EFI system
  partition and puts Sengi OS first in the firmware boot list; the other
  system's files are not touched and it stays in the boot menu, shown for 3
  seconds. Shrink the other system first, from inside it.
  Known limits: not offered with Secure Boot on or with a hybrid MBR; a
  BitLocker or TPM setup may ask for its recovery key once after the boot list
  changes; the 3-second menu wait is left out of the boot-time figure the
  install test reports (it is waiting for a person, and is reported on its
  own); only tried on virtual disks.
- Wi-Fi and Bluetooth firmware is in the image: Intel (both radios), Realtek,
  MediaTek, Atheros and Broadcom, plus Intel graphics firmware and CPU
  microcode, from Debian's non-free-firmware section.
- The licence: LICENSE (GPL-3.0-or-later), licence fields on every crate, and
  the licences of the Rust crates the programs are built from in
  /usr/share/doc/sengi-os.
- The notification centre groups the history by app, with a count and a Clear
  button for each group, says "2 min ago" instead of a clock time, and keeps
  the last 50 across a restart in a private file in ~/.local/state/sengi-os.
- Drag a pinned tile in the dock left or right to reorder it, or up and out
  of the dock to remove it. The order is saved to dock.toml.
- Right-click an app in the launcher for Open, and Keep in Dock or Remove from
  Dock. The dock picks up the change within a couple of seconds.

### Changed

- The accent colour is green (#2f9e6b), not orange.
- The Installer and Settings app icons are Phosphor Bold glyphs, like the
  Store's.

### Fixed

- Settings > About: rows of facts are placed by hand so a value cannot land
  beside the wrong label, the live system's disk line says it is held in
  memory, and a test guards the layout.
- Windows never appeared on small screens such as 800x600: a window asked to
  shrink never got the frame callback it waits for.
- App icons that draw nothing, such as xterm's small icon, no longer leave a
  blank tile in the dock: the program's name is tried next, then a letter.
- The dock's search button is a dark tile like the others, not a permanent
  orange one.
- The installed system's boot menu says "Sengi OS", not "Sengi OS GNU/Linux".
- The time zone list has Kigali, and every other country's own city.
- What a program started from the dock or the launcher prints goes to the
  journal, so one that fails to start leaves a trace (`journalctl -t name`).
- Polish pass: the sections of the Display, Sound and installer Disk pages are
  spaced like the other pages, the Store list keeps a readable width on a wide
  screen, and the launcher's letter tiles match the dock's and the Store's.

### Known limits

- Flatpak runtimes are large and the live USB keeps changes in RAM: install
  Sengi OS to the disk before installing big apps.
- Secure Boot has to be off.
- The Store opens a second window if started twice.
- Dual boot has been tested with a stand-in for Windows, not a real Windows
  disk, and only on UEFI.

## 0.8.0

Our own compositor: sengi-compositor is now what Sengi OS runs. labwc stays
in the image as the way back.

### Added

- sengi-compositor runs the whole session: xdg windows with our title bar,
  frame and rounded corners, an Alt+Tab switcher, the desktop and window
  menus, edge snapping, fullscreen, several screens with their own scale,
  mode and place (Settings' Display page speaks to it), screen capture,
  primary selection, activation, fractional scale, pointer constraints and
  the lock screen and login screen. It starts Xwayland with the session, so
  X11 programs run.
- A way back to labwc. The live image's boot menu has an entry "Sengi OS
  (labwc compositor)", which L picks directly while the machine starts,
  and the installed system's menu has the same entry beside the text
  mode one. The word `sengi.compositor=labwc` on the kernel command line,
  or `labwc` in /etc/sengi-os/compositor or /run/sengi-os.compositor, does
  the same on any system.
- The session falls back to labwc if sengi-compositor does not come up. It
  writes a ready file once a screen is lit and the session program runs;
  if that file is not there in 15 seconds the compositor is ended, and one
  that exits with an error within ten seconds is replaced too, the login
  screen as well as the session. After any error exit the rest of that boot
  uses labwc, provided /run/sengi-os exists and can be written, which the
  image sees to.
- If drawing keeps failing, 20 frames in a row, the compositor switches to
  drawing on the CPU, and if that fails it exits with an error, which the
  fallback above then covers.
- scripts/test-compositor-sh.sh tests the choice of compositor and the
  fallbacks; CI runs it.

### Changed

- sengi-compositor is the default compositor, for the login screen and
  the session, on the live image and on an installed system. Nothing has
  to be asked for. The "sengi-compositor preview" boot entry is gone, as
  it is the default entry now. The boot test and the install tests run on
  it, and CI keeps one boot test and one install test on labwc.
- The start-up programs (bar, dock, wallpaper, idle timer) no longer wait
  for Xwayland, which cost up to five seconds when it was slow. The display
  is chosen first and every program gets it at once; an X11 program that
  starts early waits on the socket.
- sengi-compositor ends with an error when the program it runs fails, where
  it used to end as if all were well.
- A window that is minimised or on no screen no longer keeps the screen on
  and the lock from coming. Programs in a sandbox are not offered the
  idle-inhibit protocol, so a Flatpak app cannot turn the auto-lock off.
- The boot test fails if the idle RAM or the boot time is missing from its
  report; before it passed.

### Known limits

- sengi-compositor has been run in QEMU only. It has not been run on real
  hardware yet, and the display paths (modes, hotplug, suspend and resume,
  the lid) are unverified on a real machine.
- No brightness or Fn keys and no lid handling: a closed lid with an
  external monitor keeps the laptop panel in the layout.
- No clipboard between X11 and Wayland programs, no text-input, and no
  touch or tablet support.
- Scales 2 and 1.5 have not been looked at on the image.
- The fall back from a GPU that cannot draw has not been run with a broken
  GPU.

### Try it

If the desktop does not come up, or its graphics do not work, labwc is the
way back. On the live image pick "Sengi OS (labwc compositor)" in the boot
menu (hold Shift or press Esc for the menu, or press L as the machine
starts). On an installed system pick "Sengi OS (labwc compositor)" in the
menu, which Esc shows; or press E on the first entry in GRUB and add
`sengi.compositor=labwc` to the `linux` line; or start the text mode entry,
log in, and write `labwc` into /etc/sengi-os/compositor to make it stay.
If sengi-compositor fails to start, the session starts labwc by itself.

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
  disk.
- The installed system's boot menu is hidden and waits one second for
  Esc, which shows it. It has an entry "Sengi OS, text mode" for when the
  desktop does not start: the same kernel without the login screen, with
  a text login where you log in as yourself and have sudo.
- The disk is known by its serial number and world-wide name as well as
  its model and size, is listed with how it is attached, and is held
  open against any other use while its partition table is written. What
  the kernel then shows of the new table is checked before anything is
  formatted.
- The installation runs as a service of its own. It goes on if the
  window is closed or killed or the session ends, holds off sleep and
  shutdown until it is over, and a window opened again finds it.
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
  the password that was chosen and checks what it finds, then starts it
  once more to see that Esc shows the boot menu. CI and the release run
  it after the boot test, through BIOS and through UEFI.

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
- An installed system does not look for updates by itself. They come when
  asked for: `sudo apt update && sudo apt upgrade`.
- The installed system's boot menu costs one second at every start, the
  price of being able to reach it.
- One language, English, and no choice of locale.
- The time zone has to be chosen from the list; nothing guesses it. The
  list is by principal city, so some capitals are not in it by name.
- A full name in a script the system's typeface lacks (Inter has Latin,
  Cyrillic and Greek) is stored as typed but may be drawn as boxes.
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
