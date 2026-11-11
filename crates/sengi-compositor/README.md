# sengi-compositor

The Wayland compositor that will replace labwc in Sengi OS. It is built on
[Smithay](https://github.com/Smithay/smithay) 0.7 and is a skeleton: it can
show windows and the bar, but it cannot drive a real screen yet.

It is not part of the ISO. The crate is its own Cargo workspace, excluded from
the main one, so the normal build does not compile Smithay.

## Build and run

    cd crates/sengi-compositor
    cargo build
    cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test

Build needs a C toolchain, `pkg-config`, `libpixman-1-dev` and
`libxkbcommon-dev`. The binary links against libpixman-1 and libxkbcommon. The
winit backend also loads libwayland-client, libwayland-egl, libEGL and a GLES
driver at run time (Mesa's llvmpipe is enough).

    # In a window on your desktop, with a terminal inside it
    sengi-compositor -s foot

    # No display at all: run clients for five seconds, save what was drawn
    sengi-compositor --backend headless --exit-after 5 --screenshot out.png \
        -s waybar -s 'sleep 2; foot'

The socket name is printed on stdout. `-s` commands get `WAYLAND_DISPLAY` set
to it. `--no-default-features` builds without winit and GLES, headless only.

## What works

Checked by running it, in the builder VM, against foot, waybar and swaybg:

- xdg-shell toplevels over `wl_shm`. A new window is centred in the free
  area; later ones cascade by `size.titlebar-height`.
- wlr-layer-shell: a top bar with an exclusive zone (waybar) and a background
  surface (swaybg). Windows are placed below the bar.
- Focus order: a new window takes focus, closing it hands focus back to the
  one used before.
- Headless backend: pixman rendering into memory, PNG screenshot on exit.
- Winit backend: GLES rendering in a window, keyboard input to the focused
  window, click to focus and raise. Tested nested inside headless labwc with
  injected keys and clicks, not yet by a person on a real desktop.
- The background colour comes from `color.desktop` in `tokens/sengi.toml`.

## What does not work

- xdg popups are tracked and configured, and the bottom and overlay layers
  are handled, but none of that has been exercised yet.
- No DRM/KMS, libinput or session handling, so it cannot run on a TTY.
- No window decorations. Clients draw their own; there is no title bar with
  buttons on the left, no rounded corners, no shadows.
- No moving, resizing, maximising, fullscreen or minimising. The requests are
  ignored. No keyboard shortcuts, no Alt+Tab.
- Layer surfaces never get keyboard focus, so a launcher cannot be typed into.
  Popup grabs are ignored, so menus do not close when clicking elsewhere.
- No cursor is drawn by the compositor. Under winit the host cursor is used
  and client cursor images are ignored.
- Keyboard layout is fixed to the xkb default (US). Nested under winit the
  host layout is not followed.
- Winit redraws every frame in full; there is no damage tracking or idle
  there. Windows are not re-placed when the window or usable area changes.
- No dmabuf, so GPU clients fall back to software or fail.
- Missing protocols: xdg-decoration, foreign-toplevel, primary selection,
  xdg-activation, fractional-scale, viewporter, cursor-shape, text-input,
  idle-notify, session-lock, screencopy, output management.
- No XWayland.
- One output only. No HiDPI scale.
- Clients started with `-s` are not stopped when the compositor exits; they
  quit on their own when the socket closes.
- RAM and frame time have not been measured.

## Next steps toward replacing labwc

1. DRM/KMS backend with libseat and libinput (udev hotplug, VT switching),
   GLES on the GPU with pixman as fallback, damage tracking and vblank-driven
   frames. This is what makes the boot test possible.
2. Compositor-drawn cursor and the cursor-shape protocol.
3. Server-side decorations from the tokens (title bar height, 6 px radius,
   Inter, buttons on the left) and xdg-decoration so clients drop theirs.
4. Interactive move and resize, maximise, fullscreen, minimise, and key
   bindings including Alt+Tab over the focus stack.
5. Keyboard focus for layer surfaces and popup grabs, for the launcher and
   menus.
6. foreign-toplevel-list and wlr-foreign-toplevel-management for the dock.
7. What the rest of the session expects: session-lock, idle-notify,
   screencopy (grim, and the CI screenshot), output management, primary
   selection, xdg-activation, dmabuf, fractional scale.
8. Keyboard layout and input settings from configuration.
9. XWayland, started on demand.
10. Hook sengi-focus up to focus changes, then measure idle RAM and time to
    first frame against labwc on the target machine.

## Layout

- `src/layout.rs`: window placement and focus order. Pure, unit-tested.
- `src/cli.rs`: argument parsing, unit-tested.
- `src/state.rs`: state shared by all backends, outputs, focus.
- `src/shell.rs`: wl_compositor and xdg-shell. `src/layer.rs`: layer-shell.
- `src/input.rs`: keyboard and pointer events, hit testing.
- `src/render.rs`: drawing an output and frame callbacks.
- `src/headless.rs`, `src/winit.rs`: the backends.
- `src/png.rs`: minimal PNG writer for screenshots.
