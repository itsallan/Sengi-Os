# sengi-store

The Sengi OS app store: an ordinary window with a search field on top and,
under it, the apps of Flathub whose names match what was typed. Each has its
icon, its name and a line about it, and an Install button.

    sengi-store               open the Store
    sengi-store --search TEXT open it with TEXT already searched for

Type a name. Up and Down choose a row, Return installs it, Escape clears
the field. Only the app's name is searched, without regard to case; every
word typed has to be in it, and a name that starts with what was typed
comes first. The line about an app is shown but not searched. A row says
Installing while it is being installed. The end of what flatpak prints is
read (about four times a second at most) for a percentage, a step such as
"2/4", a size and a speed; what is found is shown as a bar with the
percentage and "Installing 2 of 4", and what is not found is not shown or
made up: with no percentage the bar moves to and fro instead. And an Open button
afterwards; an app that is installed already has Open from the start. Open
(or Return on the row) runs `flatpak run <id>` for the user, detached: the
Store does not wait for the app, keeps none of its pipes, and does not stop
it when the Store is closed. One app is
installed at a time. What went wrong is said in the line under the list,
and the button then reads "Try again".

It is not a service. It starts when asked for, exits when its window is
closed, and leaves nothing running. The window's application id is
`sengi-store`, which is also the name of the `.desktop` file in this
directory; the image installs that as
`/usr/share/applications/sengi-store.desktop`, and the dock pins it after
Settings.

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
| Start | `flatpak --user update --appstream`, then `remote-ls` again without `--cached` | 120 s, 60 s |
| Install | `flatpak --user install -y --noninteractive flathub app/<id>` | 10 min without a word from flatpak |

The list on the disk is shown at once, so the Store is usable while the
network is asked, and offline it still searches what Flatpak fetched the
last time. With no list at all, the window says that Flathub cannot be
reached and tries again when something is typed. The list is kept in
memory for as long as the window is open.

## Icons

Flatpak unpacks the icons of the remote's apps next to its appstream data,
in `$XDG_DATA_HOME/flatpak/appstream/flathub/<arch>/active/icons/64x64/<id>.png`
(and `128x128`). They are decoded by [sengi-apps](../sengi-apps/README.md)'s
icon decoder, which runs this program again as a short-lived child, so
`main` hands its arguments to `icons::run_decoder` first. An app with no
icon gets a tile with its initial, as in the dock.

## Where installed apps show up

Flatpak's exports, `~/.local/share/flatpak/exports/share`, are among the
directories sengi-apps reads desktop files and icons from, whether or not
the session's `XDG_DATA_DIRS` names them, so a new app is in the launcher
and can be pinned to the dock without logging in again.

## Tests

The unit tests cover reading Flatpak's output, matching names, the state
of each row, the icon lookup, and the time limits. They run a fake
`flatpak` script, written by the test, in place of the real one.
