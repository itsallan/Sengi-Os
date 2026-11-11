# sengi-compositor

The Wayland compositor that will replace labwc in Sengi OS. It is built on
[Smithay](https://github.com/Smithay/smithay) 0.7. The programs the live
session starts (sengi-bar, sengi-dock, sengi-launcher, mako, swaybg,
swayidle, swaylock, sengi-focus) run on it, it sleeps when nothing changes,
and it drives a screen through DRM/KMS with software rendering. It does not
use the GPU yet.

It is not part of the ISO. The crate is its own Cargo workspace, excluded from
the main one, so the normal build does not compile Smithay.

## Build and run

    cd crates/sengi-compositor
    cargo build
    cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test

Build needs a C toolchain, `pkg-config`, `libpixman-1-dev` and
`libxkbcommon-dev`. The binary links against libpixman-1 and libxkbcommon. The
winit backend also loads libwayland-client, libwayland-egl, libEGL and a GLES
driver at run time (Mesa's llvmpipe is enough). Title bar text needs the
typeface named by `type.family` (Inter, Debian package `fonts-inter`) under
`/usr/share/fonts`, `/usr/local/share/fonts` or the user's fonts directory;
without it title bars are drawn with no title. The cursor comes from the
XCursor theme named by `XCURSOR_THEME` (default `default`) at `XCURSOR_SIZE`
(default 24); without a theme an arrow is drawn.

    # In a window on your desktop, with a terminal inside it
    sengi-compositor -s foot

    # No display at all: run clients for five seconds, save what was drawn
    sengi-compositor --backend headless --exit-after 5 --screenshot out.png \
        -s waybar -s 'sleep 2; foot'

    # On a virtual terminal, as the session (what greetd would run)
    sengi-compositor -s 'sh /etc/xdg/labwc/autostart'

The socket name is printed on stdout. `-s` commands get `WAYLAND_DISPLAY` set
to it. `--no-default-features` builds without winit and GLES, headless only.

### As the session on a real screen

With the `drm` feature (on by default) and started on a virtual terminal
with no `WAYLAND_DISPLAY` or `DISPLAY` set, the compositor takes the seat
through libseat (logind or seatd), opens the seat's primary display device,
uses the first connected connector at its preferred mode, and reads
keyboards, mice and touchpads through libinput.

Build for the image, without the development window backend:

    cargo build --release --locked --no-default-features --features drm

Build-time packages (Debian): `build-essential pkg-config libseat-dev
libinput-dev libudev-dev libgbm-dev libpixman-1-dev libxkbcommon-dev`.
Run-time packages: `libseat1 libinput10 libudev1 libgbm1 libdrm2
libpixman-1-0 libxkbcommon0`, `fonts-inter` for title text, a cursor theme
such as `adwaita-icon-theme`, and a seat provider: systemd-logind (a greetd
session with pam_systemd is enough) or seatd.

Under greetd, point the session command at a script like the image's
`sengi-session` with the last line replaced:

    exec sengi-compositor \
        -s 'LABWC_PID=$SENGI_COMPOSITOR_PID sh /etc/xdg/labwc/autostart'

Started programs get `WAYLAND_DISPLAY` and `SENGI_COMPOSITOR_PID`. The
start-up script's restart loops watch `LABWC_PID`, hence the assignment
until the script learns the new name. `sengi-compositor --exit` from inside
the session, or SIGTERM, logs out and gives the terminal back.
Ctrl+Alt+F1..F12 switch virtual terminals. The keyboard layout follows
`XKB_DEFAULT_LAYOUT` and the other `XKB_DEFAULT_*` variables.

`--backend drm --exit-after SEC --screenshot PNG` saves what was drawn by
rendering the same frame once more into memory.

### Scripted input

The headless backend has no devices, so `--script FILE` plays input from a
file: one step per line, the time in milliseconds since start, then a command.

    # drag a title bar, type a command, close the window
    2000 move 640 110
    2100 press left
    2200 move 490 50
    2300 release left
    2500 type echo Hello
    2600 key enter
    3000 screenshot /tmp/after.png
    3100 key super+q
    3500 exit

`move` takes output pixels. `press`, `release` and `click` take `left`,
`right` or `middle`. `type` and `key` assume the US layout; `key` takes a
key name or a chord. Lines starting with `#` are comments. Steps go through
the same code as real input. Leave time between a step and the screenshot
that should show its effect; clients answer asynchronously.

## What works

Checked by running it in the builder VM, mostly headless with scripted input
and screenshots, against foot, sengi-bar, waybar, swaybg, fuzzel, mako,
swaylock, swayidle, sengi-focus, wlrctl and gtk3-widget-factory:

- xdg-shell toplevels over `wl_shm`. A new window is centred in the free
  area; later ones cascade by the title bar height.
- Server-side decorations through xdg-decoration, for clients that accept
  them (foot does): title bar with close, minimise and maximise on the left,
  centred title in Inter, rounded top corners, 1 px border, separate looks
  for the focused and unfocused window, hover highlight on buttons. All
  sizes and colours are read from `tokens/sengi.toml`.
- Drag the title bar to move. Drag an edge or corner to resize; the opposite
  edges stay put, a window never gets narrower than its buttons, and the
  title bar cannot be pushed under the bar. Drags are Smithay pointer
  grabs, like popup grabs and client clicks.
- Close, minimise and maximise buttons. Maximise again restores the old size
  and place; a minimised window comes back when the dock activates it. A
  client that asks to be maximised before it maps (`foot --maximized`)
  starts maximised.
- The image's own start-up script (`autostart.in`, with `LABWC_PID` set to
  the compositor's pid) brings up swaybg, sengi-bar and sengi-dock; the
  dock's Terminal item starts foot and shows it as running, its search item
  and Super+Space open sengi-launcher, which takes the keyboard and closes
  on a click outside.
- It draws only when something changed. With swaybg, sengi-bar, a dock,
  mako and foot running and no input, the compositor made 0 wakeups in ten
  seconds (566 before); with a blinking terminal cursor, about two per
  blink. Frame callbacks go to visible surfaces, hidden ones get one a
  second. Measured on the headless backend, debug build.
- Key bindings as labwc has them in the image: Super+Space runs
  sengi-launcher (fuzzel where that is missing),
  Super+Return a terminal (`sengi-run foot`, or `foot`), Super+L
  `swaylock -f`, Super+Q closes the window that has the keyboard, Alt+Tab
  and Alt+Shift+Tab step through windows by recent use.
- wlr-layer-shell on all the layers the session uses: sengi-bar and the
  waybar dock with exclusive zones, swaybg behind, mako on the overlay
  layer. Windows are kept out from under bars, also ones that appear later,
  and maximised windows fill exactly the area left free.
- Keyboard focus for layer surfaces: fuzzel is typed into while it is open
  and the window gets the keyboard back afterwards.
- wlr-foreign-toplevel-management: title, app id, activated, maximised and
  minimised state; activate, close, maximise and minimise requests. The
  waybar taskbar lists windows and restores a minimised one on click;
  `wlrctl toplevel` can list, focus and minimise; sengi-focus connects.
- A drawn pointer cursor: theme cursors, resize cursors on frame edges,
  cursors set by clients (surface or cursor-shape).
- xdg popups, also on layer surfaces (waybar tooltips), with grabs: a GTK
  menu opens and a click elsewhere dismisses it.
- ext-session-lock and ext-idle-notify: swaylock locks, swayidle locks
  after its timeout. While locked only the lock surface is drawn and gets
  input; keys typed and clicks made on the lock screen did not reach the
  terminal behind it, and bindings did not fire. The locker is told the
  session is locked after the first locked frame is out. If the locker
  dies, the screen stays locked and plain; a new locker can take over, a
  second one is refused while the first lives.
- DRM backend, checked on virtio-gpu in a nested QEMU guest booted from the
  project's ISO, with the compositor in labwc's place in the live greetd
  session: the image's start-up script brought up swaybg, sengi-bar,
  sengi-dock, mako, swayidle and sengi-focus; Super+Return opened foot with
  our title bar and typing reached it; Super+Space opened the launcher; the
  pointer followed an absolute pointing device. QEMU's own screen dumps
  show all of it, and a click on the title bar's close button closed the
  window. 24 MB resident, no LLVM mapped, and no wakeups or CPU time in
  ten idle seconds. SIGTERM ended the session and gave the console back.
  Run as root on libseat's builtin backend, Ctrl+Alt+F2 switched to the
  text login on tty2 and Ctrl+Alt+F7 came back to a repainted desktop,
  and `--screenshot` saved the frame.
- `sengi-compositor --exit` and SIGTERM end the session; started programs
  get `SENGI_COMPOSITOR_PID`.
- Only a locker the compositor started may take over the session of a
  locker that died, and the compositor starts one itself; a foreign locker
  was refused. A forged `xdg_toplevel.move` is ignored and a toplevel that
  detaches its buffer unmaps cleanly (`examples/misbehave.rs`).
- Headless backend: pixman rendering into memory, scripted input, PNG
  screenshots.
- Winit backend: GLES rendering in a window with keyboard and pointer input.
  Tested nested inside headless labwc with injected keys and clicks (frames
  and our own cursor draw, the host cursor is hidden, the maximise button
  works), not yet by a person on a real desktop.

## What does not work

- No GPU rendering: the DRM backend composes with pixman on the CPU into
  dumb buffers. No GLES/GBM path, no dmabuf import, no hardware cursor
  plane (the cursor is composed like everything else), no direct scanout.
  This is the right path for VMs and has not been tried on real hardware,
  where full-screen updates at 1080p will cost CPU.
- DRM: one output only, chosen at start. Connectors appearing or
  disappearing later are not handled, a second display stays dark, and
  there is no way to pick a mode. Relative pointer motion and the touchpad
  settings are implemented but have not been seen working: the test
  guest's only pointer is an absolute one.
- In the 0.3.0 live image, Ctrl+Alt+Fn is refused by logind ("failed to
  change vt") for the session user; it works when the compositor runs on
  libseat's builtin backend. The image probably has the same limit under
  labwc; polkit or seatd would lift it.
- Any client may lock an unlocked session and later unlock it; only the
  takeover of a dead locker's session is restricted.
- The bottom corners of a window are square and there are no shadows.
- Fullscreen requests are ignored (and `wm_capabilities` says so).
- The bar's power icon starts `sengi-power`, which was not available to
  test; the click reaches the bar.
- Clients that do not speak xdg-decoration (GTK) keep their own title bars.
- Windows are not re-placed when the usable area changes; they are only
  pulled out from under a bar.
- Written but not exercised: moves and resizes started by the client
  (`xdg_toplevel.move`/`.resize` with a valid serial),
  popup constraining at a screen edge, a decoration mode change on a mapped
  window, frame callback throttling for a fully covered window,
  unmapping a window by detaching its buffer, on-demand layer focus by
  click, client maximum sizes, scroll events, winit window resize, animated
  cursors.
- No key binding configuration, no double-click on the title bar, no edge
  snapping. Alt+Tab has no on-screen switcher.
- Maximising moves the window at once but the client redraws a moment later.
- Title text is unkerned (Inter keeps kerning in GPOS), has no fallback
  fonts, and long titles are cut off without an ellipsis.
- Keyboard layout is fixed to the xkb default (US). Nested under winit the
  host layout is not followed.
- No dmabuf, so GPU clients fall back to software or fail.
- Missing protocols: primary selection, xdg-activation, fractional-scale,
  viewporter, text-input, screencopy, output management, idle-inhibit,
  virtual keyboard and pointer, ext-foreign-toplevel-list.
- No XWayland.
- One output only. No HiDPI scale: frames and cursors are drawn for scale 1.
- Clients started with `-s` or a key binding are not stopped when the
  compositor exits; they quit on their own when the socket closes.
- RAM and frame time have not been measured.

## Next steps toward replacing labwc

1. GLES on GBM for machines with a GPU, chosen automatically and never
   falling into llvmpipe, with dmabuf import and a hardware cursor plane.
2. Connector hotplug, a second output, mode selection, and proof on real
   hardware of VT switching, touchpads and suspend.
3. Restrict foreign-toplevel and locking an unlocked session to the
   clients meant to use them.
3. screencopy (grim, and the CI screenshot), output management, primary
   selection, xdg-activation, dmabuf, fractional scale, idle-inhibit.
4. Fullscreen, popup constraining, double-click to maximise, edge snapping,
   an Alt+Tab switcher.
5. Rounded bottom corners and shadows.
6. Keyboard layout, key bindings and input settings from configuration.
7. XWayland, started on demand.
8. Run the real session start-up script against it, then measure idle RAM
   and time to first frame against labwc on the target machine.

## Layout

- `src/layout.rs`: window placement and focus order. Pure, unit-tested.
- `src/frame.rs`: frame geometry: buttons, hit testing, resize and clamping
  arithmetic. Pure, unit-tested.
- `src/decoration.rs`: theme from the tokens and the title bar painter. Pure,
  unit-tested.
- `src/text.rs`: finding the typeface and rasterising a line of text.
- `src/keys.rs`: the key binding table. Pure, unit-tested.
- `src/cli.rs`, `src/script.rs`: arguments and input scripts, unit-tested.
- `src/state.rs`: state shared by all backends, outputs, focus.
- `src/window.rs`: per-window state, frame textures, move, resize, maximise.
- `src/shell.rs`: wl_compositor, xdg-shell and xdg-decoration.
- `src/layer.rs`: layer-shell.
- `src/foreign_toplevel.rs`: wlr-foreign-toplevel-management.
- `src/lock.rs`: session lock.
- `src/cursor.rs`: cursor theme loading and the fallback arrow.
- `src/input.rs`: keyboard and pointer events, the drag grab, hit testing.
- `src/device_input.rs`: backend device events, built only with a backend
  that has devices.
- `src/render.rs`: drawing an output and frame callbacks.
- `src/headless.rs`, `src/winit.rs`, `src/drm.rs`: the backends.
- `src/png.rs`: minimal PNG writer for screenshots.
