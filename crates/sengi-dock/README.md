# sengi-dock

The Sengi OS dock: a centred panel of app icons at the bottom of the screen,
drawn into a wlr-layer-shell surface. The search button comes first, then the
pinned apps, then any other running apps.

## Pinned apps

The pinned list is `pinned = [...]` in `sengi-os/dock.toml`: desktop-file IDs,
in dock order. The user's file (`~/.config/sengi-os/dock.toml`) replaces the
system one (`/etc/xdg/sengi-os/dock.toml`); with neither, the dock pins the
terminal (`foot`), as it always has. An empty list pins nothing.

The dock reads the file once, at start, and writes the user's file whenever
you pin or unpin. It replaces only the `pinned` entry and keeps comments and
other keys. The file is written under another name and renamed, so a crash
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
tile, right-click the same tile again, or move the pointer away from it. The
launcher has no context menu on its apps, so "Keep in Dock" is offered only
here.

Dragging tiles to reorder them is not implemented. New pins go to the end;
edit `dock.toml` to change the order.

## Code

- `model.rs`: items, click rules, pin and unpin, the rows of the menu.
- `config.rs`: reading and writing `dock.toml`.
- `app.rs`: joins the model to the Slint component (`ui/dock.slint`,
  `ui/menu.slint`).
- Tests: `cargo test -p sengi-dock`. The menu is drawn headless by
  `cargo test -p sengi-shots --test desktop` with `SENGI_RENDER_DIR` set; look
  for `dock-menu-1366.png`.
