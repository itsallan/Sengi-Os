# sengi-files

The Sengi OS file manager: an ordinary window with places on the left, the
folder on the right, a path bar with Back and Up above it and one line of
status below.

    sengi-files               open Files on your home folder
    sengi-files FOLDER        open it on FOLDER (a path or a file:// address;
                              a file stands for the folder it is in)

It is not a service. It starts when asked for, exits when its window is
closed and leaves nothing running. The window's application id is
`sengi-files`, which is also the name of the `.desktop` file here (it claims
`inode/directory`); the image installs that as
`/usr/share/applications/sengi-files.desktop`, and the default dock ends with
it. `sengi-desktop` opens folders in it.

## What it does

- Places: Home, and Desktop, Documents, Downloads and Pictures when they exist,
  and the Trash.
- The list shows folders first, then files, each by name without regard to
  case and with numbers in order ("file2" before "file10"). Names that start with a dot are hidden until Ctrl+H or the switch at
  the bottom. Sizes and times show when the window is wide enough.
- Click selects; Ctrl-click adds or removes; Shift-click and Shift with the
  arrow keys extend; Ctrl+A takes all. Up, Down, Home, End, Page Up and Page
  Down move.
- Double-click or Return opens a folder in the window, and a file with
  `xdg-open`, or `sengi-open` when that is not installed (the image has
  no `xdg-open`). With neither, the line at the bottom says so, in red,
  naming the file: nothing fails silently.
- F2 renames in place (the name is taken as typed, minus spaces at the ends;
  a name that exists is refused and the field stays open). Ctrl+Shift+N or the
  New Folder button makes "untitled folder" and starts renaming it.
- Ctrl+C, Ctrl+X, Ctrl+V copy, cut and paste inside Files; the clipboard is
  not shared with other programs. Nothing is ever replaced: a name that is
  taken makes the pasted item "name 2", "name 3" (before the extension), and
  the line at the bottom says so. Folders are copied with everything in them;
  links are copied as links and never followed, so a link out of the folder
  cannot pull in what is on the other side; sockets, pipes and devices are left
  out and counted. A copy that fails is removed again. A folder cannot be
  copied or moved into itself. Cut across disks is a copy and then a removal,
  and the original is removed only when the whole copy worked.
- Delete moves to the Trash (`~/.local/share/Trash/files` and a `.trashinfo`
  record, as `sengi-desktop` does; the code is `sengi_system::trash`, which
  both use). In the Trash view, Restore puts items back (a new name if the old
  one is taken, the folder made again if it is gone) and Empty Trash asks first
  and then deletes for good.
- Back and Up buttons, Alt+Left (back), Alt+Right (forward again), Alt+Up and
  Backspace (up) and F5 or Ctrl+R to read the folder again. Going somewhere
  new forgets the way forward.
- Right-click on an item selects it (unless it is in the selection already) and
  offers Open, Rename, Copy, Cut and Move to Trash; in the Trash, Restore. On
  the empty space it offers New Folder, New File, Paste and a Show Hidden
  switch; in the Trash, Empty Trash. What cannot be done now is dimmed. Escape
  or a click elsewhere closes the menu.
- The list follows the folder: when something is made, removed or renamed in it
  by another program, it is read again about 150 ms after the changes stop,
  with the selection and the scroll kept. A name being typed is left alone
  until it is done.
- Drag items onto a folder row or a place in the sidebar to move them there (a
  place that is the Trash trashes them). The same rules as Cut and Paste
  apply: only inside home, protected folders cannot be dragged, a folder cannot
  go into itself, and a name that is taken becomes "name 2". The place under
  the pointer is outlined while dragging. Not in the Trash.
- The places and the Back, Up, New Folder and Restore buttons carry Phosphor
  Bold glyphs; a glyph that is missing from `ui/icons/phosphor/bold` is shown
  as words instead.
- The status line says how many items, how many are selected and how much
  space is free; a notice for what just happened replaces it until the next
  click or key.

## What it will not do

Files looks anywhere that can be read, but only changes things inside the home
folder. Making, renaming, cutting, trashing, pasting, restoring and emptying
the Trash are refused elsewhere, with a line that says so. The home folder
itself, Desktop, Documents, Downloads, Music, Pictures, Public, Templates,
Videos, `.config`, `.local` and `.local/share` cannot be moved, renamed or
trashed. Paths are judged after links in their folder are resolved, so a link
in home that points at `/etc` does not make `/etc` changeable.

Not in this version: thumbnails, tabs, searching, mounting disks or USB sticks,
dragging to another window, and a Forward button (the keys work).

## Speed

A folder is read on a thread of its own and sorted there; the window shows
"Reading folder… 4500 items" meanwhile. The window is given only the rows that
are on screen, about twenty, so a folder of ten thousand files draws as fast
as one of ten. Pasting and emptying the Trash also run on a thread. The window
cannot be closed while one of them is working.

## Code

- `fsops.rs`: reading, sorting, naming, copying and moving.
- `guard.rs`: what may be changed.
- `jobs.rs`: paste, trash, restore and empty, with what to say afterwards.
- `session.rs`: the place, history, entries, selection, clipboard and path bar.
- `menu.rs`: what the right-click menu offers.
- `watch.rs`: the inotify watcher, one thread, with a 150 ms settle.
- `open.rs`: `xdg-open` and the command line argument.
- `app.rs`: the window, wired to all of that.
- `ui/files.slint`: the window, from the shared components and Settings' kit.
- `tests/window.rs`: the real window driven by keys over a made-up home
  folder; with `SENGI_RENDER_DIR` set it also writes PNGs of it.
