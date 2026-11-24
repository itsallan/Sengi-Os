# sengi-store

The Sengi OS app store: an ordinary window with a search field on top and,
under it, the apps of Flathub whose names match what was typed. Each has its
icon, its name and a line about it, and an Install button.

    sengi-store               open the Store

Type a name. Up and Down choose a row, Return installs it, Escape clears
the field. Only the app's name is searched, without regard to case; every
word typed has to be in it, and a name that starts with what was typed
comes first. The line about an app is shown but not searched. A row says
Installing while it is being installed, with a bar that moves to and fro
because Flatpak does not say how far it has got, and Installed afterwards;
an app that is installed already says so from the start. One app is
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
with whatever it started.

| When | Command | Limit |
| --- | --- | --- |
| Start | `flatpak --user remote-add --if-not-exists flathub /etc/flatpak/remotes.d/flathub.flatpakrepo` | 20 s |
| Start | `flatpak list --app --columns=application` | 20 s |
| Start | `flatpak --user remote-ls --cached --app --columns=application,name,description flathub` | 60 s |
| Start | `flatpak --user update --appstream`, then `remote-ls` again without `--cached` | 120 s, 60 s |
| Install | `flatpak --user install -y --noninteractive flathub <id>` | 30 min |

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
