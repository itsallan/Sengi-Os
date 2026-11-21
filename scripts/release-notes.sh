#!/bin/sh
# Print release notes for a version: its CHANGELOG.md section plus the
# measurements from the boot test and, if given, from the install test.
#
#   release-notes.sh VERSION METRICS_FILE [INSTALL_METRICS_FILE]
set -eu

version=${1:?usage: release-notes.sh VERSION METRICS_FILE [INSTALL_METRICS_FILE]}
metrics=${2:?usage: release-notes.sh VERSION METRICS_FILE [INSTALL_METRICS_FILE]}
installed=${3:-}
root=$(cd "$(dirname "$0")/.." && pwd)

value() { sed -n "s/^$1=//p" "$metrics"; }
installed_value() { sed -n "s/^$1=//p" "$installed"; }

awk -v v="$version" '
	/^## / { on = ($2 == v || $2 == "[" v "]") }
	on && !/^## / { print }
' "$root/CHANGELOG.md"

cat <<NOTES

### Measured

| | Result | Budget |
|---|---|---|
| Idle RAM | $(value idle_ram_mb) MB | under 400 MB |
| Power-on to desktop | $(value boot_s) s | under 10 s |

Measured in QEMU ($(value accel)), 2 cores and 4 GB RAM, booting the ISO as a disk.
NOTES

if [ -n "$installed" ]; then
	cat <<NOTES

### Measured, installed

| | Result | Budget |
|---|---|---|
| Idle RAM | $(installed_value installed_idle_ram_mb) MB | under 400 MB |
| Power-on to login screen | $(installed_value installed_boot_to_login_s) s | |
| Password to desktop | $(installed_value installed_login_to_desktop_s) s | |
| The two together | $(installed_value installed_boot_s) s | under 10 s |
| Installing | $(installed_value install_s) s | |

Installed through the installer's window onto a blank disk in QEMU
($(installed_value install_accel), $(installed_value install_firmware)), then started from that disk alone.
NOTES
fi

cat <<NOTES

### Try it

Check the download with \`sha256sum -c sengi-os-$version-amd64.iso.sha256\`,
flash the ISO to a USB stick and boot it. The live user is \`live\` with
password \`live\`. To install, open Install Sengi OS from the dock: it
erases one whole disk, which it shows you before it does.
NOTES
