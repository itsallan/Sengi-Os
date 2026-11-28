# sengi-welcome

The first-run welcome of Sengi OS (the app is called Welcome): a small window
with a few steps, shown once, on the first login on an installed system. On the
live image it asks instead whether to install Sengi OS.

    sengi-welcome               open it (also what the launcher entry does)
    sengi-welcome --again       the same; open it whether or not it was seen
    sengi-welcome --first-run   for the session: open only on an installed
                                system and only if not seen, else do nothing
    sengi-welcome --live        for the session: on the live image, ask once
                                per start, else do nothing

## The live question

"You are trying Sengi OS": it runs from the USB stick, nothing is installed and
changes are lost at shutdown. **Install Sengi OS**, which has the keyboard,
starts `sengi-installer` and closes the window; the installer asks its own
questions before it writes anything. **Try it first** closes it. Tab moves
between the two, Return presses the one with the focus ring and Escape tries
first, as does closing the window any other way.

It is asked once per start of the computer. As soon as the window is shown, the
number of this start (`/proc/sys/kernel/random/boot_id`) is written to
`~/.local/state/sengi-os/live-asked`. The runtime directory would not do: it is
emptied at logout, and the next login would ask again. Install Sengi OS stays on
the dock and in the launcher of the live system.

Nothing is resident once the window is closed. The toolkit would ask the desktop
portal for a colour scheme before opening a window, starting services that then
stay, so `--live` gives itself no session bus (`DBUS_SESSION_BUS_ADDRESS`) and
hands the session's one on to the installer.

## Steps

Back, Next and "Skip setup" are under every step. Return is Next and Escape is
Back; Tab walks the controls.

1. Welcome.
2. Look: Dark or Light, and a wallpaper from the pictures Sengi OS ships
   (`/usr/share/backgrounds/sengi`) or the plain colour. The choice shows at once.
3. Network: says whether Wi-Fi or a cable is connected, and has a button for
   `sengi-settings network`. The step is left out when a cable is connected.
4. Keyboard layout: type to search the layouts of the XKB list, then choose.
   The chosen layout goes first; any others already set stay after it.
5. Apps: a few suggestions that open the Store with the search already typed
   (`sengi-store --search TEXT`), and a button for the Store itself.
6. Done: Super+/ for the sheet of keyboard shortcuts, Print for a screenshot,
   and buttons for Files and Settings.

Nothing is downloaded. A choice is made by running `sengi-settings --set`, which
writes the setting and has the session take it up exactly as the Settings
window does (`appearance.theme`, `appearance.wallpaper`, `keyboard.layouts`),
so Settings' code is not linked in and nothing here knows how a theme is
applied. The window takes the new theme itself at once.

## Once only

When the window closes by any way (Finish, Skip setup, the close button) the
file `~/.local/state/sengi-os/welcome-done` (`$XDG_STATE_HOME/sengi-os/`) is
written, and the session does not start it again. Delete the file, or run
`sengi-welcome --again`, to see it again.

`image/rootfs/usr/lib/sengi-os/session-start.in` starts it at the end of the
session start. A subshell waits 3 seconds, then, on the live image (kernel
command line `boot=sengi`), runs `sengi-run sengi-welcome --live`, and on an
installed system, when the file is missing, `sengi-run --background
sengi-welcome --first-run`, which puts the welcome in `background.slice` so
that it gives way to the desktop; what it opens runs in `app.slice` as any app
does. The program checks the same things again. Nothing is resident
before or after. The boot test fails if the live image does not ask, or has it
running after the answer; the install test fails if an installed system asks.

## Tests

`cargo test -p sengi-welcome`: the step model, the marker rules, the commands,
and the real windows driven without a display (`tests/window.rs`,
`tests/live.rs`). With `SENGI_RENDER_DIR=dir` it writes a PNG of every step at
888x600 and 720x480, and of the live question at 440x192, 888x600 and 800x600;
`SENGI_THEME=light` or `high-contrast` draws the palettes that are not offered
yet (names end in `-light` and `-hc`).
