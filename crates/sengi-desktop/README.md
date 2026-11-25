# sengi-desktop

The desktop: the folders and files of `~/Desktop` as icons on the wallpaper,
and a menu on the right button.

    sengi-desktop        runs for the whole session; the session starts it

It is a layer-shell program (through sengi-layer) on the bottom layer: above
the wallpaper, below every window, over the whole of each output and clear of
the bar and the dock. One surface per output, all showing the same folder. The
session starts it next to sengi-bar and sengi-dock and restarts it if it dies.
It makes `~/Desktop` on the first run.

## What it does

- A right click on bare desktop opens a menu: New Folder, New File, Open Terminal Here,
  Settings.... A left click anywhere else closes it and clears the selection.
- New Folder makes "untitled folder" ("untitled folder 2", ...) and puts it
  straight into rename. Return or a click elsewhere keeps the new name, Escape
  keeps the old one.
- A click selects an icon. A double click, or Return, opens it. F2 renames
  it, Delete moves it to the Trash. A right click on an icon opens Open,
  Rename and Move to Trash.
- Move to Trash follows the freedesktop.org Trash specification for the user's
  own trash (`~/.local/share/Trash`, with the record in `info/`). If the item
  cannot be moved there it stays where it is and a notice says why. Nothing is
  ever deleted.
- Icons are drawn from Phosphor Bold glyphs in token colours: folders and
  files, no thumbnails and no image decoder. Hidden names (starting with a
  dot) are left out. At most 400 entries are shown.
- The folder is read again only when something in it changes, through inotify:
  one thread sleeps in `read`, so nothing wakes between changes.

## Opening

The image has no `xdg-open`, but it has Files (`sengi-files`). Opening an
icon runs `xdg-open` if it is installed, and otherwise a file goes to
`sengi-open` and a folder to the first of sengi-files, thunar, pcmanfm,
pcmanfm-qt, nautilus, nemo, caja or dolphin that is on `PATH`. If there is
none, a notice says "No file manager installed" (for a file: "Nothing
installed can open this file").

## Keyboard

The surface asks for the keyboard "on demand": it gets it when the desktop is
clicked and gives it back when a window is.

## Layout of the code

- `files.rs`: listing, new folder names, rename.
- `trash.rs`: the Trash.
- `open.rs`: starting a file manager, the terminal and Settings.
- `watch.rs`: inotify.
- `desk.rs`: the selection, the rename and the rules, with no window in it.
- `app.rs`: joins the desk to the Slint component in `ui/desktop.slint`.

The render test writes pictures of the desktop when `SENGI_RENDER_DIR` names
a folder.
