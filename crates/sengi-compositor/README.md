# sengi-compositor

The Wayland compositor that will replace labwc in Sengi OS. It is built on
[Smithay](https://github.com/Smithay/smithay) 0.7. The programs the live
session starts (sengi-bar, the waybar dock, fuzzel, mako, swaybg, swayidle,
swaylock, sengi-focus) run on it. It still cannot drive a real screen.

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

The socket name is printed on stdout. `-s` commands get `WAYLAND_DISPLAY` set
to it. `--no-default-features` builds without winit and GLES, headless only.

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
  title bar cannot be pushed under the bar.
- Close, minimise and maximise buttons. Maximise again restores the old size
  and place; a minimised window comes back when the dock activates it.
- Key bindings as labwc has them in the image: Super+Space runs fuzzel,
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
  terminal behind it, and bindings did not fire.
- Headless backend: pixman rendering into memory, scripted input, PNG
  screenshots.
- Winit backend: GLES rendering in a window with keyboard and pointer input.
  Tested nested inside headless labwc with injected keys and clicks (frames
  and our own cursor draw, the host cursor is hidden, the maximise button
  works), not yet by a person on a real desktop.

## What does not work

- No DRM/KMS, libinput or session handling, so it cannot run on a TTY.
- It redraws at a fixed 60 Hz whether or not anything changed, and sends
  frame callbacks to every surface on every tick. This must be replaced by
  damage-driven scheduling before a DRM backend.
- If the locker crashes the session stays locked and blank until another
  locker takes over, as the protocol requires; nothing restarts one.
- The lock is confirmed to the client when it is requested, one frame
  before the first locked frame is drawn.
- The bottom corners of a window are square and there are no shadows.
- Fullscreen requests are ignored. `wm_capabilities` is not sent.
- Clients that do not speak xdg-decoration (GTK) keep their own title bars.
- Popups are placed where the client asks, without keeping them on screen.
- A maximise request before a window's first map is dropped. A decoration
  mode change after mapping does not re-place the window. Maximise ignores
  the client's maximum size.
- Among overlapping layer surfaces of one layer only the top one is hit
  tested.
- Written but not exercised: moves and resizes started by the client
  (`xdg_toplevel.move`/`.resize`, now checked against the press serial),
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

1. Render only when something changed, with proper frame callback
   throttling; then the DRM/KMS backend with libseat and libinput (udev
   hotplug, VT switching), GLES on the GPU with pixman as fallback.
2. Port move and resize from the compositor's own drag state to Smithay
   pointer grabs.
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
- `src/input.rs`: keyboard and pointer events, drags, hit testing.
- `src/render.rs`: drawing an output and frame callbacks.
- `src/headless.rs`, `src/winit.rs`: the backends.
- `src/png.rs`: minimal PNG writer for screenshots.
