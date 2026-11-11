# sengi-compositor

The Wayland compositor that will replace labwc in Sengi OS. It is built on
[Smithay](https://github.com/Smithay/smithay) 0.7. The programs the live
session starts (sengi-bar, sengi-dock, sengi-launcher, mako, swaybg,
swayidle, swaylock, sengi-focus) run on it, it sleeps when nothing changes,
and it drives screens through DRM/KMS: with GLES where there is a GPU, with
pixman on the CPU where there is none. It has not run on real hardware yet;
everything below was seen in virtual machines.

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
lights every connected connector at its preferred mode, side by side in the
order found, and reads keyboards, mice and touchpads through libinput.

The renderer is chosen at start and named in the log. Kernel drivers that
never have a GPU get pixman on dumb buffers. Otherwise a child process
(`--probe-gl`) asks Mesa what GL would run on, so Mesa is only loaded into
the compositor when the answer is hardware; llvmpipe and the like are
refused. `--renderer gles|pixman` or `SENGI_COMPOSITOR_RENDERER` override
the choice. With GLES, clients can hand over dmabufs (linux-dmabuf with
feedback) and the cursor goes on its own plane where KMS offers one.

Build for the image, without the development window backend:

    cargo build --release --locked --no-default-features --features drm

That also leaves out the `test-hooks` feature, and with it `--script` and
the virtual cursor plane switch, which are for testing only.

Build-time packages (Debian): `build-essential pkg-config libseat-dev
libinput-dev libudev-dev libgbm-dev libpixman-1-dev libxkbcommon-dev`.
Run-time packages: `libseat1 libinput10 libudev1 libgbm1 libdrm2
libpixman-1-0 libxkbcommon0`, `fonts-inter` for title text, a cursor theme
such as `adwaita-icon-theme`, and a seat provider: systemd-logind (a greetd
session with pam_systemd is enough) or seatd.

Under greetd, point the session command at a script like the image's
`sengi-session` with the last line replaced:

    exec sengi-compositor --backend drm \
        -s 'LABWC_PID=$SENGI_COMPOSITOR_PID sh /etc/xdg/labwc/autostart'

Name the backend there. Left to choose, the compositor opens a window
when `WAYLAND_DISPLAY` or `DISPLAY` name a display that is really there,
takes the screen when started on a terminal, and otherwise runs headless.

Started programs get `WAYLAND_DISPLAY` and `SENGI_COMPOSITOR_PID`. The
start-up script's restart loops watch `LABWC_PID`, hence the assignment
until the script learns the new name. `sengi-compositor --exit` from inside
the session, or SIGTERM, logs out and gives the terminal back.
Ctrl+Alt+F1..F12 switch virtual terminals. The keyboard layout follows
`XKB_DEFAULT_LAYOUT` and the other `XKB_DEFAULT_*` variables.

`--backend drm --screenshot PNG` saves what is drawn, by rendering the same
frame once more into memory, whenever the compositor gets SIGUSR2, and at
the end with `--exit-after SEC`. Several screens come out as one picture,
laid out as they are on the desktop. SIGUSR1 prints how many page flips
there were, how many of them had the cursor on its own plane, how many
dmabufs were imported and where the pointer is. Smithay's warnings and
errors go to stderr; `SENGI_COMPOSITOR_LOG=info|debug|trace` shows more.

### A test guest with a GPU

The GPU path was checked in a KVM guest with a virgl GPU, booted from the
released ISO, with the compositor and the test script on a second disk
that the image's `sengi.test.debug` hook mounts and runs:

    qemu-system-x86_64 -machine q35,accel=kvm -cpu host -smp 2 -m 4096 \
        -drive file=sengi-os-0.3.0-amd64.iso,format=raw,if=virtio,readonly=on \
        -drive file=share.img,format=raw,if=virtio \
        -device virtio-vga-gl,id=gpu0,max_outputs=2 \
        -display egl-headless,rendernode=/dev/dri/renderD128 \
        -vnc unix:vnc.sock -vnc unix:vnc2.sock,id=v2,display=gpu0,head=1 \
        -machine vmport=off -device virtio-tablet-pci -device virtio-mouse-pci \
        -nic none -serial file:serial.log -qmp pipe:qmp -monitor stdio \
        -smbios type=11,value=io.systemd.credential:sengi.test=1 \
        -smbios 'type=11,value=io.systemd.credential:sengi.test.debug=mount /dev/vdb /mnt; sh /mnt/guest.sh'

Notes for whoever repeats it. The credential text must not contain a
comma. QEMU's `screendump` has no picture of a GL scanout ("no surface");
a VNC client gets one, and it shows the primary plane only, which is how a
cursor on its own plane can be told from a composed one. The second head
is "plugged in" by a VNC client on `vnc2.sock` that announces the extended
desktop size encoding and sends SetDesktopSize; nothing found unplugs it
again. Keys go in with the monitor's `sendkey`, absolute and relative
pointer events with QMP `input-send-event`. On virtual GPUs the kernel
hides the cursor plane from compositors that do not set the cursor hotspot
property, which Smithay does not; `SENGI_COMPOSITOR_VIRTUAL_CURSOR_PLANE=1`
claims that support so the cursor plane can be exercised there (a host
that draws the guest's cursor may then show it a few pixels off).
virtio-gpu completes page flips at once, so a client that draws
continuously runs faster than the 75 Hz the mode claims.

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
  after its timeout. While locked only the lock surfaces are drawn and get
  input, one per screen; keys typed and clicks made on the lock screen did
  not reach the terminal behind it, and bindings did not fire. The locker
  is told the session is locked once every screen has shown a frame drawn
  under the lock. If the locker dies, the screen stays locked and plain
  and the compositor starts the locker again; a second locker is refused
  while the first lives. Only the lock object that was granted the lock
  can unlock or supply lock surfaces. `tests/lock.rs` checks these rules
  against the running compositor, and on two screens in the GPU guest
  swaylock covered both while `misbehave steal-unlock` and
  `fake-lock-surface` got nowhere.
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
- GLES on GBM, checked in a KVM guest with a virgl GPU (host: Intel HD 620)
  running the image's session. The compositor's own screenshots of the
  desktop, a terminal, a window mid-drag, a resize cursor and the launcher
  are pixel for pixel what QEMU's VNC server shows of the scanout, and
  differ from the same scenes on pixman only in the clock, a text caret
  and about fifty pixels by one or two levels. Idle, ten seconds: no page
  flips, no CPU time.
- linux-dmabuf on GLES: `weston-simple-egl` attached dmabufs
  (`zwp_linux_buffer_params_v1.create_immed`, ARGB8888, no modifier) and
  no shm buffers, and was drawn. On pixman the global is not offered and
  the same client falls back to software.
- Cursor plane, with the virtual cursor switch described above: the log
  counts one cursor plane, every frame with the pointer in it had the
  cursor on that plane, DRM's debugfs state shows the plane following the
  pointer, and the primary plane's picture lacks the cursor, also for the
  resize cursor and the launcher's.
- Terminal switching and exit on GLES, as root on libseat's builtin
  backend: Ctrl+Alt+F2 reached tty2, Ctrl+Alt+F7 came back to a picture
  identical to the one before, typing still worked, SIGTERM gave the text
  console back.
- A second display plugged in while running was lit next to the first,
  swaybg and sengi-bar put their surfaces on it, and a terminal dragged
  across the edge showed on both.
- Relative pointer motion through libinput, with its acceleration: 100
  units sent slowly moved the pointer 58 to 74 pixels, the same distance
  sent in two steps 129 to 198.
- `sengi-compositor --exit` and SIGTERM end the session; started programs
  get `SENGI_COMPOSITOR_PID`.
- A forged `xdg_toplevel.move` is ignored and a toplevel that detaches
  its buffer unmaps cleanly (`examples/misbehave.rs`). A client that
  commits with a frame callback and no damage gets its callbacks at the
  refresh rate (`misbehave spin`: 69 a second on a 75 Hz display; 507
  before that was fixed).
- wp-security-context, see "Who may do what" below.
- Headless backend: pixman rendering into memory, scripted input, PNG
  screenshots.
- Winit backend: GLES rendering in a window with keyboard and pointer input.
  Tested nested inside headless labwc with injected keys and clicks (frames
  and our own cursor draw, the host cursor is hidden, the maximise button
  works), not yet by a person on a real desktop.

## What does not work

- Nothing has run on real hardware. Unseen there: GLES on i915, the
  cursor plane without the virtual switch, overlay planes and direct
  scanout (virtio-gpu offers none), suspend and resume, real vertical
  blanks.
- GLES costs memory that is Mesa's, not ours: Debian's `libgallium` links
  libLLVM, and about 42 MB of it are resident in any process that opens
  EGL, labwc included. See "Memory" below.
- Unplugging a display is handled (bars closed, windows moved to a screen
  that is left) but only unit-tested; the test guest cannot unplug.
- With several screens: new windows open on the first one, absolute
  pointing devices map to the first one, popups are constrained to the
  first screen, a taskbar is told every window is on the first screen, screens line up left to right in the order found with no
  way to arrange them or pick a mode, and a gap left by an unplugged
  screen is not closed.
- The touchpad settings (tap to click, disable while typing) have not
  been seen working: there is no touchpad in a VM.
- In the 0.3.0 live image, Ctrl+Alt+Fn is refused by logind ("failed to
  change vt") for the session user; it works when the compositor runs on
  libseat's builtin backend. The image probably has the same limit under
  labwc; polkit or seatd would lift it.
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
- No dmabuf on the pixman path, so GPU clients fall back to software
  there. No `wl_drm`; Mesa finds the device through dmabuf feedback.
- Missing protocols: primary selection, xdg-activation, fractional-scale,
  viewporter, text-input, screencopy, output management, output power
  management (so nothing can blank the panel when idle), idle-inhibit,
  pointer constraints and relative pointer (games, remote desktops),
  virtual keyboard and pointer, ext-foreign-toplevel-list.
- The icon of a drag-and-drop is not drawn; the drop itself works.
- No XWayland.
- No HiDPI scale: frames and cursors are drawn for scale 1.
- Clients started with `-s` or a key binding are not stopped when the
  compositor exits; they quit on their own when the socket closes.
- Frame time has not been measured.

## Memory

Measured in the GPU guest above with the whole session running and idle
(swaybg, sengi-bar, sengi-dock, mako, swayidle, sengi-focus, PipeWire),
release build, from `/proc/PID/smaps_rollup` and `smaps`, in MB:

| compositor                 | Rss | Pss | private dirty | libLLVM resident | heap | session Pss |
|----------------------------|-----|-----|---------------|------------------|------|-------------|
| sengi-compositor on pixman |  15 |  11 |             9 |                0 |    1 |          58 |
| sengi-compositor on GLES   |  88 |  83 |            31 |               42 |   13 |         130 |
| labwc on GLES              | 100 |  92 |            30 |               42 |   19 |         130 |

The 73 MB between pixman and GLES are Mesa: 42 MB of libLLVM (5.5 MB of
it dirty from relocations, the rest clean file pages that the kernel can
drop and that other GL programs share), 10 MB of libgallium, and 12 MB
of heap that Mesa allocates for its context and shaders. `ldd` on
`libgallium-25.0.7` in the image lists `libLLVM.so.19.1` and `libz3`;
virgl and iris do not use LLVM, the library is simply linked in.
Switching off Mesa's shader disk cache and GL threading changed nothing.
labwc pays the same on the same renderer. What would help is a Mesa
built without LLVM in the image, which is a packaging question and not
something the compositor can do. In all three rows 4 MB are the
wallpaper's shm buffer, mapped while swaybg keeps it attached; in ours
3 MB are the binary itself, which in the test sits on a tmpfs overlay
and so counts as dirty.

## Next steps toward replacing labwc

1. Run it on the target laptop: GLES on i915, the cursor plane, VT
   switching under logind, the touchpad, suspend, idle RAM and frame time.
2. Arranging screens and choosing modes, windows and popups that know
   which screen they are on.
3. Output power management, so swayidle can blank the panel.
4. screencopy (grim, and the CI screenshot), output management, primary
   selection, xdg-activation, fractional scale, idle-inhibit, pointer
   constraints and relative pointer.
5. Fullscreen, popup constraining, double-click to maximise, edge snapping,
   an Alt+Tab switcher, drag-and-drop icons.
6. Rounded bottom corners and shadows.
7. Keyboard layout, key bindings and input settings from configuration.
8. XWayland, started on demand.
9. Measure idle RAM and time to first frame against labwc on the target
   machine.

## Running it in the Sengi OS image

What it takes to offer the compositor as a second session beside labwc.
Nothing here has been wired in; the session below was run by hand in a
guest booted from the 0.4.0 ISO, with this start-up script and the
programs of that image: swaybg, sengi-bar, sengi-dock, sengi-focus and
swayidle came up, the bar's bell opened the notification centre, a
notification sent over D-Bus started sengi-notify and showed its card,
Super+Space opened sengi-launcher, Super+L locked with swaylock, and the
idle compositor used no CPU time in five seconds. The bar's network item
opens a terminal with iwctl there, not a quick settings panel; sengi-quick
was not reached from the bar in that image.

### The ISO builder

The crate is its own workspace, so it is built on its own, next to the
main one:

    cargo build --release --locked \
        --manifest-path crates/sengi-compositor/Cargo.toml \
        --no-default-features --features drm

That gives `sengi-compositor` (3.5 MB) under the crate's target directory,
or under `CARGO_TARGET_DIR` if that is set as `image/build.sh` does;
install it as `/usr/bin/sengi-compositor`. Build-time Debian packages
beyond what the main workspace needs: `libseat-dev libinput-dev
libudev-dev libgbm-dev libpixman-1-dev libxkbcommon-dev` (and `pkg-config`,
a C compiler). EGL and GLES are loaded at run time and need no headers;
that has only been built on a machine that happened to have them, so if
the build asks for `libegl-dev`, add it.

### The image

Run-time packages: `libseat1 libinput10 libudev1 libgbm1 libdrm2
libpixman-1-0 libxkbcommon0 libegl1 libgles2`, most of which labwc
already pulls in, besides `libgl1-mesa-dri`, `fonts-inter`, `swaylock`
and `swayidle`, which the image has. A cursor theme (`adwaita-icon-theme`
or any XCursor theme named `default`) is wanted; without one the pointer
is a plain drawn arrow.

The session script, a variant of `/usr/bin/sengi-session`, for instance
`/usr/bin/sengi-session-compositor`:

    #!/bin/sh
    # Start the Sengi OS desktop session on sengi-compositor.
    export XDG_CURRENT_DESKTOP=sengi:wlroots
    export XDG_SESSION_DESKTOP=sengi
    export XDG_SESSION_TYPE=wayland
    export MOZ_ENABLE_WAYLAND=1

    exec sengi-compositor --backend drm --locker '/usr/bin/swaylock -f' \
        -s 'LABWC_PID=$SENGI_COMPOSITOR_PID sh /etc/xdg/labwc/autostart'

There is no renderer test in it as labwc's script has for `WLR_RENDERER`:
the compositor chooses pixman on displays without a GPU by itself, and
says which in its first lines on stderr. Programs it starts get
`WAYLAND_DISPLAY` and `SENGI_COMPOSITOR_PID`, its own pid.

`/etc/xdg/labwc/autostart` works as it is with the `LABWC_PID=` prefix
above, which is how it was run. To make it work under either compositor
without that prefix, let it take whichever pid it was given, and use that
in the restart loop:

    session_pid=${LABWC_PID:-${SENGI_COMPOSITOR_PID:-}}
    ...
    while kill -0 "$session_pid" 2>/dev/null; do

One more line of it matters. It picks the wallpaper by `WLR_RENDERER`,
which nothing sets here, so swaybg always gets the one-colour buffer.
That is drawn correctly on both of our renderers; the tiled image is not
needed.

The idle line stays the same, with one condition: `swayidle ... 'swaylock
-f'` must start the same file that `--locker` names. The compositor lets
only that file ask for the lock, compared by device and inode, so a
swaylock found elsewhere on `PATH`, or a wrapper script around it, is
refused; the log then says so once, and the session does not lock on
idle. With `/usr/bin` first in `PATH` and `--locker '/usr/bin/swaylock
-f'` they agree.

Key bindings. The four in `rc.xml.in` are built in with the same keys:
Super+Space runs `sengi-launcher`, Super+Return `sengi-run foot`, Super+L
the locker, Super+Q closes the focused window. They cannot be changed
without rebuilding. Also built in: Alt+Tab and Alt+Shift+Tab (no on-screen
switcher) and Ctrl+Alt+F1 to F12. What labwc adds by default and this does
not have: Alt+F4, the window menu on Alt+Space, moving and resizing by
Super and a mouse button, snapping with Super and the arrow keys,
double-click on the title bar to maximise. The rest of `rc.xml.in` (theme,
focus, placement) has its counterpart in the tokens and is not read.

A second greetd entry or boot menu entry then only has to run the other
session script. Under greetd the session user cannot switch virtual
terminals (logind refuses it, the same as under labwc).

### The boot test

`sengi-boot-report` looks for `labwc` by name in two places: the "labwc
llvm maps" line and the list of programs that must be running. For this
session both should name `sengi-compositor`. It also waits for the socket
`/run/user/1000/wayland-0`. The compositor takes the first free name, so
that is what it gets when it is the first session after boot; in the
tests here it came second, after labwc, and was `wayland-1`. That first-
boot case has not been run.

What to check, from the guest, as the session user or root:

    pid=$(pgrep -o sengi-composito)
    # The compositor is up and says what it renders with.
    grep 'renderer:' /path/to/its/stderr
    # No LLVM in a guest without a GPU.
    grep -c -i llvm /proc/$pid/maps            # 0 on pixman
    # Idle means idle: no page flips over ten seconds.
    kill -USR1 $pid; sleep 10; kill -USR1 $pid
    grep 'page flips' /path/to/its/stderr | tail -2

Each SIGUSR1 prints two lines on stderr, so the session script has to
send stderr to a file for this (`2>/run/user/$UID/sengi-compositor.log`):
`N page flips, N with the cursor on its own plane, N dmabufs imported,
pointer at X,Y` and `session unlocked` or `session locked, locker alive,
N lock surface(s), locker told yes`. Equal flip counts ten seconds apart
are the idle check; "session locked ... locker told yes" after `swaylock
-f` is the lock check.

For a picture from inside, start the compositor with `--screenshot
/run/user/$UID/shot.png` and send SIGUSR2; the file (mode 0600) is
written within a second and shows every screen. On the boot test's
display without a GPU the compositor uses dumb buffers, and QEMU's own
`screendump` works as it does for labwc; it is only with a GL scanout
(virtio-vga-gl) that `screendump` has no picture. Keys through `sendkey`
and the absolute pointer through QMP reach it as they reach labwc.

The RAM and boot time lines need no change. Expect the compositor itself
at about 11 MB Pss on pixman against labwc's, and the same Mesa cost as
labwc on a GPU (see "Memory").

## Who may do what

The lock screen is only worth something if other programs cannot get
around it, so this is spelled out.

A client that connects through a security context, which is how Flatpak
apps arrive once the portal sees a compositor that supports it, is not
offered session lock, layer shell, foreign-toplevel management, idle
notification or the security context manager, and cannot bind them by
number either. It can still do what any window can: open windows, read
the clipboard while it has keyboard focus, and offer drags.

The session lock is offered only to the locker program: the first word
of `--locker` (default `swaylock -f`; a program and its arguments, no
shell syntax), found in `PATH` at start and compared with the
executable of the process that connected. Whoever holds the lock is the
only one who can unlock or supply lock surfaces; a second locker is
refused while the first lives, and one that connects after the first
died takes its place.

A program that runs unsandboxed as the user is not confined by any of
this, here or under labwc. It can put up a layer surface over everything
and take all keys, list, activate and close all windows, and watch idle
time, because the bar, the dock and swayidle do exactly that through the
same globals. It can start the locker itself, or connect and then
replace itself with the locker so that the executable check passes for a
connection its parent kept, and with the user's rights it could as well
trace the real locker or replace the session's start-up script. Keeping
untrusted programs in a sandbox is what protects against them.

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
- `src/lock.rs`: session lock and who may use it.
- `src/security.rs`: security contexts and what sandboxed clients are
  kept from.
- `src/cursor.rs`: cursor theme loading and the fallback arrow.
- `src/input.rs`: keyboard and pointer events, the drag grab, hit testing.
- `src/device_input.rs`: backend device events, built only with a backend
  that has devices.
- `src/render.rs`: drawing an output and frame callbacks.
- `src/headless.rs`, `src/winit.rs`, `src/drm.rs`: the backends.
- `src/gpu.rs`: choosing between GLES and pixman. Pure, unit-tested.
- `src/dmabuf.rs`: linux-dmabuf.
- `src/log.rs`: printing what Smithay logs, and the `say!` and `tell!`
  macros everything else prints with.
- `tests/`: tests that start the compositor on the headless backend and
  talk to it as clients (`common/` is the client, shared with
  `examples/misbehave.rs`; `harness/` starts the compositor).
- `src/png.rs`: minimal PNG writer for screenshots.
