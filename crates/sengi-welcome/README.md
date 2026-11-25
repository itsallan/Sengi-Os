# sengi-welcome

The first-run welcome of Sengi OS (the app is called Welcome): a small window
with a few steps, shown once, on the first login on an installed system.

    sengi-welcome               open it (also what the launcher entry does)
    sengi-welcome --again       the same; open it whether or not it was seen
    sengi-welcome --first-run   for the session: open only on an installed
                                system and only if not seen, else do nothing

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
session start, when the file is missing and the kernel command line has no
`boot=sengi` (the live image's word, so the live session never shows it): a
subshell waits 3 seconds, then runs `nice -n 19 sengi-run sengi-welcome
--first-run`. The program checks the same two things again. Nothing is resident
before or after; the boot test fails if the live image has it running.

## Tests

`cargo test -p sengi-welcome`: the step model, the marker rule, the commands, and
the real window driven without a display (`tests/window.rs`). With
`SENGI_RENDER_DIR=dir` it writes a PNG of every step at 888x600 and 720x480;
`SENGI_THEME=light` makes the light ones (names end in `-light`).
