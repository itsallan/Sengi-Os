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

## Building and testing

    cargo build -p sengi-settings
    cargo test -p sengi-settings

`SENGI_SETTINGS_TRACE=1` prints how long the window took to come up.
