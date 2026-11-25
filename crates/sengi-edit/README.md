# sengi-edit

The Sengi OS text editor (the app is called Text Editor): one plain UTF-8 file
in an ordinary window, with a bar on top, the text, and a line of status at the
bottom. No tabs, no syntax colours, no plugins.

    sengi-edit               an empty page ("Untitled")
    sengi-edit FILE          open FILE (a path or a file:// address); if it does
                             not exist yet it is made when you save

It is not a service. It starts when asked for, exits when its window is closed
and leaves nothing running. The window's application id is `sengi-edit`, which
is also the name of the `.desktop` file here (Name=Text Editor, in the
launcher). `sengi-open` starts it for text files. Only the first file given is
opened.

## Keys

- Ctrl+S saves. A page with no name asks for one: a name typed there is put in
  your home folder unless it is a path or starts with `~/`, and a name that is
  taken is refused (nothing is replaced).
- Ctrl+Z undoes and Ctrl+Shift+Z redoes (the toolkit has no Ctrl+Y); Ctrl+A, Ctrl+C, Ctrl+X and
  Ctrl+V are the toolkit's. Undo is the text field's own and goes back to the
  state when the file was opened or last set.
- Ctrl+F opens the find bar: the text is searched as you type, case does not
  matter for the letters A to Z, Return or F3 goes to the next match, Shift+F3
  to the one before, round the ends of the text; the bar says "2 of 5" or "No
  match". Escape closes it.
- Ctrl+W or Ctrl+Q closes the window, and so does its close button. With
  unsaved changes (the title and the name show `*`) it asks first: "Close
  without saving?", with Escape or "Keep editing" to go back.

The line at the bottom says the line and column of the caret, or what just
happened ("Saved", or why a save failed, in red).

## What it will not open

Nothing is opened that could be damaged by a text editor, and it says why in
the window and leaves the Save button off:

- a file that is not valid UTF-8, or that has a NUL byte in it;
- a file over 5 MB. It is not loaded at all (so not read-only either): the
  whole text is kept in memory and drawn wrapped, which is not what a log of
  50 MB is for;
- a folder or a device.

## Saving

The text is written to a file next to the target and renamed over it, so a
failed save (a full disk, no permission) leaves the old file as it was, and a
link is followed rather than replaced. An existing file keeps its mode. Line
endings are whatever the text holds; nothing is converted.

## Files

- `src/doc.rs`: loading with its limits, saving, names for new files.
- `src/find.rs`: searching, counting, line and column.
- `src/app.rs`: the window and the file.
- `ui/edit.slint`: the window. The text area itself is `ui/text-area.slint`, in
  the shared library, built from the tokens alone.
- `tests/window.rs`: the real window driven by keys without a display. With
  `SENGI_RENDER_DIR` set it writes `edit-*.png` pictures.
