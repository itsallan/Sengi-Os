# sengi-launcher

The Sengi OS application search and menu. It opens a panel in the middle of
the screen: type to filter, Up and Down (or the wheel) to move, Return to
choose, Escape or a click outside to close.

    sengi-launcher                      search installed applications
    sengi-launcher --menu --prompt Session < lines
                                        choose one of the lines on standard
                                        input; it is printed on standard output
    sengi-launcher --list [QUERY]       print ranked results, no window

Options: `--prompt TEXT`, `--lines N`, `--wide` (a menu as wide as the
application search). `--dmenu`, `-d`, `-p` and `-l` are accepted for scripts
written for other launchers.

Exit status: 0 when something was chosen, 1 when the panel was dismissed, 2 on
an error.

Only one panel is open at a time. Running the same command again closes the
open panel, so one key binding toggles it; running the other kind replaces it.

## Applications

Desktop entries are read from `$XDG_DATA_HOME/applications`, each
`$XDG_DATA_DIRS/applications` and Flatpak's export directories, earlier
directories overriding later ones by desktop file ID. Entries that are hidden,
`NoDisplay`, not for this desktop, not applications, or whose `TryExec` is
missing are left out.

The chosen application is started through `sengi-run` when that is installed,
inside `foot -e` for `Terminal=true`. The Exec line is split and expanded by
the Desktop Entry rules and run directly, never through a shell. If the program
it names is not installed, the panel stays open and says so.

Results are ranked: start of the name, start of a word in the name, anywhere
in the name, then the generic name, keywords and comment, then names that only
contain the typed letters in order. Ties go to the applications launched most
and most recently (`$XDG_STATE_HOME/sengi-launcher/usage`), then the alphabet.

## Known limits

- No application icons.
- Desktop actions, D-Bus activation and startup notification are not
  supported, and no files or URLs are passed to the application.
- Accents are ignored when matching for Latin letters only. Combining marks
  outside U+0300 to U+036F are not attached to their letter, so a highlight
  can separate the two.
- Names in right-to-left scripts are matched, but a highlighted name is drawn
  as separate runs laid out left to right, so a partly highlighted
  right-to-left name can appear in the wrong order.
- Desktop files over 1 MiB are ignored, shown text is cut at 256 characters,
  and a menu is read up to 1 MiB or 10,000 lines.
- Copy and paste work inside the panel only.

## Building

Needs `libxkbcommon-dev` to build (through sengi-layer) and `libxkbcommon0`,
`libfontconfig1` and `libfreetype6` to run.

`cargo run -p sengi-launcher --example preview` shows the panel in an ordinary
window, with made-up applications, for working on the design.
