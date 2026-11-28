# sengi-dock

The Sengi OS dock: a centred panel of app icons at the bottom of the screen,
drawn into a wlr-layer-shell surface. The search button comes first, then the
pinned apps, then any other running apps.

## Pinned apps

The pinned list is `pinned = [...]` in `sengi-os/dock.toml`: desktop-file IDs,
in dock order. The user's file (`~/.config/sengi-os/dock.toml`) replaces the
system one (`/etc/xdg/sengi-os/dock.toml`); with neither, the dock pins the
terminal (`foot`), as it always has. An empty list pins nothing.

The dock reads the file at start, and again when another program changes it,
and writes the user's file whenever you pin, unpin or reorder. It replaces
only the `pinned` entry and keeps comments and other keys.

A pinned app that is not installed has no tile, but stays in the list. When an
app is removed its tile goes within two seconds, and it is back in its place
when the app is installed again. For that the dock also looks, every two
seconds, at when each `applications` directory last changed (one `stat` each)
and reads the apps again only if one has. The file is written under another name and renamed, so a crash
cannot leave half a list. A user file the dock cannot read is kept as
`dock.toml.bak` before it is replaced. If the file cannot be written (no
home, read-only) the change still takes effect until the dock exits, and an
error goes to standard error.

The list lives in `dock.toml` and not in `settings.toml` because the dock has
always read its own file, and an existing hand-edited list keeps working.

## Right-click menu

Right-click a tile (not the search button) for a small menu above it:

- Open: starts an app that is not running.
- Keep in Dock / Remove from Dock: pins an app after the others, or unpins
  it. An app that is still running stays after the pinned ones until it
  quits, with its dot, as before. Windows that match no installed app cannot
  be pinned.
- Quit: closes all of the app's windows, which lets the app ask to save. It
  does not kill anything.

Pinning and unpinning take effect at once. The menu is drawn inside the
dock's own surface, in the room above the panel that the name label uses, so
there is no second surface. The surface only takes the pointer over the panel
and, while open, the menu. The menu closes when you choose a row, click a
tile, right-click the same tile again, or move the pointer away from it.

The launcher has the same two rows on its own right-click menu ("Keep in
Dock", "Remove from Dock"). It writes `dock.toml` itself; the dock looks at
the file's modification time every two seconds (one `stat`) and takes the
new list in when another program has changed it.

## Dragging

Drag a pinned tile left or right to reorder. A line marks where it will
land and a copy of the tile follows the pointer; letting go saves the new
order to `dock.toml`, the same way pinning does. Only pinned tiles can be
dragged, and only pinned tiles are places to drop on: the search button
stays first, and an app that is only running stays after the pinned ones.
Dragging past either end of the pinned tiles puts the tile first or last
among them.

Drag a pinned tile up out of the dock, until a "Remove" label shows above
it, and let go to take it out of the dock, as "Remove from Dock" does. An app
that is still running stays as a running tile. Letting go anywhere else, or
a click that did not move, changes nothing.

While a drag is on, the surface takes the pointer over its whole area, not
just the panel, and goes back to the panel afterwards. The drag does not
rely on a pointer grab reaching beyond the surface: the room above the
panel (the same room the name label and the menu use) is where the tile can
be taken to. If the pointer is taken away mid-drag the drag is cancelled.

## Code

- `model.rs`: items, click rules, pin, unpin and move, the rows of the menu.
- `config.rs`: the pinned list's file. The code is in
  `sengi_apps::pins`, shared with the launcher.
- `app.rs`: joins the model to the Slint component (`ui/dock.slint`,
  `ui/menu.slint`).
- Tests: `cargo test -p sengi-dock`. The menu is drawn headless by
  `cargo test -p sengi-shots --test desktop` with `SENGI_RENDER_DIR` set; look
  for `dock-menu-1366.png`. `cargo test -p sengi-shots --test groups` drives
  the pointer through a drag and writes `dock-drag-reorder-1366.png` and
  `dock-drag-remove-1366.png`.
