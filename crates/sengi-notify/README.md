# sengi-notify

The Sengi OS notification service. It owns `org.freedesktop.Notifications`
on the session bus (Desktop Notifications Specification 1.2), shows what
arrives as cards under the top bar, and keeps a short history for the
notification centre.

## Running

    sengi-notify                      run the service
    sengi-notify --toggle-centre      open or close the notification centre
    sengi-notify --dismiss-all        dismiss every notification on screen
    sengi-notify --dnd on|off|toggle  set do-not-disturb
    sengi-notify --dnd status         print "on" or "off"

The options talk to the running service. It is meant to be started by
D-Bus activation, so that it only runs once something has a notification
to show, and it needs `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR` in its
environment, as well as the session bus.

Exit statuses of the service:

| Status | Meaning |
| --- | --- |
| 0 | Told to quit |
| 1 | Could not start: no session bus, no compositor, no layer-shell |
| 2 | Bad command line |
| 3 | Another notification service already owns the name |
| 4 | The name was taken away while running |
| 5 | The session bus went away |

The name is requested before the compositor is contacted, so a start that
loses to a running instance costs nothing. The name is never handed over
to another program that asks for it: whoever owns it sees every
notification in the session.

## What clients can rely on

- `GetCapabilities` answers `actions`, `body`, `body-markup`. Body markup
  is accepted and stripped to plain text; nothing is rendered as markup.
- `replaces_id` replaces an open notification for the client that sent
  it. From another client it does so only if the sender has left the bus,
  which is what makes `notify-send --replace-id` work from one run to the
  next; while the sender is still connected, a stranger asking for its id
  gets a new notification under a new id instead.
- `NotificationClosed` and `ActionInvoked` are sent to the client that
  sent the notification, not broadcast, and never before the reply to its
  `Notify` call.
- `expire_timeout` -1 uses the `timing.notification-timeout` token, 0 never
  expires, anything else is taken as given with a floor of one second.
  Critical notifications never expire.
- During do-not-disturb, notifications that are not critical are not
  shown. They stay open, go to the history, and close with reason 1 when
  their timeout runs out, as they would have on screen.
- The `transient` hint keeps a notification out of the history.

## Limits

Everything a client sends is cut to size before it is kept: app name 48
characters, summary 160, body 600, three action buttons with labels of 24.
Control characters, direction overrides and characters that draw nothing
are removed.

| | In all | From one client |
| --- | --- | --- |
| Cards on screen | 4 | 2 |
| Waiting for a place | 32 | 4 |
| Open but held back (do-not-disturb) | 32 | 4 |
| Lines in the centre | 50 | 20 |
| `Notify` calls | 60 at once, then 20 a second | 30 at once, then 10 a second |

A client over its share loses its own oldest notification first. Calls
over the rate are refused with `org.freedesktop.DBus.Error.LimitsExceeded`.
Clients are told apart by their connection to the bus, so a program that
opens a new connection for every notification is held only by the limits
for everyone.

## For other Sengi OS programs: `org.sengios.Notify`

On the object `/org/sengios/Notify`, at the name
`org.freedesktop.Notifications`.

| Member | |
| --- | --- |
| `ToggleCentre()` | Open or close the notification centre |
| `DismissAll()` | Dismiss every notification on screen |
| `SetDoNotDisturb(b)` | Turn do-not-disturb on or off |
| `ToggleDoNotDisturb()` | |
| `DoNotDisturb` (b, read) | Whether do-not-disturb is on |
| `Unread` (u, read) | Lines in the centre that came in since it was last opened |

Both properties announce changes with the usual
`org.freedesktop.DBus.Properties.PropertiesChanged`.

## In the notification centre

Tab, Shift+Tab and the arrow keys move between Clear all, the
do-not-disturb switch and the notifications. Return or Space presses the
button or flips the switch, Delete removes the notification the keyboard
is on, and Escape closes the centre.

## Known limits

- Clicking a notification does not raise the application's window. That
  takes an `ActivationToken` signal backed by xdg-activation, which
  sengi-layer does not speak yet.
- No application icons, and `image-data` is ignored.
- No sounds.
- Do-not-disturb is forgotten when the service restarts.
- One output only: cards and the centre appear on the output the
  compositor picks.
- Removing joiners and variation selectors from text also takes apart
  emoji that are built from several characters.

## Testing

    cargo test -p sengi-notify
    cargo build -p sengi-notify
    crates/sengi-notify/tests/smoke.sh target/debug/sengi-notify

`smoke.sh` starts its own session bus and headless labwc, and needs
`labwc`, `dbus-daemon`, `dbus-run-session`, `dbus-monitor`, `gdbus` and
`notify-send`.

## Building

Needs `libxkbcommon-dev` at build time and `libxkbcommon0` at run time, as
everything built on sengi-layer does.
