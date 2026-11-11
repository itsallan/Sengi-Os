# sengi-settings

The Sengi OS settings application: an ordinary window with the categories
down the left and the chosen one's page at the right.

    sengi-settings            open Settings
    sengi-settings sound      open it at a page

The pages are `network`, `bluetooth`, `sound`, `display`, `power`,
`date-time`, `keyboard`, `mouse`, `notifications` and `about`. If Settings
is already open, a second invocation brings that window to the page it
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
| `power.blank-after` | `300` | Seconds without input before the screen is switched off. `0` is never. At most 14400. |
| `power.lock-after` | `300` | Seconds without input before the screen is locked. `0` is never. |
| `power.lock-on-sleep` | `true` | Lock the screen when the machine is suspended. |
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

Two options are for the session's start-up scripts, which should not have
to read TOML:

    sengi-settings --get power.lock-after    print the value in effect
    sengi-settings --apply                    write the files below

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
- `~/.config/labwc/environment` keeps the user's own lines. Between two
  marker comments it carries the system file's lines, `TZ` for the time
  zone, and `XKB_DEFAULT_LAYOUT`, `XKB_DEFAULT_VARIANT` and
  `XKB_DEFAULT_OPTIONS` for the keyboard.

After writing them Settings runs `labwc --reconfigure`. The generated
`rc.xml` is a copy, so it goes stale when an update changes the system
file; running `sengi-settings --apply` before labwc starts writes it
afresh.

## Pages

No page ever starts a service: a call on the bus that would start iwd or
BlueZ on a machine without the hardware is made with the no-auto-start
flag, and the page says the service is not running. A page is drawn at
once and fills in as its service answers, from a worker thread that
sleeps until the service signals a change.

### Network

Wi-Fi through iwd on the system bus (`net.connman.iwd`), with an agent for
passphrases; cabled interfaces from `/sys/class/net`; addresses from the
kernel (`getifaddrs`), whoever configured them.

- The switch powers the adapter. A radio held off by rfkill is reported,
  not unblocked.
- The connected network has a card of its own with signal, security and
  address, and buttons to disconnect and to forget.
- Networks iwd has joined before come first, then by signal level, then
  by name. The list does not move while the pointer is over it or the
  keyboard is in it; a network that drops out of range meanwhile stays as
  a row that cannot be used.
- A known network joins at once. If its stored passphrase has stopped
  working, the new one is asked for.
- A new network with a passphrase asks for it. A wrong one is said to be
  wrong and the field is emptied.
- A new open network asks first, saying that it is not encrypted.
- Forgetting a network asks first.
- The passphrase is never logged, put on a command line or written to
  disk by Settings; iwd stores it. `src/secret.rs` says which copies in
  memory are overwritten and which are only freed. The process cannot be
  core-dumped. The agent hands the passphrase only to the iwd it was
  typed for, for the network it was typed for, once.
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
  device. The agent answers only the BlueZ it registered with, and only
  about the device being paired; anything else is refused unseen. A PIN is
  masked, held like a passphrase, and never logged.
- A paired device is marked trusted, so it can reconnect later without an
  agent, and connected.

Not done: more than one adapter, making this computer discoverable,
renaming devices, battery levels of devices.

### Sound

PipeWire through WirePlumber's `wpctl`: `wpctl status` for the outputs and
inputs, `set-default`, `set-volume -l 1` and `set-mute` to change them.

- Each direction has its device, a volume slider and a mute button. The
  slider stops at 100%; a drag runs one `wpctl` at a time and skips the
  positions it has already moved on from.
- Changes made elsewhere (the volume keys, headphones plugged in) show at
  once. `wpctl` cannot wait for a change, so a `pw-dump --monitor` child
  is kept for as long as the page has been opened and read only for the
  fact that it printed something. It is killed when the window closes,
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
  has hung still gets its picture back. Only killing Settings outright in
  those 15 seconds leaves the new mode in place.
- An output that lists no modes with a refresh rate, as in a virtual
  machine, takes any size and is offered a few common ones.
- The brightness slider never goes below 5%.

Not done: arranging several displays, mirroring, rotation, switching an
output off, saving the layout across sessions (labwc keeps what it is
given only until it exits; a kept mode is not written to a file yet).

## Building and testing

    cargo build -p sengi-settings
    cargo test -p sengi-settings

Debug builds have two aids that release builds are compiled without, so
that the shipped program can only ever show the real machine:

- `sengi-settings --demo NAME` runs over made-up services, for looking at
  states the machine at hand cannot produce. An unknown name lists them.
- `SENGI_SETTINGS_SYS_CLASS=DIR` reads `DIR` in place of `/sys/class`.
  With `DBUS_SYSTEM_BUS_ADDRESS` pointing at a private bus this lets the
  real bus code be run against stand-ins for iwd, BlueZ and logind.

`SENGI_SETTINGS_TRACE=1` prints how long the window took to come up.
