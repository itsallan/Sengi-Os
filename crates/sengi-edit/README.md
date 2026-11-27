# sengi-edit

The Sengi OS text editor (the app is called Text Editor): one plain UTF-8 file
in an ordinary window. A toolbar with the file's name in the middle, the text
edge to edge, and a quiet line of status at the bottom. No tabs, no syntax
colours, no plugins.

    sengi-edit               an empty page ("Untitled")
    sengi-edit FILE          open FILE (a path or a file:// address); if it does
                             not exist yet it is made when you save

It is not a service. It starts when asked for, exits when its window is closed
and leaves nothing running. The window's application id is `sengi-edit`, which
is also the name of the `.desktop` file here (Name=Text Editor, in the
launcher). `sengi-open` starts it for text files. Only the first file given is
opened.

## The window

- Toolbar: New, Open, Save, Undo and Redo at the left; Line numbers and Find at
  the right. Resting the pointer on a button shows its name and key. A dot
  beside the file's name means there are changes that are not saved.
- Text: DejaVu Sans Mono, wrapped at the window's edge. With line numbers on,
  lines are not wrapped and the text scrolls sideways as well. The bars at the
  edges can be dragged. The setting is not remembered between windows.
- Status: the line and column of the caret (and how many characters are
  selected), what just happened ("Saved", or why a save failed, in red), the
  number of characters, the line endings (LF or CRLF) and the encoding.

## Keys

| Key | Does |
| --- | --- |
| Ctrl+N | New page |
| Ctrl+O | Open |
| Ctrl+S, Ctrl+Shift+S | Save, Save As |
| Ctrl+Z, Ctrl+Y or Ctrl+Shift+Z | Undo, redo |
| Ctrl+A, Ctrl+C, Ctrl+X, Ctrl+V | The toolkit's selection and clipboard |
| Ctrl+F | Find; selected words are looked for |
| Return or F3, Shift+F3 | Next match, previous match |
| Ctrl+L | Line numbers |
| Tab | Spaces up to the next stop of four |
| Esc | Close the find bar or a dialog |
| Ctrl+W, Ctrl+Q | Close the window |

Find searches as you type, takes no notice of case for the letters A to Z, goes
round the ends of the text and says "2 of 5" or "No match".

Undo is the text field's own and goes back to the state when the file was
opened.

## Dialogs

- New, Open and closing the window with unsaved changes ask "Save changes?"
  with Save, Discard and Cancel. Return saves.
- Open and Save As are one dialog: the folder's files (folders first, hidden
  ones left out, at most 500) over a field that takes a name, a path or `~/`.
  Up and Down walk the list, Return opens or saves, double click does the same.
  Saving over another file asks "Replace?" first.

## Large files

A file over 512 KB is opened for reading only: it can be searched, selected
from and copied, and the buttons that change it are off. Slint lays the whole
text out again after every key typed, which at 1 MB took half a second a key
in a release build in the builder VM. Moving the caret and finding stay quick.

## What it will not open

Nothing is opened that could be damaged by a text editor, and it says why in
the window and leaves the editing buttons off:

- a file that is not valid UTF-8, or that has a NUL byte in it;
- a file over 5 MB. It is not loaded at all (so not read-only either): the
  whole text is kept in memory and laid out at once;
- a folder or a device.

## Saving

The text is written to a file next to the target and renamed over it, so a
failed save (a full disk, no permission) leaves the old file as it was, and a
link is followed rather than replaced. An existing file keeps its mode. A file
whose lines all end in CRLF is edited with plain newlines and saved with CRLF
again; any other mix of line endings is left exactly as it is.

## Tabs

Slint's text field draws a tab as one space and takes none from the keyboard.
So a file that uses tabs to indent, and for nothing else, is edited with four
spaces in each tab's place and saved with tabs again (every four leading spaces
become one), and the status line says "Tabs". Tab types spaces.

## Limits

- Tabs inside a line, or mixed with four or more spaces, are kept as they are
  and look like one space. A new file is always indented with spaces.
- There is no replace, and no menu on the right button.

## Files

- `src/doc.rs`: loading with its limits, line endings, saving.
- `src/browse.rs`: the folder in the file dialog and where a typed path leads.
- `src/find.rs`: searching, counting, line and column.
- `src/app.rs`: the window and the file.
- `ui/edit.slint`: the window; `ui/chrome.slint` the toolbar buttons and
  tooltip; `ui/dialogs.slint` the two dialogs. The text area is
  `ui/text-area.slint`, in the shared library, built from the tokens alone.
- `tests/window.rs`: the real window driven by keys and pointer without a
  display. With `SENGI_RENDER_DIR` set it writes `edit-*.png` pictures
  (`SENGI_THEME=light` for the light palette).
