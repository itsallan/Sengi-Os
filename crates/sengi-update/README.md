# sengi-update

System and app updates for Sengi OS, from Settings, Updates.

    sengi-update check     list what could be updated, one JSON line each
    sengi-update apply     update the system's packages (needs root)
    sengi-update status    say what would stop an update, as one JSON line
    sengi-update install-package cups
                           install the print system (needs root)

Nothing else is accepted: no options, and the only package name is one from
a short list built into the program (see "Installing a package" below).
Any other argument is refused before anything runs. The crate is also a library, which
Settings links: it looks for updates as the user, runs the helper through
`pkexec`, runs Flatpak, and reads what they print.

It is not a service. Nothing runs while Settings is closed, there is no timer
and no daemon. Settings looks for updates when it starts, and only if the
last look is more than a day old (and at most once in six hours if that
failed); that look is what the badge on the Updates entry in the sidebar
shows. The result is kept in `~/.cache/sengi-os/updates`.

## What a person sees

Settings, Updates has a **Check for updates** button and the time of the last
check; the count of system packages (security ones marked) and Flatpak apps
that can be updated, with names and versions; and **Update all**. While it
runs the page shows the phase, a bar with the percentage, "Step N of M",
bytes downloaded and the tool's own words, and a **Cancel** button that is
on until the install starts. At the end it says "Up to date", or "Restart
needed" (when `/run/reboot-required` exists) with a **Restart** button, which
runs `systemctl reboot`, that is logind, which asks polkit.

On the live system the page says "Updates are available after Sengi OS is
installed" and every control is off: the root there is in memory and has no
package lists. It is the same check the About page makes (the root file
system is an overlay, tmpfs or ramfs).

## Privilege

Checking needs no root. `apt-get update` is run with lists of its own under
`~/.cache/sengi-os/apt` (`-o Dir::State::Lists=...`, no package cache files),
which any user may do, so the check, and the badge, never ask for a password.
The simulation (`apt-get -s --with-new-pkgs upgrade`) is read for the list of
packages, which of them come from a security suite, and the download size.

Installing needs root and goes through `pkexec /usr/bin/sengi-update apply`
under the polkit action `org.sengi.os.update`
(`image/rootfs/usr/share/polkit-1/actions/`). It is `auth_admin_keep` for the
active local session, `no` for everyone else, and a rule
(`image/rootfs/etc/polkit-1/rules.d/50-sengi-update.rules`) makes the person
who is signed in the one who authenticates, with their own password, if they
may use sudo. There is no setuid binary of ours, no stored password and no
sudoers entry.

### Who asks for the password

Polkit needs an agent to ask, and the image has none: no GNOME and no Qt are
allowed, and any agent would be a program running all the time for something
used now and then. A tiny agent of our own was weighed and left out: it
would need the agent side of polkit's D-Bus interface, a call to its setuid
`polkit-agent-helper-1` with its own protocol, and a prompt window, and it
would have to run for the whole session to catch other programs' requests.
`pkexec` has a text agent of its own that reads the password from its
controlling terminal, so Settings gives it one: a pseudo-terminal with echo
off. When pkexec prints "Password:" there, Settings opens its own dialog and
writes the answer to the terminal once. The password is never in an argument
or the environment, never logged, held in a buffer that is overwritten when
dropped (the dialog's own copy inside Slint cannot be, as with the Wi-Fi
password: see `sengi-system`'s `secret.rs`), and checked by polkit's helper
against PAM, so lockout and delay apply as everywhere. A second prompt means
the password was wrong and ends the attempt. The helper's output comes on a
pipe, not the terminal, so it cannot be taken for a prompt. `pkexec` is a
Debian package of its own, which the image now installs; it runs for the length of one update.

## What the helper runs

As root, in this order, with an environment of its own (`PATH`, `LC_ALL=C`,
`DEBIAN_FRONTEND=noninteractive`, `APT_LISTCHANGES_FRONTEND=none`) and
`apt-get` by absolute path:

1. refuse: on the live system; on battery below 20% (`/sys/class/power_supply`,
   batteries of devices such as a mouse are ignored); while another apt or
   dpkg holds one of its lock files (`fcntl` locks, asked with `F_GETLK`);
2. `apt-get update`;
3. `apt-get -s --with-new-pkgs upgrade`, and refuse if it would **remove**
   anything;
4. `apt-get -y -d --with-new-pkgs --no-remove upgrade`: the download, which
   can be cancelled;
5. `apt-get -y --with-new-pkgs --no-remove -o Dpkg::Options::=--force-confdef
   -o Dpkg::Options::=--force-confold upgrade`: the install.

No `dist-upgrade`, no `full-upgrade`, no removal, no edit of the sources:
the release does not change. Packages named `sengi-*` are not held: the
programs of Sengi OS are not Debian packages, they come with the ISO.

**Cancel.** Settings writes `cancel` on the helper's standard input (the
user cannot signal a root process). The helper acts on it, or on SIGTERM,
until step 5 begins, and then not at all: from there it ignores SIGHUP,
SIGINT and SIGTERM, so closing the terminal or Settings does not cut dpkg
off. Closing the pipe is not a cancel. Settings does not even offer Cancel
once the install phase has started.

**Progress** is read from apt's own machine-readable status descriptor
(`-o APT::Status-Fd=3`): `dlstatus:N:PERCENT:Retrieving file I of M` while
downloading and `pmstatus:PACKAGE:PERCENT:message` while installing
(see apt's `doc/progress-reporting.md`). Downloaded bytes are derived from the
percentage and the "Need to get" total of the simulation, and the page says
"12 MB of 36 MB".

Apps are updated as the user with `flatpak --user update -y`; the list comes
from `flatpak --user remote-ls --updates`. Not `--noninteractive`: with it
Flatpak names each step and nothing more. Their progress is read from its
standard output by `sengi_text::progress`, as in the Store: the step, the
percentage of that step, the most it downloads, the speed and the time left.
Flatpak does not print how much has arrived, so no such number is shown, and
a missing percentage means no bar.

## Installing a package

`install-package NAME` is for the few things the image leaves out because they
are large and not everyone wants them. It takes a name and looks it up in a
list in `helper.rs` (`INSTALLABLE`); a name that is not on it, a second name, a
list, an option, another case, are all refused with the usage before anything
runs, and what is given to apt-get is what the list says for the name, never
the caller's text. The list has one entry: `cups` installs `cups`,
`cups-client` and `avahi-daemon`, which is the print system and what it finds
network printers through (Settings, Printers offers it; see
`crates/sengi-print`).

It does what an update does, with the same checks (not on the live system, not
on a low battery, not while apt or dpkg runs): `apt-get update`, then
`apt-get -y -d --no-install-recommends --no-remove install PACKAGES`, which
can be cancelled, then the same without `-d`, which cannot. Nothing is
removed, Recommends are not installed, and the files in `/etc` that are
changed on the system are kept. Then, for `cups`, what the image would have
done had it carried CUPS: Debian enables `cups.service` for boot, which keeps
cupsd in memory from the first second, so it is disabled and its socket is
enabled instead (cupsd starts when something asks; its path unit stays off,
because that starts cupsd at every boot once cupsd has run), and
`/etc/cups/cupsd.conf` is changed to listen on no network port, not announce
or look for printers, and serve no web page. Avahi, which CUPS asks over the
bus to find printers on the network and which would otherwise announce this
computer on every network at boot, is enabled and then has the one link that
starts it at boot removed: what stays is the alias the bus starts it by, so
the first search starts it, and it stays up until the next boot. It uses the
update's polkit action and its password.

## Tests

`cargo test -p sengi-update`: the apt and Flatpak formats against samples
written from their documentation; the JSON lines both ways; every refusal
(live, battery, another apt holding a real `fcntl` lock, a removal in the
plan, not root); cancel before the install and no cancel in it; the order and
the exact arguments of every command the helper runs, with a fake `apt-get`;
the whole pkexec path with a fake `pkexec` that asks for a password on its
controlling terminal, including the right, the wrong and no password; Flatpak
with a fake `flatpak`; `install-package`: every name that is not on the list
and every malformed argument list (nothing is run), the exact commands with a
fake `apt-get` and `systemctl`, the cups configuration before and after, a
failed download that sets nothing up, and the live system. `elevate.rs`, the
part of the pkexec path that Settings, Printers shares, is tested with the
same fake `pkexec`. The fakes are shell scripts in a temporary directory.
The helper only takes a made-up system (`SENGI_UPDATE_TEST_DIR`) when it is
not root.

Settings' pictures of the page (idle, checking, list, 45% progress, restart
needed, failed, live) are in `sengi-shots`: `SENGI_RENDER_DIR=out cargo test
-p sengi-shots --test settings`.

## What these tests cannot show

Only a real installed Sengi OS with a network shows: that Debian's `apt-get`
prints the status lines exactly as documented; that `pkexec` finds no agent
and prompts on the pseudo-terminal as described (it prints
`==== AUTHENTICATING FOR org.sengi.os.update ====` and `Password:`); that
polkit takes the rule and the action file; that `auth_admin_keep` is kept
between two updates; that a real `apt-get update` works as a user with lists
of its own; how Flatpak prints progress when it is not on a terminal; and
that the install survives Settings being closed.
