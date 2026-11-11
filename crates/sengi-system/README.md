# sengi-system

What the Sengi OS shell needs from the system around it: Wi-Fi, Bluetooth,
brightness, audio, battery, link state, and the bus connections under them.

Used by sengi-quick (everything), by sengi-notify and sengi-bar for the bus
connections, by sengi-launcher for the single-instance socket, and by
sengi-bar for reading the volume. A settings application can build on the
same traits.

## Features

Nothing is on by default. Without features the crate is plain std:

- `instance`: one open panel per program. A socket under
  `$XDG_RUNTIME_DIR`, with a lock file around asking, unlinking and
  binding, so two invocations starting together cannot both open, and a
  process only ever removes the socket it created.
- `volume` and `commands`: parsing `wpctl get-volume`, the arguments for
  setting it, and the names of the programs involved.

The features add:

- `bus`: `connect`, the D-Bus connections opened by hand. zbus's own connect
  starts a helper thread that wakes twice a second for as long as the
  process lives; a socket that is already connected avoids it.
- `services` (implies `bus`): the services, the traits a program reaches
  them through, and the worker threads that run them.

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
| Battery, links, rfkill | `machine`, `battery` | `/sys/class` |
| Volume | `programs`, `volume` | `wpctl` on `@DEFAULT_AUDIO_SINK@` |
| Do not disturb | `programs` | `sengi-notify --dnd` |

What it is careful about:

- No call starts iwd or BlueZ. A machine without the hardware stays without
  the daemons, and a service that is not running reads as absent.
- A bus policy that forbids a call (`AccessDenied`) is told apart from a
  service that is not there.
- The passphrase agent answers only the iwd it registered with, checked by
  the caller's unique bus name, and each join has its own ticket so an old
  request cannot be answered with a new passphrase.
- A passphrase is a `secret::Secret`, which is wiped when dropped and never
  printed. `secret::forbid_core_dumps()` keeps the process out of core
  dumps; a program that handles passphrases calls it first thing in `main`.
- Volume writes are coalesced: a drag on a slider becomes a few `wpctl`
  runs, the last value always among them.
- Names from outside (SSIDs, device names) go through sengi-text and are
  cut to 32 characters.

A program names itself for the crate's messages on standard error with
`set_program_name("sengi-quick")`.
