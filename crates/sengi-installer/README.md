# sengi-installer

The window of the Sengi OS installer. It runs in the live session, asks
where and for whom to install, shows what that will erase, and follows the
installation. It never writes to a disk itself: that is
[sengi-install](../sengi-install/README.md), which it starts as root.

## The pages

1. **Welcome.** What will be asked, and what is not offered yet.
2. **Region.** The keyboard layout and the time zone. The layout is put
   into effect in the running session at once, the way Settings does it,
   so that the password typed two pages later is typed with the layout it
   will be typed with at the login screen. There is a field to try the
   keys in. The time zone starts at UTC.
3. **Disk.** Every disk, with its model, size and what is on it now. The
   ones that cannot be chosen are listed too, greyed out, with the reason:
   the stick Sengi OS is running from, a disk with something mounted, in
   use as swap or under another volume, read-only, or under 8 GB. Nothing
   is chosen for you. "Look again" reads the disks again.
4. **You.** Your name, a user name (offered from the name until one is
   typed), the password twice, and a name for the computer. What is wrong
   with a field is said under it once something has been typed.
5. **Review.** The disk, that all of it is erased, the account, the
   computer's name, the layout and the zone.
6. **Installing**, which becomes **Sengi OS is installed** or **The
   installation failed**. The nine steps are listed and marked as they
   run; the copy shows a percentage, counted in bytes on the new disk.
   The window cannot be closed while this runs.

## Erasing is deliberate

Nothing is changed before the Review page's button is pressed and its
question answered.

- On the Review page the keyboard starts on Back. The button that goes on,
  "Erase disk and install…", is drawn as a warning and is not what Return
  presses.
- It opens a question that names the disk and what is on it. There the
  keyboard starts on Cancel, and Escape cancels.
- The plan handed to sengi-install carries the size and model of the disk
  that was shown. If the device of that name is no longer that disk,
  sengi-install stops before touching anything.

## How it gets root

The window runs as the live user. To install, it runs

    sudo -n systemd-run --scope --quiet --collect --slice=system.slice \
        --property=ManagedOOMPreference=omit /usr/bin/sengi-install run

and writes the password, then the plan, to its standard input. The password
is never in an argument or in the environment.

`sudo -n` works because the live image lets the `live` user be root without
a password (`/etc/sudoers.d/live`). That is acceptable only there: whoever
sits at a live session can already do anything to the machine, and nothing
on the stick is theirs to protect. The installed system has no such rule,
no `live` user and no installer, and sengi-install removes all three from
what it installs. On any other system the window opens and the installation
fails at once with sudo's refusal.

The scope under `system.slice` takes the installing program out of the
window's own app scope, which systemd-oomd may end when memory is short.

## The password

Typed into the shared password field, which never holds it: the key
presses are collected in a `PasswordEntry`, the buffer the lock screen and
the login screen use, kept out of swap and overwritten when it is let go.
The two fields are compared inside that type. The process is kept out of
core dumps. When the installation starts the password is written to
sengi-install's standard input and dropped; the second copy is cleared.

Not protected: the pipe to sengi-install and that program's own copy,
which it hands to `chpasswd` the same way.

## What it reuses

- The shared components in `ui/`, including `SengiPasswordField`, and two
  that were added for it: `SengiProgress` and `SengiStep`.
- Settings' window kit (`crates/sengi-settings/ui/kit.slint`): pages,
  cards, rows, the chooser and the dialog.
- Settings' code for the list of time zones, the keyboard layouts and
  their names, and for putting a layout into effect.
- sengi-install's rules for names, its list of disks and its plan, so that
  what the window accepts is what the installing program accepts.

## Files

| Path | What |
|---|---|
| `src/main.rs` | The program: joins the pages to the model, starts sengi-install |
| `src/model.rs` | What the pages show, without the toolkit: tested as it is |
| `ui/installer.slint` | The window |
| `sengi-installer.desktop` | The entry for the launcher and the dock |

The dock pins it in the live image (`/etc/xdg/sengi-os/dock.toml`); the
installed system's dock does not have it.

## Looking at it without a disk to erase

A debug build takes `--demo`, which shows three made-up disks and plays an
installation that touches nothing, and `--demo-fail`, which plays one that
fails:

    cargo run -p sengi-installer -- --demo

A release build has neither.

## What this does not do

- Install beside another system, encrypt, or lay out partitions by hand.
- Guess the time zone or the keyboard layout.
- Offer a language: it is in English.
- Ask about the strength of the password. An empty one is refused.
- Connect to a network. Nothing is downloaded: what is installed is what
  is on the stick.
