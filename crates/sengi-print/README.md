# sengi-print

Printing for Sengi OS, from Settings, Printers. CUPS does the printing; this
crate is what Settings uses to ask it, and the one small helper that needs
root.

    sengi-print add NAME URI    add a driverless printer (needs root)
    sengi-print remove NAME     remove a printer (needs root)
    sengi-print discover        list printers on the network

Nothing else is accepted: no options, no other command, and any argument
that does not match the patterns below is refused before anything runs.
The crate is also a library, which Settings links: it asks CUPS as the user,
runs the helper through `pkexec`, and reads what they print.

It is not a service. Nothing runs while Settings is closed, there is no timer
and no monitor. CUPS is asked when the Printers page comes into view and when
Refresh is pressed.

## Why CUPS is not in the image

CUPS is installed on demand, from Settings, Printers ("Install printing
support"), and not carried by the ISO. The numbers, measured in the builder VM
(a Debian 13 cloud image, which already has the libraries a desktop has, so
they are upper bounds for the Sengi OS image):

| What is installed | Packages | Installed | Download |
|---|---|---|---|
| `cups` (with `cups-client`) | 40 | 87 MB | 25 MB |
| the above and `avahi-daemon` (what the image installs on demand) | 43 | about 88 MB | about 26 MB |
| `cups-core-drivers` + `cups-client` | 21 | 23 MB | 7 MB |
| `cups-daemon`, `cups-client`, `cups-common` only | 7 | 6 MB | 1 MB |

The limit was about 60 MB, and `cups` is over it: it depends on `cups-filters`,
which depends on Ghostscript, and Ghostscript, its fonts and the Poppler data
are 51 MB of the 87. The smaller sets were tried with a fake IPP Everywhere
printer (`ippeveprinter`, from the same packages) and do not do the job:

* `cups-core-drivers` has the daemon and the filters that turn a PDF into
  what a printer takes, but `cups-deviced`, `cups-driverd`, `rastertopwg` and
  the `dnssd`, `socket` and `lpd` backends are in the `cups` package itself.
  Without them `lpinfo -v` fails ("cups-deviced failed to execute"), so the
  network cannot be searched, and a printer that takes only raster
  (`image/pwg-raster`, `image/urf`) refuses the PDF that apps send
  (`Unsupported document-format "application/pdf"`).
* The 6 MB set has no filters, so no PDF at all.

Resident cost: Debian enables `cups.service` for `multi-user.target`, which
keeps cupsd in memory (8.4 MB resident in the VM) from boot. That is not
acceptable, so after the install the helper disables it and enables
`cups.socket` and `cups.path`, which start cupsd when something asks (see
`crates/sengi-update`, "Installing a package"). In the measurements here,
once cupsd has been asked it did not leave on its own after its 60 second
`IdleExitTimeout`, even with sharing, browsing and the network port off; it
stays about 8.4 MB until the next boot. `cups-browsed` is not installed (it is
only a Recommends, and the install does not take Recommends), so nothing adds
printers in the background.

Finding printers on the network is `lpinfo -v`, whose `dnssd` backend asks
Avahi over the bus. Without Avahi it lists only the bare scheme `network ipp`
and no printer (seen in the VM), so Avahi is installed with CUPS, but not
started at boot: `systemctl enable` links the alias the bus starts it by, and
the helper takes away the one link that would start it at boot. The first
search starts it (seen in the VM: 2.8 MB and 1.2 MB resident for its two
processes) and it stays until the next boot, announcing this computer's name
on the network while it runs.

The install is `pkexec /usr/bin/sengi-update install-package cups` (an
allow-list of one name, which installs `cups`, `cups-client` and `avahi-daemon`; not on the live system, not on a low battery, not while
apt runs), with its own password dialog and a bar that does not claim to know
how far it is. If Allan would rather the ISO carry the 23 MB set (printing to
printers that take PDF, typed addresses only), the lines are
`cups-client` and `cups-core-drivers` in `image/packages.list` plus the same
`systemctl` and `cupsd.conf` steps in `image/customize.sh`; the raster-only
printers and the network search would still need the install.

## Privilege

CUPS lets only `root` and the `lpadmin` group add or remove a printer or list
devices. The first user is not in `lpadmin`, so those three things go through
pkexec, under the polkit action `org.sengi.os.printer`
(`image/rootfs/usr/share/polkit-1/actions/`): `auth_admin_keep` for the active
local session, `no` for everyone else, and a rule
(`image/rootfs/etc/polkit-1/rules.d/50-sengi-printer.rules`) that makes the
signed-in person the one who authenticates, with their own password, if they
may use sudo. The answer is kept a few minutes, so searching and then adding
asks once. The password is asked in a dialog in Settings and given to pkexec's
own text agent through a pseudo-terminal, exactly as for updates; see
`crates/sengi-update` (`elevate.rs` is the part this crate shares). There is
no setuid binary of ours and no stored password.

Installing CUPS goes through the update helper and the update's polkit action
(see above and `crates/sengi-update`).

Everything else is done as the user, with no password: `lpstat` (printers,
default, queue), `lpoptions -d` (the default printer, written to the user's own
`~/.cups/lpoptions`), `lp` (the test page) and `cancel`.

## What the helper runs

As root, with an environment of its own (`PATH`, `LC_ALL=C`) and the programs
by absolute path:

* `add NAME URI`: `/usr/sbin/lpadmin -p NAME -E -v URI -m everywhere
  -o printer-is-shared=false`. A driverless (IPP Everywhere) queue that is
  switched on and not shared. It refuses a name that is in use (`lpstat -p
  NAME`). When CUPS cannot ask the printer what it can do, `lpadmin` has
  already made a queue, as a raw one, and leaves it (seen with a printer that
  was not there), so the helper takes it away again with `lpadmin -x NAME`.
* `remove NAME`: `/usr/sbin/lpadmin -x NAME`.
* `discover`: `/usr/sbin/lpinfo --include-schemes dnssd,ipp -v`, for at most
  20 seconds, printing the lines that name a printer. It stops early when its
  standard input is closed or says `cancel`, which is how Settings stops it,
  since a program that is not root cannot signal one that is.

The arguments are checked again as root (`valid.rs`):

* A printer name starts with a letter or digit and holds letters, digits,
  `.`, `_`, `+` and `-`, at most 127 characters. It can never be read as an
  option, a path or two arguments.
* A device address is at most 512 characters of `A-Z a-z 0-9 . _ ~ % : / ? = &
  -`, with a scheme of `ipp`, `ipps`, `dnssd`, `socket` or `lpd`, followed by a
  host or an encoded name. No `file:`, no `usb:`, no user name or password, no
  quote, space or shell character.
* What a person types in Settings is a name or an IPv4 address with an optional
  port, which becomes `ipp://HOST/ipp/print`, or a whole `ipp://`, `ipps://`,
  `socket://` or `lpd://` address. Queue names are made from what the printer
  calls itself, reduced to the allowed characters and made different from the
  queues there are.

## The test page

`/usr/share/sengi/printing/testpage.pdf`, a one page A4 PDF of ours (text in
four sizes, grey steps, lines, a border to see the margins). CUPS's own test
page is PostScript, and turning that into something a printer takes needs
Ghostscript, so a PDF, which needs nothing, is the one used. The image carries
the file whether or not CUPS is installed.

## Tests

`cargo test -p sengi-print`: the patterns (every unsafe name, address and job
number tried), the parsing of `lpstat -d -p -v`, `lpstat -o` and `lpinfo -v`
against samples of their real output, the helper's exact arguments, its refusal
of every unsafe or surplus argument (nothing is run), a search that is stopped
and one that takes too long, and the client against fake `lpstat`, `lpoptions`,
`lp`, `cancel` and `pkexec` found on a `PATH` of their own, including the
install through a fake update helper. The samples of `lpstat` output in the
parser's tests were taken from CUPS 2.4.10 on Debian 13 with a queue pointing
at `ippeveprinter`. What no test here can show is a real printer: see
"Not verified" in the report that came with this change.
