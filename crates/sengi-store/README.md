# sengi-store

The Sengi OS app store: an ordinary window with a search field on top. Under
it are a tile for each category and a grid of featured apps from Flathub.

    sengi-store               open the Store
    sengi-store --search TEXT open it with TEXT already searched for

It is not a service. It starts when asked for, exits when its window is
closed, and leaves nothing running. The window's application id is
`sengi-store`, which is also the name of the `.desktop` file in this
directory; the image installs that as
`/usr/share/applications/sengi-store.desktop`, and the dock pins it after
Settings.

## Pages

- **Start**: the categories as a grid of tiles (an icon and a name), then
  Featured, the first app of each category.
- **Category**: a back button, the category's name and its apps.
- **Search**: whatever is typed searches every app of Flathub by name, from
  any page. Clearing the field goes back to the page it was typed on.

The tiles are two to five to a row, one fewer where the last would be alone.

An app is a card: its icon, its name, up to two lines about it and one
button, Install or Open. The grid has two to four columns by the width of
the window and stops growing at 1200 px.

While an app is installed its card shows the step, size and speed that
flatpak prints, with a bar and a percentage; what is not printed is not made
up, and with no percentage a bar moves to and fro instead. The end of the
output is read about four times a second at most. One app is installed at a
time. Open runs `flatpak run <id>` for the user, detached: the Store does not
wait for the app, keeps none of its pipes, and does not stop it when the
Store is closed. Nothing is installed without its button or Return.

## Keyboard

The keyboard stays in the search field and a ring marks the chosen tile or
card.

| Key | Does |
| --- | --- |
| Down, Up | into the grids and between their rows; Up from the first row returns to the field |
| Left, Right | along the grid, once in it or with the field empty |
| Tab, Shift+Tab | the next and the previous tile or card |
| Return | opens the category, or installs or opens the app |
| Escape | clears the search, then goes back from a category |
| Alt+Left, Backspace in an empty field | back |

With something typed the best result is chosen already, so Return installs it.

## Featured

Sengi OS ships no browser, office suite or media player, so the Store
recommends some. They are listed in [`featured.toml`](featured.toml) by
category: a `[category]` line, an `icon = "name"` line naming a Phosphor Bold
icon of `ui/icons/phosphor/bold` for its tile, then one
`"flathub.application.Id" = "Name"` line for each app. The first app of a
category is the featured one. The tests check that every ID is valid and used
once, that no category is empty and that every icon is there.

The name and the summary of a recommended app are Flathub's. An ID that
Flathub does not list is left out, so the Store never offers an app that
cannot be installed. An app is also found by the last part of its ID when its
name does not have the word: `gimp` finds GIMP, which Flathub names "GNU
Image Manipulation Program". This is how the Welcome wizard's suggestions
(`sengi-store --search firefox`, `libreoffice`, `vlc`, `gimp`) find their
apps.

## Icons

No app's logo is part of Sengi OS. Flatpak unpacks the icons of the remote's
apps next to its appstream data, in
`$XDG_DATA_HOME/flatpak/appstream/flathub/<arch>/active/icons/128x128` (and
`64x64`). They are decoded, at the size the screen draws them, by
[sengi-apps](../sengi-apps/README.md)'s icon decoder, which runs this program
again as a short-lived child, so `main` hands its arguments to
`icons::run_decoder` first.

Most pictures are named after the app's ID. Some are named after the app's
entry in the appstream data instead (`org.kde.okular.desktop.png` for
`org.kde.okular`, about 150 apps of Flathub), so when one is not there the
names are read from `active/appstream.xml`, once for each version of the data.

Until the data is there a card has a placeholder, the glyph of its category
or the initial of its name. `active` is a link to a folder named after the
data's version: when it changes, whoever fetched it, what was missing is
looked for again and the pictures appear. A file that is there but does not
decode in time is tried three times.

## Without a network

Without Flathub and without a list from an earlier time, the categories and
the recommended names are shown without buttons, under a banner such as
"You're offline. Connect to the internet to browse apps." with a Retry
button. With an earlier list, that list is shown and the banner says so. What
flatpak said is translated into plain words (offline, too slow, no secure
connection, trouble at Flathub, disk full) and the original goes to the log
(standard error).

While the list could not be had, the Store looks every 3 seconds at the
kernel's routing tables (`/proc/net/route` and `ipv6_route`; no traffic, and
nothing that depends on what set up the network). A default route that
appears or changes is a reason to try again at once; one that stays is tried
again after 5, 15, 30 and 60 seconds and then every 2 minutes. Typing or
opening a category also tries again, no sooner than every 30 seconds.

## What it runs

Everything goes through the `flatpak` command, for the user's own
installation, so there is no root, no polkit and no network code of our
own. Each call has a time limit, and a call that goes over it is killed
with whatever it started. A limit is how long flatpak may say nothing, so a big install that keeps printing is not cut off. Closing the window kills what is still running: Flatpak deploys an app only when all of it has arrived, so a cut-off install leaves nothing half installed and the next try carries on.

Installs use `-y`: no question is asked and no permission list is shown. The Store also checks that the user's `flathub` remote is the real one, with signatures checked, and refuses to go on if it is not. Before an install it asks again what is installed, so an app that is there already, for the user or the system, is not installed twice.

The Store does not stop a second copy of itself from opening.

| When | Command | Limit |
| --- | --- | --- |
| Start | `flatpak --user remote-add --if-not-exists flathub /etc/flatpak/remotes.d/flathub.flatpakrepo` | 20 s |
| Start | `flatpak list --app --columns=application` | 20 s |
| Start | `flatpak --user remote-ls --cached --app --columns=application,name,description flathub` | 60 s |
| Start | `flatpak --user update --appstream`, then `remote-ls` again without `--cached` | 10 min, 60 s |
| Install | `flatpak --user install -y --noninteractive flathub app/<id>` | 10 min without a word from flatpak |

`update --appstream` says one line and then nothing while it fetches some
thousands of icons, so its limit is long; without a network it fails at once.
The list on the disk is shown first, so the Store is usable while the
network is asked, and offline it still searches what Flatpak fetched the
last time. The list is kept in memory for as long as the window is open.

## Where installed apps show up

Flatpak's exports, `~/.local/share/flatpak/exports/share`, are among the
directories sengi-apps reads desktop files and icons from, whether or not
the session's `XDG_DATA_DIRS` names them, so a new app is in the launcher
and can be pinned to the dock without logging in again.

## Tests

The unit tests cover reading Flatpak's output, matching names, the pages, the
keys in the grids, the plain words for errors, reading the routing tables,
the icon lookup against a copy of Flatpak's appstream layout, and the time
limits. They run a fake `flatpak` script, written by the test, in place of
the real one. `crates/sengi-shots` draws the pages and checks the keys.
