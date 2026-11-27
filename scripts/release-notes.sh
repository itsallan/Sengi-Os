#!/bin/sh
# Print release notes for a version: its CHANGELOG.md section plus the
# measurements from the boot test and, if given, from the install test
# (one file for each firmware it was run under, the one to lead with first).
#
#   release-notes.sh VERSION METRICS_FILE [INSTALL_METRICS_FILE [INSTALL_METRICS_FILE]]
set -eu

version=${1:?usage: release-notes.sh VERSION METRICS_FILE [INSTALL_METRICS_FILE [INSTALL_METRICS_FILE]]}
metrics=${2:?usage: release-notes.sh VERSION METRICS_FILE [INSTALL_METRICS_FILE [INSTALL_METRICS_FILE]]}
installed=${3:-}
also=${4:-}
root=$(cd "$(dirname "$0")/.." && pwd)

value() { sed -n "s/^$1=//p" "$metrics"; }
installed_value() { sed -n "s/^$1=//p" "$installed"; }

awk -v v="$version" '
	/^## / { on = ($2 == v || $2 == "[" v "]") }
	on && !/^## / { print }
' "$root/CHANGELOG.md"

cat <<NOTES

### Measured

| | Idle RAM | Boot |
|---|---|---|
| Live | $(value idle_ram_mb) MB | $(value boot_s) s |
NOTES

# One row for each install test, the one that matters most first: the release
# gives UEFI there, the target machine's way.
for installed in "$installed" "$also"; do
	[ -n "$installed" ] || continue
	echo "| Installed, $(installed_value install_firmware) | $(installed_value installed_idle_ram_mb) MB | $(installed_value installed_boot_s) s |"
done

cat <<NOTES

Budget: under 400 MB and 10 s. Measured in QEMU with 2 cores and 4 GB of RAM.

### Install

Check the download with \`sha256sum -c sengi-os-$version-amd64.iso.sha256\`,
write it to a USB stick and boot. The live user and password are \`live\`.
NOTES
