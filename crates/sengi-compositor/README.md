# sengi-compositor

The Wayland compositor that will replace labwc in Sengi OS. It is built on
[Smithay](https://github.com/Smithay/smithay) 0.7. Windows can be used:
they have frames, and can be moved, resized, maximised and closed. It still
cannot drive a real screen.

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
without it title bars are drawn with no title.

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

Checked by running it in the builder VM against foot, waybar, swaybg and
fuzzel, mostly headless with scripted input and screenshots:

- xdg-shell toplevels over `wl_shm`. A new window is centred in the free
  area; later ones cascade by the title bar height.
- Server-side decorations through xdg-decoration, for clients that accept
  them (foot does): title bar with close, minimise and maximise on the left,
  centred title in Inter, rounded top corners, 1 px border, separate looks
  for the focused and unfocused window, hover highlight on buttons. All
  sizes and colours are read from `tokens/sengi.toml`.
- Drag the title bar to move. Drag an edge or corner to resize; the opposite
  edges stay put and a window never gets narrower than its buttons.
- Close and maximise buttons; maximise again restores the old size and
  place. Super+Q closes the focused window.
- wlr-layer-shell: a top bar with an exclusive zone (waybar) and a background
  surface (swaybg). Windows cannot be placed or dragged under the bar, are
  pushed out from under a bar that appears after them, and maximised windows
  fill exactly the area the bar leaves free.
- Keyboard focus for layer surfaces: a launcher asking for exclusive focus
  (fuzzel) is typed into while it is open, and the window gets the keyboard
  back when it closes.
- Focus order: a new window takes focus, clicking a window focuses and raises
  it, closing it hands focus back to the one used before.
- Headless backend: pixman rendering into memory, scripted input, PNG
  screenshots.
- Winit backend: GLES rendering in a window with keyboard and pointer input.
  Tested nested inside headless labwc with injected keys and clicks (frames
  draw, the maximise button works), not yet by a person on a real desktop.

## What does not work

- No DRM/KMS, libinput or session handling, so it cannot run on a TTY.
- The bottom corners of a window are square: rounding them means clipping
  the client's own pixels, which needs a custom render element. There are no
  shadows either.
- The minimise button does nothing. Nothing could bring the window back until
  the dock can list windows. Fullscreen requests are ignored.
- Clients that do not speak xdg-decoration (GTK) keep their own title bars,
  with buttons wherever the toolkit puts them.
- Moves and resizes started by such clients (`xdg_toplevel.move` and
  `.resize`), on-demand layer focus by click, xdg popups, the bottom and
  overlay layers, client maximum sizes, scroll events and winit window resize
  are implemented but have not been exercised.
- No cursor is drawn by the compositor and there are no resize cursors. Under
  winit the host cursor is used and client cursor images are ignored.
- No keyboard shortcuts other than Super+Q. No Alt+Tab, no double-click on
  the title bar, no snapping to screen edges.
- Popup grabs are ignored, so menus do not close when clicking elsewhere.
- Maximising moves the window at once but the client redraws a moment later,
  so one or two frames show the old size at the new place.
- Kerning in titles is limited to the font's legacy `kern` table; Inter keeps
  its kerning in GPOS, so title text is unkerned. No fallback fonts: glyphs
  Inter lacks show as its missing-glyph box. Long titles are cut off without
  an ellipsis.
- Keyboard layout is fixed to the xkb default (US). Nested under winit the
  host layout is not followed.
- Winit redraws every frame in full; there is no damage tracking or idle
  there.
- No dmabuf, so GPU clients fall back to software or fail.
- Missing protocols: foreign-toplevel, primary selection, xdg-activation,
  fractional-scale, viewporter, cursor-shape, text-input, idle-notify,
  session-lock, screencopy, output management, virtual keyboard and pointer.
- No XWayland.
- One output only. No HiDPI scale: frames are drawn for scale 1.
- Clients started with `-s` are not stopped when the compositor exits; they
  quit on their own when the socket closes.
- RAM and frame time have not been measured.

## Next steps toward replacing labwc

1. DRM/KMS backend with libseat and libinput (udev hotplug, VT switching),
   GLES on the GPU with pixman as fallback, damage tracking and vblank-driven
   frames. This is what makes the boot test possible.
2. Compositor-drawn cursor, resize cursors and the cursor-shape protocol.
3. foreign-toplevel-list and wlr-foreign-toplevel-management for the dock,
   and with them a working minimise.
4. Key bindings from configuration, Alt+Tab over the focus stack, fullscreen,
   double-click to maximise, edge snapping.
5. Popup grabs for menus; exercise popups and client-started moves with a GTK
   application.
6. Rounded bottom corners and shadows.
7. What the rest of the session expects: session-lock, idle-notify,
   screencopy (grim, and the CI screenshot), output management, primary
   selection, xdg-activation, dmabuf, fractional scale.
8. Keyboard layout and input settings from configuration.
9. XWayland, started on demand.
10. Hook sengi-focus up to focus changes, then measure idle RAM and time to
    first frame against labwc on the target machine.

## Layout

- `src/layout.rs`: window placement and focus order. Pure, unit-tested.
- `src/frame.rs`: frame geometry: buttons, hit testing, resize and clamping
  arithmetic. Pure, unit-tested.
- `src/decoration.rs`: theme from the tokens and the title bar painter. Pure,
  unit-tested.
- `src/text.rs`: finding the typeface and rasterising a line of text.
- `src/cli.rs`, `src/script.rs`: arguments and input scripts, unit-tested.
- `src/state.rs`: state shared by all backends, outputs, focus.
- `src/window.rs`: per-window state, frame textures, move, resize, maximise.
- `src/shell.rs`: wl_compositor, xdg-shell and xdg-decoration.
- `src/layer.rs`: layer-shell.
- `src/input.rs`: keyboard and pointer events, drags, hit testing.
- `src/render.rs`: drawing an output and frame callbacks.
- `src/headless.rs`, `src/winit.rs`: the backends.
- `src/png.rs`: minimal PNG writer for screenshots.
