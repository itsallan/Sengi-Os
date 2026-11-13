# sengi-system

What the Sengi OS shell needs from the system around it: Wi-Fi, Bluetooth,
brightness, audio, battery, link state, and the bus connections under them.

Used by sengi-quick (the services), by sengi-settings (the services and,
with the `settings` feature, what only it manages), by sengi-notify and
sengi-bar for the bus connections, by sengi-launcher for the
single-instance socket, and by sengi-bar for reading the volume.

## Features

Nothing is on by default. Without features the crate is plain std:

- `instance`: one open panel per program. A socket under
  `$XDG_RUNTIME_DIR`, with a lock file around asking, unlinking and
  binding, so two invocations starting together cannot both open, and a
  process only ever removes the socket it created. `claim` is for a
  panel: the newcomer closes the one that is open. `deliver` is for a
  window that stays: the newcomer hands it a short message, which the
  window checks with a function of the program's own before taking it,
  and leaves.
- `volume` and `commands`: parsing `wpctl get-volume`, the arguments for
  setting it, and the names of the programs involved.

The features add:

- `bus`: `connect`, the D-Bus connections opened by hand. zbus's own connect
  starts a helper thread that wakes twice a second for as long as the
  process lives; a socket that is already connected avoids it.
- `services` (implies `bus`): the services, the traits a program reaches
  them through, and the worker threads that run them.
- `settings` (implies `services`): what a settings application manages
  and the panel does not. Leaving and forgetting networks (`WifiService`'s
  `disconnect` and `forget`, which without the feature refuse); the
  cabled interfaces and every interface's addresses (`links`); the
  Bluetooth devices one by one, searching, pairing with an agent,
  connecting and removing (`pairing`, the `DeviceService` trait); the
  audio outputs and inputs one by one and a watch for their changes
  (`audio`, the `AudioDevices` trait). The panel is built without it and
  carries none of that code.

## Services

`services` defines one trait per service (`WifiService`, `BluetoothService`,
`AudioService`, `Machine`, `DndService`), gathered in `Services`. A program
gets the real ones from `real_services()` or supplies fakes of its own, then
hands them to `Workers::start`, which runs each on a thread and reports
through `Event`s; `Command`s go the other way.

| Service | Module | Backed by |
|---|---|---|
| Wi-Fi | `iwd`, `wifi` | iwd on the system bus, with a passphrase agent |
| Bluetooth | `bluez`, `bluetooth` | BlueZ on the system bus |
| Brightness | `machine`, `backlight` | `/sys/class/backlight`, set through logind |
| Battery, links, rfkill | `machine`, `battery` | `/sys/class`; `battery::packs` is each battery in full, and several are read as one |
| Volume | `programs`, `volume` | `wpctl` on `@DEFAULT_AUDIO_SINK@` |
| Audio devices | `audio` | `wpctl status`, `wpctl` on a node, `pw-dump --monitor` |
| Bluetooth devices | `pairing` | BlueZ on the system bus, with a pairing agent |
| Cabled links, addresses | `links` | `/sys/class/net`, `getifaddrs` |
| Do not disturb | `programs` | `sengi-notify --dnd` |

What it is careful about:

- No call starts iwd or BlueZ. A machine without the hardware stays without
  the daemons, and a service that is not running reads as absent. The one
  call made without the no-auto-start flag is `Bus::timedate_properties`,
  to timedated, which exists to be started by being asked; the name is
  fixed inside it.
- A bus policy that forbids a call (`AccessDenied`) is told apart from a
  service that is not there.
- The passphrase agent answers only the iwd it registered with, checked by
  the caller's unique bus name, and each join has its own ticket so an old
  request cannot be answered with a new passphrase. `Cancel` and
  `Release` are taken from that iwd only.
- The pairing agent is held to the same: each pairing records the device
  it is with and the owner of `org.bluez` at that moment. A request from
  anyone else, about any other device, or with no pairing under way is
  rejected without being shown, the ones that only display a number
  included, and none is ever agreed to without the user's answer.
  `AuthorizeService` is always rejected.
- A passphrase is a `secret::Secret`, which is wiped when dropped and never
  printed. `secret::forbid_core_dumps()` keeps the process out of core
  dumps; a program that handles passphrases calls it first thing in `main`.
- Volume writes are coalesced: a drag on a slider becomes a few `wpctl`
  runs, the last value always among them.
- A command (`wpctl`, `sengi-notify`) is found on `PATH` and started by
  its full path, has five seconds, and is read up to 256 KiB. Past
  either it is killed and reaped.
- A watch on the bus (`Bus::watch`) is one sleeping thread and can be
  stopped; `unwatch()` on a service ends its watches.
- The audio watch reads `pw-dump --monitor` for nodes, devices and
  metadata only. A reader's own `wpctl` shows there as a client coming
  and going, which must not cause another read.
- Names from outside (SSIDs, device names) go through sengi-text and are
  cut to 32 characters, or 64 for a device in a list with a row to itself.

Every setter the panel calls returns nothing and reports on standard
error. Each has a `try_` twin that returns a `Refusal` (`Denied` by the
bus policy, or `Failed`) for a program that can show it.

A program names itself for the crate's messages on standard error with
`set_program_name("sengi-quick")`.

`Event`, `Command` and `JoinError` are `#[non_exhaustive]`: match them with
a wildcard arm, so a new kind of news does not break the program reading it.

In debug builds only, `SENGI_SYS_CLASS=DIR` reads `DIR` in place of
`/sys/class`, for trying the battery and backlight code on a machine that
has neither. A release build ignores it. `sys_class()` is where to read,
for a program that reads more of `/sys/class` than this crate does.
