# sengi-settings

The Sengi OS settings application: an ordinary window with the categories
down the left and the chosen one's page at the right.

    sengi-settings            open Settings
    sengi-settings sound      open it at a page

The pages are `network`, `bluetooth`, `sound`, `display`, `wallpaper`,
`power`, `date-time`, `keyboard`, `mouse`, `notifications`, `updates` and
`about`. If Settings is already open, a second invocation brings that window to the page it
was asked for and exits; the two meet on a socket in `$XDG_RUNTIME_DIR`.

It is not a service. It starts when asked for, exits when its window is
closed, and leaves nothing running.

The compositor draws the title bar. The window's application id is
`sengi-settings`, which is also the name of the `.desktop` file in this
directory; the image installs that as
`/usr/share/applications/sengi-settings.desktop`.

## The settings file

Settings that Sengi OS keeps itself are in one file,
`$XDG_CONFIG_HOME/sengi-os/settings.toml` (normally
`~/.config/sengi-os/settings.toml`). A system-wide file of the same name
under `/etc/xdg/sengi-os/` gives defaults; a key in the user's file wins,
and a key in neither has the default below. Only the user's file is ever
written, one key at a time, by writing a new file beside it and renaming
it into place.

The file is plain text meant to be read and edited: `[section]` headers
and `key = value` lines, where a value is a quoted string, a whole number,
`true` or `false`, or a one-line list of strings. Comments start with `#`.
Lines the program does not understand, and keys it does not know, are
reported on standard error and kept exactly as they are when it rewrites
the file. A value of the wrong kind or out of range is reported and
ignored in favour of the system's value or the default.

| Key | Default | Meaning |
| --- | --- | --- |
| `power.dim-after` | `120` | Seconds without input before the screen is dimmed. `0` is never. Not used if it is not before `blank-after`. |
| `power.blank-after` | `300` | Seconds without input before the screen is switched off. `0` is never. At most 14400. |
| `power.lock-after` | `0` | Seconds without input before the screen is locked. `0` is never. |
| `power.lock-on-sleep` | `true` | Lock the screen when the machine is suspended. |
| `power.lid` | `"sleep"` | What closing the lid does: `"sleep"` (what the system does) or `"nothing"`. Only offered on a machine with a lid. |
| `clock.format` | `"24h"` | `"24h"` or `"12h"`, for the clock in the bar. |
| `clock.time-zone` | `""` | A zone name such as `"Europe/Paris"` for this user's session. Empty uses the system's zone. |
| `keyboard.layouts` | `["us"]` | One to four XKB layouts, each `layout` or `layout(variant)`. |
| `keyboard.switch` | `""` | The XKB option that switches between layouts, such as `"grp:alt_shift_toggle"`. |
| `keyboard.repeat-delay` | `600` | Milliseconds a key is held before it repeats. 100 to 2000. |
| `keyboard.repeat-rate` | `25` | Repeats per second. 1 to 100. |
| `mouse.speed` | `0` | Pointer speed of mice, -100 to 100. |
| `mouse.natural-scroll` | `false` | Scrolling moves the content, not the view. |
| `mouse.left-handed` | `false` | Swap the left and right buttons. |
| `touchpad.speed` | `0` | Pointer speed of touchpads, -100 to 100. |
| `touchpad.natural-scroll` | `false` | As for mice. |
| `touchpad.tap-to-click` | `true` | A tap is a click. |
| `touchpad.disable-while-typing` | `true` | Ignore the touchpad while keys are being pressed. |
| `touchpad.click-method` | `"clickfinger"` | `"clickfinger"` (one finger left click, two fingers right click) or `"areas"` (the bottom corners are the buttons). |
| `touchpad.scroll-method` | `"two-finger"` | `"two-finger"` or `"edge"` (one finger along the right or bottom edge). |
| `touchpad.gestures` | `true` | Three-finger swipes: left and right switch windows, up shows the switcher (sengi-compositor). |
| `display.primary` | `""` | The output the bar and the dock go to, such as `"eDP-1"`. Empty is the one the pointer is on (sengi-compositor). |
| `display.night-light` | `false` | Warm the screen between `night-from` and `night-to` (sengi-compositor). |
| `display.night-from` | `"21:00"` | When the night light comes on, `HH:MM`, local time. |
| `display.night-to` | `"07:00"` | When it goes off. |
| `display.night-strength` | `50` | How warm, 0 to 100. 0 is no tint, 100 is about 3000 K. |
| `appearance.window-shadows` | `true` | Whether framed windows have a shadow (sengi-compositor). |
| `appearance.wallpaper` | `"solid"` | `"solid"` for the desktop colour, or the full path of a PNG or JPEG to show behind the desktop. |
| `appearance.week-start` | `"monday"` | `"monday"`, `"sunday"` or `"saturday"`: the first column of the calendar under the clock. Edit the file; Settings has no control for it yet. |

Two options are for the session's start-up scripts, which should not have
to read TOML:

    sengi-settings --get power.lock-after    print the value in effect
    sengi-settings --apply                    write the files below
    sengi-settings --wallpaper               print the picture to show, if any

A third changes a setting from a script or a test the way the window
does: the value is checked and saved, the files below are written, and
the running compositor is told.

    sengi-settings --set keyboard.layouts us,de
    sengi-settings --set mouse.natural-scroll true

labwc is told with `labwc --reconfigure` and reads its files again.
sengi-compositor reads `settings.toml` itself and is told with
`sengi-compositor --reload`.

### Files written for other programs

labwc reads the user's `rc.xml` and `environment` *instead of* the
system's, and the system's `rc.xml` carries the Sengi OS theme and key
bindings. So the user's copies are written as the system files with the
settings added:

- `~/.config/labwc/rc.xml` is generated whole from `/etc/xdg/labwc/rc.xml`
  plus key repeat and pointer settings, and says so in its first comment.
  It exists only while one of those settings differs from the default. A
  `rc.xml` without that comment is taken to be the user's own and is never
  touched; Settings says so when a setting cannot be applied because of it.
- `~/.config/labwc/environment` keeps the user's own lines. Above them,
  between two marker comments, it carries the system file's lines; below
  them, between two more, `TZ` for the time zone and `XKB_DEFAULT_LAYOUT`,
  `XKB_DEFAULT_VARIANT` and `XKB_DEFAULT_OPTIONS` for the keyboard. labwc
  lets a later line replace an earlier one, so the user's lines win over
  the system's, and a setting that was changed from its default wins over
  both. A layout or variant that XKB's `evdev.lst` does not list, or a
  zone that is not under `/usr/share/zoneinfo`, is left out and reported.

Both are written whole and renamed into place. A file that is replaced
keeps its permissions, and a symbolic link is written through to the file
it leads to. The same goes for `settings.toml`, which is also read again
before a key in it is changed, so an edit made by hand while Settings is
open is kept.

After writing them Settings runs `labwc --reconfigure`. The generated
`rc.xml` is a copy, so it goes stale when an update changes the system
file; running `sengi-settings --apply` before labwc starts writes it
afresh.

## Pages

No page ever starts a service: a call on the bus that would start iwd or
BlueZ on a machine without the hardware is made with the no-auto-start
flag, and the page says the service is not running. A page is drawn at
once and fills in as its service answers, from a worker thread that
sleeps until the service signals a change. The Network, Bluetooth and
Sound pages let that thread and its watch go when another page is shown,
unless a join, a pairing or a connection is under way, and then when
that ends.

There is one dialog. It belongs to the page that opened it, which is
the only one that can change or close it. A notice waits until no
question is open; another page's question backs the open one out as
Escape would. A close that finds no dialog of the caller's own is said
on standard error, and stops a debug build.

### Network

Wi-Fi through iwd on the system bus (`net.connman.iwd`), with an agent for
passphrases; cabled interfaces from `/sys/class/net`; addresses from the
kernel (`getifaddrs`), whoever configured them.

- The switch powers the adapter. A radio held off by rfkill is reported,
  not unblocked.
- The connected network has a card of its own with signal, security and
  address, and buttons to disconnect and to forget.
- Up to 24 networks are listed. Those iwd has joined before come first,
  then by signal level, then by name. The list does not move while the pointer is over it or the
  keyboard is in it; a network that drops out of range meanwhile stays as
  a row that cannot be used.
- A known network joins at once. If its stored passphrase has stopped
  working, the new one is asked for.
- A new network with a passphrase asks for it. A wrong one is said to be
  wrong and the field is emptied.
- A new open network asks first, saying that it is not encrypted.
- Forgetting a network asks first.
- The passphrase is never logged, put on a command line or written to
  disk by Settings; iwd stores it. sengi-system's `secret.rs` says which copies in
  memory are overwritten and which are only freed. The process cannot be
  core-dumped. The agent hands the passphrase only to the iwd it was
  typed for, for the network it was typed for, once. All of this is
  sengi-system's Wi-Fi service, the one the quick settings panel uses.
- Names are chosen by strangers. They are stripped of control, direction
  and invisible characters, cut to 32 characters, and wrapped in
  directional isolates wherever they appear inside a sentence.

Not done: hidden networks, enterprise (802.1X) networks without a
provisioning file (listed, and say what they need), a second Wi-Fi
adapter, static addresses and DNS.

### Bluetooth

BlueZ on the system bus (`org.bluez`).

- The switch powers the first adapter. rfkill blocks are reported.
- "My devices" are the paired ones: Return or a click connects or
  disconnects, Delete removes after asking.
- "Search" looks for devices for 30 seconds, or until stopped or the page
  is left. Devices that announce no name are not listed.
- Pairing registers an `org.bluez.Agent1` (capability `KeyboardDisplay`,
  not the default agent) and shows what BlueZ asks in the dialog: a
  number to compare, a PIN or passkey to type, a number to type on the
  device. The agent answers only the BlueZ the pairing was asked of, and
  only about the device being paired; anything else is refused unseen,
  a number that is only to be displayed included. A PIN is masked, held
  like a passphrase, and never logged. The agent is sengi-system's
  (`pairing`).
- The page's worker and its watch on the bus run while the page is
  showing, or a pairing or connection is under way, and not otherwise.
- A paired device is marked trusted, so it can reconnect later without an
  agent, and connected.

Not done: more than one adapter, making this computer discoverable,
renaming devices, battery levels of devices.

### Sound

PipeWire through WirePlumber's `wpctl`: `wpctl status` for the outputs and
inputs, `set-default`, `set-volume -l 1` and `set-mute` to change them.

- Each direction has its device, a volume slider and a mute button. The
  slider stops at 100%; a drag runs one `wpctl` at a time and skips the
  positions it has already moved on from. While the pointer or an arrow
  key is moving it, the slider is the user's: a reading of the volume
  made before its newest position was carried out does not move it.
- A change PipeWire does not take is said in a dialog.
- Changes made elsewhere (the volume keys, headphones plugged in) show at
  once. `wpctl` cannot wait for a change, so a `pw-dump --monitor` child
  is kept while the page is showing and read for which object changed:
  only a node, a device or the defaults is a reason to read again. The
  page's own `wpctl` shows there as a client coming and going, and is
  not. The child is killed when the page is left or the window closes,
  and by the kernel if Settings dies first.

Not done: a level meter (it would need a capture stream of its own, which
is not cheap), per-application volumes, device profiles.

### Display

Outputs through the compositor's `wlr-output-management-unstable-v1`, on a
Wayland connection of its own; brightness read from
`/sys/class/backlight` and set through logind's `Session.SetBrightness`.

- Each connected output has its resolution, refresh rate and scale.
- No change is final until it is confirmed. A dialog asks whether to keep
  it and counts down from 15 seconds; "Go back", Escape, no answer, or
  closing the window all restore what was there before. The clock is the
  worker thread's, not the window's, so a window that cannot be seen or
  has hung still gets its picture back, and it is looked at on every turn
  of the worker's loop, so a compositor with a lot to say cannot put the
  moment off. SIGTERM, SIGINT and SIGHUP restore it too, from a thread of
  their own, before Settings exits; only SIGKILL in those 15 seconds
  leaves the new mode in place. If the compositor says the outputs
  changed while it was going back, Settings asks again a few times, and
  if it still cannot go back it says so instead of claiming it did.
- An output that lists no modes with a refresh rate, as in a virtual
  machine, takes any size and is offered a few common ones.
- The brightness slider never goes below 5%.

Not done: arranging several displays, mirroring, rotation, switching an
output off, saving the layout across sessions (labwc keeps what it is
given only until it exits; a kept mode is not written to a file yet).

### Wallpaper

- The picture behind the desktop: `solid` (the default, the desktop
  colour), one of the six wallpapers Sengi OS ships, or a PNG or JPEG of
  the user's. The setting is `appearance.wallpaper`: `solid`, or the full
  path of the picture.
- The page shows what is in use with a preview, a grid of the shipped
  wallpapers with their thumbnails, a field for the full path of a picture
  (with Return or Use), and the PNG and JPEG files directly in
  `~/Pictures`, each with its size or the reason it cannot be used. There
  is no file chooser yet: once Files exists it can fill the same field.
- A file is judged by its first bytes, not its name: it has to be a
  readable PNG or JPEG of at most 10 megapixels and 64 MB. swaybg keeps
  the decoded picture while it runs (four bytes a pixel), so a picture over
  2.5 megapixels says what it costs, for example "about 34 MB". A picture
  that is refused says why next to the field.
- A picture that goes missing or stops being readable after it was chosen
  does not break the session: the desktop colour is shown, and the page
  says that the chosen picture cannot be shown and why.
- The change shows at once. After the key is written Settings runs
  `sh /usr/lib/sengi-os/wallpaper.sh apply`, which starts a new swaybg with
  the picture that `sengi-settings --wallpaper` prints and then ends the
  old one. At login `session-start` runs the same function. The lock and
  login screens have their own look and ignore the setting.
- Previews are decoded by Settings itself and only for PNG: the shipped
  wallpapers come with a 192 by 108 thumbnail, and a picture of the user's
  is shrunk as it is read. A JPEG is listed by name and size and is not
  drawn.

### Power

- The battery from `/sys/class/power_supply`: charge, state and time
  left, health (what it holds now against what it held new), draw, cycle
  count and type. Two batteries are one charge, worked out from the
  energy in both, and each has a row for its own charge and health. The
  reading and the adding up are sengi-system's, so the quick settings
  panel shows the same charge. Read
  when the page opens and every 30 seconds while it is showing, not
  otherwise.
- When to dim the screen, switch it off and lock it, whether to lock on
  sleep, and what closing the lid does: `power.dim-after`,
  `power.blank-after`, `power.lock-after`, `power.lock-on-sleep`,
  `power.lid`. Under sengi-compositor the compositor reads them itself and
  is told to read them again after each change (`sengi-compositor
  --reload`), so they take effect at once; the lid choice is only shown on
  a machine whose input devices include a lid switch. Under labwc the
  session's start-up script reads them with `--get` and starts `swayidle`
  accordingly; `sengi-settings --screen off|on` is the command that
  switches the displays, through `wlr-output-power-management`. A running
  `swayidle` cannot be given new times, so after a change Settings ends
  the session's one, which the session starts again, but only one whose
  environment carries `SENGI_IDLE_SUPERVISED=1`: ending one that nobody
  restarts would leave the session never locking. Without the mark the
  page says the change applies at the next login.
- The power button is logind's (`HandlePowerKey`), the same for every user
  and changed only as root (`/etc/systemd/logind.conf.d`). It is shown,
  read from logind, with where to change it.

### Date & Time

- The time zone chosen here is the user's own: `clock.time-zone`, written
  as `TZ` into labwc's environment, so that everything the session
  starts from then on has it. Programs already running keep the old zone
  until the next login. The list is tzdata's `zone.tab`, which has a line for every country
  (`zone1970.tab` would lose Kigali to Maputo), searched by
  typing.
- `clock.format` is `24h` or `12h`, for the clock in the bar.
- The system's zone and the state of network time are read from
  timedated and shown. Changing them (`SetTimezone`, `SetNTP`) is guarded
  by polkit actions that want an administrator; the image has no polkit,
  so systemd refuses everyone but root, and Settings does not offer what
  would only fail. `src/datetime.rs` has the details.

### Keyboard

- Up to four layouts, each with a variant, picked from the XKB database
  (`/usr/share/X11/xkb/rules/evdev.lst`) by typing part of the name.
  Delete removes one; the last stays.
- The shortcut that switches between them, from a short list of XKB
  `grp:` options. A second layout added while none is chosen gets
  Alt + Shift, so it can be reached.
- Repeat delay and rate.
- Keyboard shortcuts…, under sengi-compositor only: runs
  `sengi-compositor --shortcuts`, which shows the compositor's sheet of
  its key bindings (also Super+/ anywhere).

All of it is written for labwc as described under "Files written for
other programs", and labwc is told to read its files again.

### Mouse & Touchpad

Pointer speed, natural scrolling and left-handed buttons for mice;
speed, natural scrolling, tap to click and ignoring the touchpad while
typing for touchpads. They are written as two `<libinput>` device
categories, `non-touch` and `touchpad`, in the generated `rc.xml`, which
labwc applies to the devices present when it reconfigures.

### Notifications

Do not disturb, and a button that dismisses every notification on
screen. Both belong to the notification service and are reached through
its command, `sengi-notify --dnd on|off|status` and `sengi-notify
--dismiss-all`; the page says when the service is missing or silent.

### Updates

System packages and Flatpak apps that can be updated, a Check for updates
button, Update all with its progress and a Cancel, and Restart when the
system asks for one. The work and the reasons for how it is done (the
password, polkit, what is run as root) are in `crates/sengi-update`. The
sidebar shows the count from the last look, which Settings makes when it
starts if the last is over a day old. Nothing runs in the background.

### About

The system's name and version from `/etc/os-release`, the kernel, the
processor, installed memory, the graphics adapters (maker and driver; the
image has no PCI database to name the model), the disk, and the memory
in use at this moment: `MemTotal` less `MemAvailable`, the figure the
idle budget is measured in. It is measured when the page opens and when
asked, not on a timer.

## Building and testing

    cargo build -p sengi-settings
    cargo test -p sengi-settings

Debug builds have two aids that release builds are compiled without, so
that the shipped program can only ever show the real machine:

- `sengi-settings --demo NAME` runs over made-up services, for looking at
  states the machine at hand cannot produce. An unknown name lists them.
- `SENGI_SYS_CLASS=DIR` reads `DIR` in place of `/sys/class`, the same
  variable sengi-quick reads.
  With `DBUS_SYSTEM_BUS_ADDRESS` pointing at a private bus this lets the
  real bus code be run against stand-ins for iwd, BlueZ and logind.

`SENGI_SETTINGS_TRACE=1` prints how long the window took to come up.
