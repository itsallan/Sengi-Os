#!/bin/sh
# Print release notes for a version: its CHANGELOG.md section plus the
# measurements from the boot test.
#
#   release-notes.sh VERSION METRICS_FILE
set -eu

version=${1:?usage: release-notes.sh VERSION METRICS_FILE}
metrics=${2:?usage: release-notes.sh VERSION METRICS_FILE}
root=$(cd "$(dirname "$0")/.." && pwd)

value() { sed -n "s/^$1=//p" "$metrics"; }

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

### Try it

Check the download with \`sha256sum -c sengi-os-$version-amd64.iso.sha256\`,
flash the ISO to a USB stick and boot it. The live user is \`live\` with
password \`live\`.
NOTES
