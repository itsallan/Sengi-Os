# sengi-compositor

The Wayland compositor of Sengi OS: the default from 0.8, with labwc kept
as the fallback and the way back. It is built on
[Smithay](https://github.com/Smithay/smithay) 0.7. The programs the image's
session starts (sengi-bar, sengi-dock, sengi-launcher, sengi-notify,
sengi-lock, sengi-focus, swaybg, swayidle) run on it, it sleeps when
nothing changes, and it drives screens through DRM/KMS: with GLES where
there is a GPU, with pixman on the CPU where there is none. It has not run
on real hardware yet; everything below was seen in virtual machines.

The ISO starts it by default, for the login screen and the session. labwc
is in the image too: the boot menu entry "Sengi OS (labwc compositor)"
forces it, and if sengi-compositor hangs or fails the session starts labwc
instead. See "In the Sengi OS image" below. The crate
is its own Cargo workspace, excluded from the main one, so the normal build
does not compile Smithay.

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
XCursor theme named by `XCURSOR_THEME` (default `default`) at the size of
`appearance.cursor-size` in the settings: 24, 36 or 48 pixels. With Normal
chosen, `XCURSOR_SIZE` in the compositor's own environment is still honoured.
Programs it starts get `XCURSOR_SIZE` set to the same size, and a new size
is taken up on SIGHUP. Without a theme an arrow is drawn, scaled to match.

The title bars, menus and switcher follow `appearance.theme` (dark, light or
high-contrast, which has 2 px lines) and `appearance.text-size`, which scales
the type and the heights of title bars and rows. The compositor has no
animations of its own, so `appearance.reduce-motion` has nothing to stop in it.
Sticky keys and slow keys are not implemented.

    # In a window on your desktop, with a terminal inside it
    sengi-compositor -s foot

    # No display at all: run clients for five seconds, save what was drawn
    sengi-compositor --backend headless --exit-after 5 --screenshot out.png \
        -s sengi-bar -s 'sleep 2; foot'

    # On a virtual terminal, as the session (what greetd would run)
    sengi-compositor -s '. /usr/lib/sengi-os/session-start'

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
`sengi-session`, which runs

    sengi-compositor --backend drm --locker '/usr/bin/sengi-lock -f' \
        -s '. /usr/lib/sengi-os/session-start'

Name the backend there. Left to choose, the compositor opens a window
when `WAYLAND_DISPLAY` or `DISPLAY` name a display that is really there,
takes the screen when started on a terminal, and otherwise runs headless.

Started programs get `WAYLAND_DISPLAY` and `SENGI_COMPOSITOR_PID`, which
the session's restart loops watch. From inside the session,
`sengi-compositor --exit` (or SIGTERM) logs out and gives the terminal
back, and `sengi-compositor --lock` locks the screen and returns once
every screen is covered. `sengi-compositor --reload` (or SIGHUP, which is
also what `labwc --reconfigure` sends) has the settings read again, see
"What Settings sets" below. Ctrl+Alt+F1..F12 switch virtual terminals.

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
key name or a chord, `keydown` and `keyup` one key, to hold a modifier
while the pointer does something. Lines starting with `#` are comments. Steps go through
the same code as real input. Leave time between a step and the screenshot
that should show its effect; clients answer asynchronously.

### Several screens, headless

`--outputs 1280x720,800x600` makes several headless screens side by side,
named HEADLESS-1 and so on; `1920x1080:panel` makes one that has a list of
modes of its own, as a real display has, where a plain one takes any size.
With a screenshot the screens come out as one picture, in order. A build
with `test-hooks` also takes a line on the control socket (the `.control`
file beside the Wayland socket): `plug 800x600 [NAME] [panel]`, `unplug
NAME`, and `do STEP` for any script step, which is how `tests/outputs.rs`
plugs screens and moves the pointer without waiting on a clock.

## What works

Checked by running it in the builder VM, mostly headless with scripted input
and screenshots, and by the tests in `tests/`. The programs it was tried
against are foot, sengi-bar, swaybg, swayidle, sengi-focus, wlrctl and
gtk3-widget-factory, and, before the image had its own, waybar, fuzzel, mako
and swaylock, which is why they are named in some of the notes below. The
boot test runs the image's session on it (sengi-dock, sengi-launcher,
sengi-notify and sengi-lock included):

- xdg-shell toplevels over `wl_shm`. A new window is centred in the free
  area; later ones cascade by the title bar height.
- Every xdg toplevel gets our frame. xdg-decoration and the KDE
  server-decoration protocol are offered and always answered server side,
  whatever the client asks; a client that asks for client side, or never
  negotiates (Chromium, Firefox and GTK4 may do either), is framed anyway so
  close, minimise and maximise are always there. Fullscreen windows have no
  frame, X11 windows follow their Motif hints, popups and menus have none.
  The frame:
  title bar with close, minimise and maximise on the left,
  centred title in Inter, rounded top corners, 1 px border, separate looks
  for the focused and unfocused window, hover highlight on buttons. All
  sizes and colours are read from `tokens/sengi.toml`.
- A faint shadow around framed windows, moved down a little, darker for the
  focused window than for the others, none for a window that fills the
  screen or is maximised; a snapped one keeps it. It is a stack of flat
  translucent rounded rectangles of growing size and falling opacity, no gradient and no
  blur: eight small pictures (four corners and four strips, stretched along
  the sides) painted once per screen scale and focus state, so a window
  costs eight textured rectangles and nothing is painted per frame. Steps,
  reach, offset, opacity and colour are the `[shadow]` tokens; zero steps
  turn it off. `[appearance] window-shadows = false` in `settings.toml`
  turns it off for one user, at the next SIGHUP. Menus and popups have none.
- Drag the title bar to move. Drag an edge or corner to resize; the opposite
  edges stay put, a window never gets narrower than its buttons, and the
  title bar cannot be pushed under the bar. Drags are Smithay pointer
  grabs, like popup grabs and client clicks.
- Close, minimise and maximise buttons. Maximise again restores the old size
  and place; a minimised window comes back when the dock activates it. A
  client that asks to be maximised before it maps (`foot --maximized`)
  starts maximised.
- The image's own start-up script (`session-start`, which gets the
  compositor's pid in `SENGI_COMPOSITOR_PID`) brings up swaybg, sengi-bar,
  sengi-dock, sengi-focus and swayidle; the
  dock's Terminal item starts foot and shows it as running, its search item
  and Super+Space open sengi-launcher, which takes the keyboard and closes
  on a click outside.
- It draws only when something changed. With swaybg, sengi-bar, a dock,
  mako and foot running and no input, the compositor made 0 wakeups in ten
  seconds (566 before); with a blinking terminal cursor, about two per
  blink. Frame callbacks go to visible surfaces, hidden ones get one a
  second. Measured on the headless backend, debug build.
- Key bindings as labwc has them in the image: Super+Space runs
  sengi-launcher (fuzzel where that is not installed),
  Super+Return a terminal (`sengi-run foot`, or `foot`), Super+L
  the locker, Super+Q and Alt+F4 close the window that has the keyboard,
  Alt+Tab and Alt+Shift+Tab step through windows by recent use. Super+/
  shows them all.
- With Super held, the left button moves a window from anywhere on it and
  the right button resizes it by the nearest corner. A double click on the
  title bar maximises and restores.
- wlr-layer-shell on all the layers the session uses: sengi-bar and
  sengi-dock with exclusive zones, swaybg behind, notification cards on the
  overlay layer. Windows are kept out from under bars, also ones that appear later,
  and maximised windows fill exactly the area left free.
- Keyboard focus for layer surfaces: a launcher is typed into while it is open
  and the window gets the keyboard back afterwards.
- A surface on the bottom layer is a desktop program (sengi-desktop): it gets
  the right-click itself, and the compositor's own root menu opens only on
  the wallpaper.
- wlr-foreign-toplevel-management: title, app id, activated, maximised and
  minimised state; activate, close, maximise and minimise requests. The
  waybar taskbar lists windows and restores a minimised one on click;
  `wlrctl toplevel` can list, focus and minimise; sengi-focus connects.
- A drawn pointer cursor: theme cursors, resize cursors on frame edges,
  cursors set by clients (surface or cursor-shape).
- xdg popups, also on layer surfaces (waybar tooltips), with grabs: a GTK
  menu opens and a click elsewhere dismisses it.
- ext-session-lock and ext-idle-notify: the locker locks, swayidle locks
  after its timeout. While locked only the lock surfaces are drawn and get
  input, one per screen; keys typed and clicks made on the lock screen did
  not reach the terminal behind it, and bindings did not fire. The locker
  is told the session is locked once every screen has shown a frame drawn
  under the lock. If the locker dies, the screen stays locked and plain
  and the compositor starts the locker again; a second locker is refused
  while the first lives. Only the lock object that was granted the lock
  can unlock or supply lock surfaces. `tests/lock.rs` checks these rules
  against the running compositor, and on two screens in the GPU guest
  the locker covered both while `misbehave steal-unlock` and
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

### What ordinary programs expect

Checked by `tests/apps.rs` against the running compositor; where it
matters what was drawn, the test takes a picture of the screen through
wlr-screencopy and looks at the pixels.

- Primary selection: what one program selects is offered to whichever
  has the keyboard.
- Data control, both ext-data-control-v1 and wlr-data-control (smithay 0.7
  has both): a program with no window, such as wl-copy and wl-paste, a
  clipboard manager or sengi-shot, sets and reads the clipboard and the
  primary selection without the keyboard focus. Not offered to sandboxed
  clients or to Xwayland, which could otherwise read what the user copies
  from any program.
- xdg-activation: a program that has the keyboard, and shows it with the
  serial of that focus, gets a token that brings a window forward within
  ten seconds; a launcher does that for what it starts. A token asked for
  without the keyboard, with an old serial or made up brings nothing
  forward, so a program in the background cannot put itself in front.
- Fullscreen: a window that asks fills its screen without a frame and is
  drawn over the top layer, bar and dock included, for as long as it is
  the window in front; leaving puts it back where it was. A window that
  asks before it is first shown starts that way.
- viewporter and fractional-scale. Every output has its own scale, 0.5 to
  4, which Settings sets (`--scale FACTOR` is the scale of the outputs it
  has not set): 2 is announced as it is, 1.5 as 2 to those who cannot
  take a fraction and as 1.5 to those who ask. Each window is told the
  scale of the output most of it is on, and is drawn at each output's own
  scale where it is over two.
- Several outputs, and wlr-output-management (version 4, the one
  Settings' Display page speaks): every output is a head with its modes,
  position, scale and turn, switched on or off. A configuration is
  checked and put in place whole or refused (`failed`), a serial that is
  out of date is `cancelled`, and one that would switch every output off
  or lay two over each other is refused. A head that is not given a
  place keeps to its neighbours: when an output gets smaller or larger the
  ones beside it move up to it, so that the pointer can still cross. The
  layout is kept in `sengi-os/outputs.toml` beside the settings file, and
  read at the start, when a screen is plugged in and on SIGHUP. A change a
  program makes is saved twenty seconds after the last one, which is
  longer than the fifteen seconds Settings gives a change before it takes
  it back, so a change nobody keeps is never what is saved. Screens in
  the file that are not there are kept and ignored. New windows, the
  switcher and the menus come up on the output the pointer is on; a
  maximised, snapped or fullscreen window uses the output it is on; a bar
  takes room on its own output only. A window goes with its output when
  it is moved in Settings, and to the one that is left when its output
  is pulled out or switched off, fullscreen ones to fill that one.
- Adaptive sync is not offered: a head says it is off, and turning it on
  is refused. A saved screen is found by make, model and serial when it
  has a serial, else by its name; the DRM backend does not read the
  display's EDID, so for now that means the connector name.
- Pointer constraints and relative pointer: a locked pointer stays where
  it is and its movement is still reported; a confined one does not leave
  its surface or the region named. A constraint begins when the pointer
  is on the surface and ends when it leaves.
- wlr-screencopy, version 1, into shared-memory buffers, a whole output or
  a part of it. Refused while the session is locked, and not offered to
  sandboxed clients. The picture always includes the cursor.
- The picture a drag carries is drawn under the pointer.

## What does not work

Known gaps, as of today.

- Nothing has run on real hardware. Everything on the DRM side was seen in
  virtual machines (virtio-gpu, one with virgl): GLES on i915 and the 940MX,
  the cursor plane without the virtual switch, overlay planes and direct
  scanout, suspend and resume, real vertical blanks, plugging and
  unplugging real displays, mode changes and switching a panel off are all
  unverified there. The DRM code for plugging, mode changes and switching
  off compiles and is checked by clippy and by the headless backend, but
  no real connector, CRTC or mode change has driven it. The cursor is turned with the output, which is
  not what a rotated panel wants.
- A GPU that cannot draw is given up on: after 20 failed frames in a row
  the screens are rebuilt on pixman, and if that fails too the compositor
  exits with an error and the session starts labwc. That path has not been
  run with a broken GPU, and clients that already handed over GPU buffers
  cannot be shown after the switch.
- No brightness or Fn keys are bound. Only the volume keys and the
  terminal switches are.
- No lid or switch handling. A closed lid with an external monitor keeps
  the laptop panel in the layout, and closing it does not suspend. The
  panel can be switched off in Settings.
- No clipboard between X11 and Wayland programs.
- No text-input and no input method, so no on-screen keyboard or input
  method; nor virtual keyboard and pointer, ext-foreign-toplevel-list or
  ext-image-copy-capture (screen copies are wlr-screencopy's, which is what
  grim in Debian stable uses).
- Touch and tablets are not handled. An absolute pointing device acts as a
  pointer on the first output that is on, not on the one it belongs to.
- Scales 2 and 1.5 are tested on the headless backend and by screenshots
  there, but have not been looked at on the image or on a real screen.
- The night light needs a real display driver: the colour ramp has not been
  set on one (no real CRTC was driven), and which of the laptop panel and
  an external screen take a ramp depends on the driver. Mirroring and the
  primary output have not been tried with real screens either.
- Three-finger swipes and the click and scroll methods have only been
  tested without a device: the swipes by a script that calls the same
  code, the libinput settings not at all. They need a real touchpad.
- The touchpad settings (tap to click, disable while typing) have not been
  seen working: there is no touchpad in a VM.
- GLES costs memory that is Mesa's, not ours: Debian's `libgallium` links
  libLLVM, and about 42 MB of it are resident in any process that opens
  EGL, labwc included. See "Memory" below.
- Ctrl+Alt+Fn is refused by logind ("failed to change vt") for the session
  user of the 0.3.0 live image; it works when the compositor runs on
  libseat's builtin backend. The image probably has the same limit under
  labwc; polkit or seatd would lift it.
- Windows have no shadows. A fullscreen window has no key binding of ours
  to leave it by; labwc has none by default either.
- A client that draws its own title bar (Chromium, Firefox, GTK4) shows it
  inside our frame, so such a window has two bars. The alternative, trusting
  the client to go without, leaves it with no buttons if it does not.
  Not tried on real Chromium or Firefox, only on test clients.
- Windows are not re-placed when the usable area changes; they are only
  pulled out from under a bar. A maximised window cannot be dragged off its
  place; restore it first. Maximising moves the window at once but the
  client redraws a moment later.
- No key binding configuration. There is no Alt+Space; the window menu is
  on the right click of a title bar.
- Written but not exercised: moves and resizes started by the client
  (`xdg_toplevel.move`/`.resize` with a valid serial), popup constraining
  at a screen edge, a decoration mode change on a mapped window, frame
  callback throttling for a fully covered window, on-demand layer focus by
  click, client maximum sizes, scroll events, winit window resize,
  animated cursors.
- Title text is unkerned (Inter keeps kerning in GPOS) and has no fallback
  fonts.
- Nested under winit the host's keyboard layout is not followed.
- No dmabuf on the pixman path, so GPU clients fall back to software
  there. No `wl_drm`; Mesa finds the device through dmabuf feedback.
- X11 programs run through Xwayland, which starts once, at the start of a
  normal session (not on the login screen). The display number is chosen
  before it starts and every program the compositor starts, the startup
  programs first, gets `DISPLAY` for it at once and is never held: an X11
  client that connects before Xwayland is ready waits on the socket, which
  the compositor already owns. If Xwayland has not reported ready in five
  seconds it is given up for the session, and programs started after that
  get no `DISPLAY`. It stays up for the whole session on purpose, whether
  or not an X11 window is open, and is brought back, on the same number, by
  the next program the compositor starts if it dies. Until then a program
  that was started before it died, and is given the DISPLAY, is refused
  when it connects. Only the socket in
  /tmp/.X11-unix is used, not the abstract one, so a sandboxed program
  cannot reach it by sharing the network. Its windows get the same frame,
  title and switcher entry as the others, read from Motif hints and
  WM_CLASS. Only scale 1 has been tried. Known gaps: no click outside an
  X11 menu to dismiss it; a fullscreen override-redirect window is not
  usable; a transient window is not centred or kept above its parent; a
  window that withdraws and maps again is placed anew; moves and resizes a
  program asks for itself are not honoured, only the title bar's.
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
2. Arranging screens and choosing modes on real displays, and seeing the
   panel really go dark and come back.
3. Brightness and Fn keys, lid and switch events.
4. The gaps above that programs notice: the X11 clipboard bridge,
   text-input, touch and tablets, X11 windows at other scales.
5. Key bindings from configuration, and shadows.
6. Measure idle RAM and time to first frame against labwc on the target
   machine.

## In the Sengi OS image

The image runs the compositor by default, and has labwc beside it as the
fallback and the way back.

`image/build.sh` builds it from this workspace with `--release --locked
--no-default-features --features drm` and installs `/usr/bin/sengi-compositor`
(3.5 MB). The builder needs `libseat-dev libinput-dev libudev-dev libgbm-dev
libpixman-1-dev libxkbcommon-dev`; EGL and GLES are loaded at run time and
need no headers. Every library it links against is already in the image
because labwc needs it too, and the build stops if that ever changes.

sengi-compositor is the default. The session (`/usr/bin/sengi-session`) and
the login screen (`/usr/libexec/sengi-greeter-session`) are started on
labwc instead when it is asked for, in one of three ways, which
`/usr/lib/sengi-os/compositor.sh` looks at for both; the header of that
file has the order:

- The word `sengi.compositor=labwc` on the kernel command line. The live
  image's boot menu has an entry "Sengi OS (labwc compositor)" that adds
  it; the menu is hidden, so press L while the machine starts to pick the
  entry, or Esc for the menu. An installed system's menu has the same
  entry (Esc shows it). On any other entry press E and add the word to the
  `linux` line.
- A file `/etc/sengi-os/compositor` that says `labwc`, for every login
  until it is removed: `sudo mkdir -p /etc/sengi-os && echo labwc | sudo
  tee /etc/sengi-os/compositor`, then log out and in.
- A file `/run/sengi-os.compositor` that says `labwc`, until the next
  start. The boot test makes it.

The word `sengi.compositor=own` and the content `own` are still accepted,
and ask for what is the default.

The session then reads the environment file labwc would read (the user's
`~/.config/labwc/environment`, or else `/etc/xdg/labwc/environment`) for
the cursor size and the time zone, and runs

    sengi-compositor --backend drm --locker '/usr/bin/sengi-lock -f' \
        -s '. /usr/lib/sengi-os/session-start'

with its output in the journal (`journalctl -t sengi-compositor`). The
compositor writes `$XDG_RUNTIME_DIR/sengi-compositor.ready` once a screen
is lit and the session program is running, and removes it when it ends.
The script gives it 15 seconds to do that: a compositor that stays silent
is ended, and one that exits with an error within ten seconds of its start,
which includes the session program failing, is replaced too. Either way
the session writes a line to the journal (`-t sengi-session`) and starts
labwc instead, once. After any error exit that was not asked for, late ones
too, `/run/sengi-os/compositor-failed` is written, and the rest of that
boot, the login screen included, uses labwc, so that a crash does not turn
into a loop of login screens. That holds only if `/run/sengi-os` exists
and can be written, which the image's tmpfiles.d entry makes so (open to
every user, as the login screen and the session run as different ones; on
a machine with one user the only thing another user could do with it is
send the rest of the boot to labwc). The note also overrides an explicit
`sengi.compositor=own` for the rest of that boot; reboot, or remove
`/run/sengi-os/compositor-failed`, to try ours again.

An installed system has no labwc entry in its boot menu: the installer's
GRUB has only the text-mode entry. To get labwc there, press E on the
entry in GRUB and add `sengi.compositor=labwc` to the `linux` line, or
log in from the text-mode entry and write `labwc` into
`/etc/sengi-os/compositor`. `scripts/test-compositor-sh.sh` tests the
script against a fake compositor.

`/usr/lib/sengi-os/session-start` is the one start-up script of the
session; labwc's `autostart` runs the same file. Its restart loops watch
the compositor by the process number each one gives its children. It
starts swaybg through `/usr/lib/sengi-os/wallpaper.sh`: with the picture
chosen in Settings (`appearance.wallpaper`) if there is a usable one, else
the desktop colour, picked by `WLR_RENDERER` (which nothing sets here, so
the one-colour buffer, which both of our renderers draw correctly).
The compositor itself draws no wallpaper and decodes no image; swaybg keeps
the picture decoded while it runs.
The session menu's "Log out" runs `sengi-compositor --exit`.

Locking goes through the compositor. Super+L, or anything that runs
`sengi-compositor --lock`, makes it start the locker on a connection of
its own; see "Who may do what". `sengi-lock` started by anything else in
this session (swayidle, the session menu, quick settings) passes the
request on with `sengi-compositor --lock` and so behaves as it does under
labwc: `sengi-lock -f` returns once the screen is locked.

### The login screen

greetd starts `sengi-greeter-session` as its own user whenever nobody is
logged in. With sengi-compositor asked for, that runs

    sengi-compositor --backend drm --greeter --session /usr/bin/sengi-greeter

`--session` ends the compositor when the login screen ends, which is what
lets greetd start the session of whoever logged in. `--greeter` leaves
out every key binding except the ones that switch terminals, and the
locker, so nothing can be started as greetd's user from the keyboard;
and it takes the keyboard from `XKB_DEFAULT_*`, which the script exports
from `/usr/share/sengi-os/greeter/environment`, the file the installer
writes the chosen layout to. If the compositor hangs or gives up within
ten seconds, the login screen is started in labwc instead, as the session
is.

The login screen's compositor and the session's follow each other on the
same terminal. Each gets the display from logind when it starts and
hands it back when it ends.

### What Settings sets

The compositor reads `sengi-os/settings.toml` itself, the file Settings
writes, at start and again on SIGHUP. Settings still writes labwc's
`rc.xml` and `environment` from the same settings, for labwc; the
compositor reads neither for these. When something is changed, Settings
tells the compositor it is running under: `labwc --reconfigure` there,
`sengi-compositor --reload` here, so a change takes effect at once.

- `[keyboard]`: `layouts` and `switch` make the keymap, `repeat-delay` and
  `repeat-rate` the key repeat (600 ms and 25 a second unless set, as
  under labwc). The layout is always named to XKB, plain `us` too, so
  that the `XKB_DEFAULT_*` variables the session was started with, which
  are those of the last login, never stand in for it. A layout the
  system does not have leaves the keymap as it was and says so in the
  journal. The layout in use is told to clients the usual way, so the
  lock screen shows it.
- `[mouse]`: `speed`, `natural-scroll`, `left-handed`, for every pointing
  device that cannot be tapped.
- `[touchpad]`: `speed`, `natural-scroll`, `tap-to-click`,
  `disable-while-typing`, `click-method` (`areas`, the bottom corners,
  unless set, or `clickfinger`, one finger left and two right) and `scroll-method`
  (`two-finger` or `edge`), for those that can. A pad with real buttons
  has no click method, and one without an edge has no edge scrolling: the
  setting is left alone there. `gestures` switches the swipes off.
- `[display]`: `primary`, the output the bar and the dock go to, and the
  night light, see "Display: scale, arrangement and night light".
- `[clipboard]`: `history`, whether the last 20 texts copied are kept in
  memory for Super+V (off unless set).
- `[appearance]`: `window-shadows`, whether framed windows have a shadow
  (on unless set).
- `[power]`: `dim-after`, `blank-after` and `lock-after`, in seconds (zero
  is never), `lock-on-sleep` and `lid`. See "Idle, sleep and the lid".

`sengi-settings --screen off`, which swayidle runs under labwc, switches
the outputs off through wlr-output-power-management.
The compositor lights them again at the first key, click, scroll or
pointer movement, whether or not anybody asks it to. That first one only
lights the screen: it was made without seeing anything and reaches no
program and no shortcut (labwc passes it on). A lock that begins while a
screen is dark does not wait for it, unless switching it off failed. A client can hold idleness off with
idle-inhibit while its surface is mapped on an output, not minimised, and
the session is unlocked; a sandboxed client is not offered the protocol.

SIGUSR1 adds to its report a line on the keyboard, one on the outputs
and one for each pointing device, read back from libinput:

    keyboard: layout us,de, variant ,, options grp:alt_shift_toggle, repeat 25 a second after 600 ms
    outputs: eDP-1 on
    input: SynPS/2 Synaptics TouchPad (touchpad): speed 0.00, natural scroll off, left-handed off, tap on, disable while typing on

Not seen: any of the touchpad settings on a touchpad (a VM has none),
and a panel really going dark.

Seen, in an earlier image, in a QEMU guest booted from the then preview entry (no GPU there,
so pixman on dumb buffers, and no LLVM mapped): swaybg, sengi-bar,
sengi-dock, sengi-focus and swayidle come up; the bar's power icon opens
the session menu and its network item the quick settings panel; a
notification sent over D-Bus starts sengi-notify and shows its card;
Super+Return opens foot and the dock marks it running; the dock brings a
minimised window back; Super+Space opens the launcher; Super+L locks with
the locker; Alt+F4, Super and the mouse buttons, and the double click on the
title bar work as on labwc. Idle, the compositor used no CPU time in thirty
seconds and flipped no page. It is 11 MB Pss against labwc's 25.

Not seen: anything on real hardware, the lock after the idle timeout in
this image, and logging out through the session menu.

### Display: scale, arrangement and night light

Scale: every output has its own, from 0.5 to 4, announced as fractional
scale where a client asks for it. Settings offers 100 to 200 percent in
steps of 25. It is saved by output name in `outputs.toml`, applied live by
wlr-output-management and again on SIGHUP, and bar, dock and desktop take
their size from the output they are on, so they follow (tested at 1.5 and
2 on the headless backend).

Arrangement: Settings' Position choice is a position set through the same
protocol, left of, right of, above or below another output. An overlap is
refused, with one exception: outputs of the same logical size in the same
place are a mirror, and each draws the whole desktop there. Mirroring
outputs of different sizes is not done: Settings sets the second to the
first's size and scale, or does not offer it. `display.primary` names the
output that the bar and the dock (layer surfaces called `sengi-bar` and
`sengi-dock` that name no output) are put on, and when it changes on SIGHUP
they are moved there. Without it they go where the pointer is. Laptop
panel, lid and plug-in events do not move them again.

Night light: `display.night-light`, `night-from` and `night-to` (21:00 and
07:00 unless set, local time) and `night-strength` (0 to 100, which is 6500
K down to 3000 K). The tint is the CRTC's colour ramp (the gamma table the
kernel gives every CRTC), set when the night begins or ends, not a pass over
each frame: it costs nothing while drawing, direct scanout and damage
tracking stay as they are, and what wlr-screencopy hands out, sengi-shot
included, is the picture before the ramp, so screenshots are untinted. It is
chosen over a multiply in the final composition because that would cost
every frame and tint the screenshots. A driver without a ramp (virtio-gpu in
a VM) gets no night light, and the journal says so
once. One timer is set for the next edge; nothing wakes the compositor
meanwhile. It is set again on SIGHUP and when the session comes back after a
switch of terminals or sleep. The headless backend has no display, so it
applies the same ramp to the pictures its script saves, as a panel would,
and tests set the clock with `SENGI_COMPOSITOR_NIGHT_NOW=HH:MM:SS`. The
colour of the ramp changes at once at the edge, with no fade.

Touchpad gestures: libinput's three-finger swipes. Left is the window used
before this one, as Alt+Tab goes, a further 70 pixels of travel the one
before that, and going back undoes it; the window is picked when the fingers
lift. Up opens the switcher and keeps it open, left and right then move it,
down picks, Escape closes. There is no overview of the windows, so up shows
the switcher only. Pinch, hold and swipes with other numbers of fingers are
ignored. `swipe FINGERS DX DY` in a script plays one.

SIGUSR1 adds a `night light:` line.

### Idle, sleep and the lid

Under this compositor nothing outside it watches for idleness. After the
times in `[power]` without a key, click or movement it

1. dims the screen after `dim-after` (120 s unless set): it runs
   `sengi-brightness dim`, which remembers the brightness in the runtime
   directory and turns the panel down to 30 percent of it, never below the
   floor of the brightness keys;
2. switches every output off after `blank-after` (300 s), the same
   switching-off wlr-output-power-management does, and any input lights
   it again (that first input is not passed on);
3. locks after `lock-after` (0, never, unless set), by starting the
   locker like Super+L does.

Input brings the brightness back (`sengi-brightness restore`) and starts
the count again. A dimming time that is not before the time for switching
off is not used. A surface holding idleness off with idle-inhibit stops the
count while it shows, and the count starts again when it stops.

It takes no wake-ups for this. `src/power.rs` keeps the time of the last
input and one calloop timer set for the next step that is due; input only
writes the time down. The timer fires when a step is due, or earlier if
settings changed, finds nothing due and sets itself again. With nothing
left to do, such as the screen off and no lock set, there is no timer.
`IdleClock` takes the time as an argument, so the unit tests in
`src/power.rs` run it on a clock they move by hand. The settings are read
again on SIGHUP, and the timer is set again for the new times at once.

SIGUSR1 reports it:

    idle: awake, dim 120s, off 300s, lock never; lid: left to the system

Locking before the computer sleeps is not done by a program that waits for
it. The image has `/usr/lib/systemd/system-sleep/sengi-lock-on-sleep`,
which systemd runs, as root and before it suspends, for every sleep (the
lid, the power button, a menu). For each running compositor's control
socket in `/run/user/*` it runs `sengi-compositor --lock-for-sleep` as that
user. The compositor locks and answers once every screen is covered, so
the hook holds the suspend until then; the lock screen is what shows on
waking. It does nothing when `lock-on-sleep` is off, on the login screen,
or when the session is already locked. Under labwc the swayidle
`before-sleep` does it.

The lid and the power button are logind's, set in the image by
`/etc/systemd/logind.conf.d/10-sengi.conf`: closing the lid sleeps, on
battery or on mains, and does nothing when docked; a short press of the
power button sleeps and a long one switches off. `lid = "nothing"`, set on
Settings' Power page for a machine that has a lid, has the compositor ask
logind to leave the lid alone: it starts `systemd-inhibit
--what=handle-lid-switch ... cat` and keeps the pipe to `cat` open. The
inhibitor lasts as long as the pipe, so it goes when the setting does and
when the compositor dies, which a lid left ignored by mistake would not.

Not seen, because none of it can be here: a suspend and a resume, the hook
running under systemd-sleep, the lock screen on waking, the lid switch,
logind honouring the inhibitor, a panel dimmed through logind, and a real
screen going dark and coming back.

### Snap hints and the shortcut sheet

Dragging a window by its title bar to the top of a screen maximises it on
release, and to the left, right or bottom edge fills that half. While the
pointer is within a pixel of such an edge an outline shows the place: the
accent, flat and see-through (`snap.hint-fill-alpha` and `hint-edge-alpha`
in the tokens), with the theme's corner radius, drawn above the windows
and gone when the button is let go or the pointer leaves the edge. The
place is worked out by the same code that places the window on release
(`snap_at` and `half_frame`), so the two cannot differ. At a top corner the
top wins, as it does on release; there are no quarter snaps. The picture is
made once for a place and a scale, not on every movement.

Super+/ (and Super+F1, for layouts where / needs Shift) shows a sheet of
the shortcuts in the middle of the screen, Escape, the same keys again or a
click close it, and `sengi-compositor --shortcuts`, which Settings'
Keyboard page runs, opens it. It is made from `keys::BINDINGS`, the table
the keys are looked up in, so it lists exactly what is bound; a test tries
every key with every combination of modifiers and fails for an action the
sheet does not list. Its two columns fit a 1280-pixel screen.

### Clipboard history

Off unless `[clipboard] history = true` in the settings file, which
Settings' Keyboard page, under Privacy, switches; it is off by default
because what people copy is often private. When it is on, the compositor
keeps the last 20 texts that were copied, newest first, and Super+V
shows them in a list in the middle of the screen that is active, drawn
like the menus from the tokens (the same code, a wider card). Each row
is one line: the first 60 characters, with new lines and tabs squeezed
to a space and an ellipsis where the text goes on. The last row is
Clear history.

- Up and Down move, Return puts the chosen text on the clipboard (as
  `text/plain;charset=utf-8`, `text/plain` and `UTF8_STRING`) and closes
  the list, Delete forgets the selected entry, Escape or Super+V again
  closes. A click on a row does what Return does, a click anywhere else
  only closes. Nothing is typed for the user: the program in front gets
  the text when the user pastes, with Ctrl+V as always.
- A copy is recorded only if it offers `text/plain;charset=utf-8`, does
  not offer `x-kde-passwordManagerHint` (what KeePassXC, Bitwarden and
  other password managers add, whatever its value), and is at most
  100 KiB. A longer text is not cut, it is skipped. Pictures, files and
  texts offered only as plain `text/plain` are skipped. The same text
  twice is one entry, moved to the front.
- Memory only: nothing is written to disk and nothing is logged; the
  SIGUSR1 report says `clipboard: on 3`, a count and never a text. The
  list is emptied when the session locks (the lock screen is not a place
  for it either: Super+V does nothing there), when the setting is turned
  off and the settings are read again, and by Clear history. The most it
  can hold is 20 times 100 KiB, 2 MB.
- The text is read from the program that owns the clipboard as soon as it
  is offered, through the same request a paste makes, on the event loop
  with no thread. A program that does not hand its text over within two
  seconds is given up on. Nothing runs while the clipboard is not
  changing, so an idle desktop still makes no wake-ups for it.
- Sending a chosen text to a program that pastes it takes a short thread
  that gives up after ten seconds, so one program that stops reading
  holds up nothing but its own paste.

Tests are in `tests/clipboard.rs` (a client with no window sets and reads
the clipboard through data control; the history, Super+V, Return, the
pointer, Delete, Clear history, the lock, the password hint, the size limit
and the setting being turned off) and in `src/clipboard.rs`.

### The boot test

`scripts/boot-test.sh` boots the default entry and expects
sengi-compositor. `SENGI_COMPOSITOR=labwc scripts/boot-test.sh ISO OUT`
hands the guest the `/run` file above, under either firmware, and expects
labwc. `SENGI_LABWC_ENTRY=1` gets there through the boot menu instead: it
presses the labwc entry's hotkey, L, between SeaBIOS handing over and the
kernel starting (BIOS only). The guest reports which compositor the session
really came up on, and the test fails if that is not the one meant. The
checks and screenshots are the same for both.

From inside the guest, SIGUSR1 makes the compositor print two lines to the
journal: `N page flips, N with the cursor on its own plane, N dmabufs
imported, pointer at X,Y` and `session unlocked` or `session locked, locker
alive, N lock surface(s), locker told yes`. Equal flip counts ten seconds
apart are the idle check.

### When the screen freezes

A compositor whose main loop is stuck draws no cursor, reads no keys and
cannot switch terminals, so it looks like a frozen machine. A small thread
asks the loop for an answer every 3 seconds and, if none came within 2,
writes `watchdog: the main loop has not answered for N s (main thread state
X, waiting in F)` to the journal, again each time the stall has doubled, and
`answered again` if it comes back. State S in `futex_wait_queue` is a
deadlock, R a loop that spins, D a wait on a disk or a driver. The line
does not appear if the kernel itself has stopped. It costs the idle
compositor one wake-up every 3 seconds and draws nothing.

The journal is kept in memory, on an installed system too, so a restart
loses it. To keep it through a freeze, have it written to a USB stick that
Files has opened (here one labelled `LOGS`) and leave that running while
reproducing:

    log=$(findmnt -rno TARGET LABEL=LOGS)/freeze.log
    sudo journalctl -b -f -o short-precise | while IFS= read -r line; do
      printf '%s\n' "$line" >>"$log"
      sync -d "$log"
    done

The kernel's messages are in there too, so a graphics driver that stops
the machine leaves what it said, as far as the last write got.

On an installed system the journal can instead be kept on disk, after which
`journalctl -b -1` shows the start before this one:

    printf '[Journal]\nStorage=persistent\nSystemMaxUse=64M\n' |
      sudo tee /etc/systemd/journald.conf.d/99-keep.conf
    sudo systemctl restart systemd-journald

Two things tell the cases apart without a log. If Alt+SysRq+B (SysRq is on
Print Screen) restarts the machine, the kernel was still running. If the
boot menu's "NVIDIA graphics off" entry (press N while the machine starts)
makes a freeze go away on a laptop with two graphics chips, nouveau was
the cause. An installed system has no such entry: press E on its boot
entry and add `modprobe.blacklist=nouveau` to the `linux` line. The
compositor opens only the display chip the firmware booted on; it never
opens a second one, so an output wired to the other chip stays dark with or
without the driver. These show which chip each output belongs to:

    grep DRIVER /sys/class/drm/card?/device/uevent
    grep . /sys/class/drm/card*-*/status

### A client that commits without damage

`misbehave spin` commits with a frame callback and no damage as fast as
callbacks come; `misbehave flood` commits a hundred times between round
trips with no callback. Release build in the image, QEMU guest with two
CPUs, pixman, CPU time of the compositor per run of 3.5 seconds:

| compositor       | spin: callbacks a second | spin: CPU      | flood: commits in 3 s  | flood: CPU     |
|------------------|--------------------------|----------------|------------------------|----------------|
| sengi-compositor | 73 to 74                 | 0.18 to 0.24 s | 619,000 to 627,000     | 2.49 to 2.53 s |
| labwc            | 1611 to 1651             | 0.63 to 0.66 s | 1,443,000 to 1,625,000 | 2.07 to 2.12 s |

We hold frame callbacks to the refresh rate where nothing was shown, so
the spinning client costs a third of what it costs labwc, which answers
every commit. One empty commit, though, costs us about three times what it
costs labwc (4.0 against 1.4 microseconds). System calls are not it (a few
thousand for 290,000 commits); the time goes in protocol handling in user
space, which has not been profiled. An empty frame on the DRM backend
costs about 1 ms of CPU where the headless backend needs 0.3 ms, also not
looked into.

## Who may do what

The lock screen is only worth something if other programs cannot get
around it, so this is spelled out.

A client that connects through a security context, which is how Flatpak
apps arrive once the portal sees a compositor that supports it, is not
offered session lock, layer shell, foreign-toplevel management, idle
notification, idle inhibit, data control or the security context manager, and cannot bind them by
number either. It can still do what any window can: open windows, read
the clipboard while it has keyboard focus, and offer drags.

The session lock is offered only to a locker the compositor started
itself. The locker is the command of `--locker` (default `sengi-lock -f`;
a program and its arguments, no shell syntax, found in `PATH` at start).
The compositor starts it on Super+L, when `sengi-compositor --lock` asks,
and for a locked session whose locker died. It makes a socket pair, keeps
one end as a client that may see the lock, and hands the other to the
program as `WAYLAND_SOCKET`. No client that connects to the session's
socket is shown the lock, whatever program it is. Nothing is looked up
about the locker's process, so a locker that has closed itself to other
processes (`PR_SET_DUMPABLE` off, as sengi-lock does to keep its password
out of reach) works like any other. Whoever holds the lock is the only
one who can unlock or supply lock surfaces, and no second locker is
started while one lives.

`--lock` is asked for over a socket beside the Wayland one
(`$XDG_RUNTIME_DIR/wayland-N.control`), which like it is open to the
user's own programs only. All it can do is have the screen locked.

A program that runs unsandboxed as the user is not confined by any of
this, here or under labwc. It can put up a layer surface over everything
and take all keys, list, activate and close all windows, and watch idle
time, because the bar, the dock and swayidle do exactly that through the
same globals. It can ask for the screen to be locked, and with the user's
rights it could as well replace the session's start-up script. Keeping
untrusted programs in a sandbox is what protects against them.

## Layout

- `src/layout.rs`: window placement and focus order. Pure, unit-tested.
- `src/frame.rs`: frame geometry: buttons, hit testing, resize and clamping
  arithmetic. Pure, unit-tested.
- `src/decoration.rs`: theme from the tokens and the title bar painter. Pure,
  unit-tested.
- `src/text.rs`: finding the typeface and rasterising a line of text.
- `src/clipboard.rs`: the clipboard history, and reading what was copied.
- `src/keys.rs`: the key binding table, and the shortcut sheet made from
  it. Pure, unit-tested.
- `src/cli.rs`, `src/script.rs`: arguments and input scripts, unit-tested.
- `src/state.rs`: state shared by all backends, outputs, focus.
- `src/window.rs`: per-window state, frame textures, move, resize, maximise.
- `src/shell.rs`: wl_compositor, xdg-shell and xdg-decoration.
- `src/layer.rs`: layer-shell.
- `src/foreign_toplevel.rs`: wlr-foreign-toplevel-management.
- `src/config.rs`: what is taken from the settings file. Pure,
  unit-tested.
- `src/output_power.rs`: switching outputs off and on for clients.
- `src/power.rs`: the idle clock, dimming, switching off, the idle lock and
  the lid hold. The clock is pure and unit-tested.
- `src/snap.rs`: where a dragged window will snap, and the outline.
- `src/idle.rs`: idle-inhibit.
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
