# sengi-quick

The Sengi OS quick settings panel: Wi-Fi, Bluetooth, volume, brightness,
do not disturb, battery, and buttons for the lock screen and the session
menu. It drops down under the right end of the bar.

    sengi-quick

opens the panel, and the same command closes it if it is open. It is not a
service: it starts when asked for and exits when dismissed, by Escape, a
click outside it, or the keyboard going elsewhere.

## What it talks to

| Tile | Through |
| --- | --- |
| Wi-Fi | iwd on the system bus (`net.connman.iwd`), with an agent for passphrases |
| Bluetooth | BlueZ on the system bus (`org.bluez`) |
| Volume | `wpctl` on `@DEFAULT_AUDIO_SINK@` |
| Brightness | `/sys/class/backlight` to read, logind's `Session.SetBrightness` to write |
| Do not disturb | `sengi-notify --dnd on\|off\|status` |
| Battery | `/sys/class/power_supply` |
| Lock, Session | `swaylock -f`, `sengi-power` |

The commands are constants in `src/commands.rs`. No call on the bus ever
starts iwd or BlueZ: on a machine without the hardware they are not
running, and the tile says so. A radio held off by rfkill says that too.

The panel is drawn at once from what `/sys` says and fills in as the
services answer. Each service runs on its own thread behind a trait in
`src/services.rs`; `src/panel.rs` holds every rule about what is shown and
is tested without a bus or a window.

## Wi-Fi

- Networks iwd has joined before come first, then by signal level, then by
  name. The list does not move while the pointer is over it or the
  keyboard is in it.
- A known network joins on a click. If its passphrase has changed since,
  the panel asks for the new one.
- A new network with a passphrase asks for it in the tile.
- A new open network asks first: the row says it is not encrypted and
  offers a Join button.
- The passphrase is never logged, put on a command line or written to disk
  by the panel; iwd stores it. `src/secret.rs` says exactly which copies in
  memory are overwritten and which are only freed. The process cannot be
  core-dumped.
- Names of networks and Bluetooth devices are chosen by strangers. They
  are stripped of control, direction and invisible characters, cut to 32
  characters, and never placed where they could read as the panel's own
  words.

## Known limits

Not done in this release:

- Hidden networks: only networks that announce their name are listed.
- Forgetting a network, or disconnecting from one without switching Wi-Fi
  off. `iwctl known-networks NAME forget` does the first.
- Enterprise (802.1X) networks that iwd has no provisioning file for. They
  are listed, and say they have to be set up first.
- A second Wi-Fi adapter: the first station device is the one shown.
- Bluetooth pairing. Devices paired by other means are shown when
  connected. The place for it is marked in `src/system/bluez.rs` and
  `ui/quick.slint`.
- More than one battery: the first system battery is the one shown.
- Lifting an rfkill block: the tile says a radio is blocked but does not
  unblock it.
- The volume slider stops at 100%; a louder setting made elsewhere is
  shown as its number and brought down to 100% when the slider is touched.

## Building and testing

    cargo build -p sengi-quick
    cargo test -p sengi-quick

Needs `libxkbcommon-dev` at build time, through sengi-layer.

Debug builds have two aids that release builds are compiled without, so
that the shipped panel can only ever show the real machine:

- `sengi-quick --demo NAME` runs the panel over made-up services, for
  looking at states the machine at hand cannot produce: `connected`,
  `networks`, `open-network`, `passphrase`, `wrong-passphrase`, `joining`,
  `changed-passphrase`, `failed`, `bluetooth`, `low-battery`, `charging`,
  `off`, `blocked`, `desktop`, `hostile`. Its Lock and Session buttons are
  real.
- `SENGI_QUICK_SYS_CLASS=DIR` reads `DIR` in place of `/sys/class`. With
  `DBUS_SYSTEM_BUS_ADDRESS` pointing at a private bus this lets the real
  bus code be run against stand-ins for iwd, BlueZ and logind.

`SENGI_QUICK_TRACE=1` prints how long each step of starting up took.
