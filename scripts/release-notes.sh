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

section=$(awk -v v="$version" '
	/^## / { on = ($2 == v || $2 == "[" v "]") }
	on && !/^## / { print }
' "$root/CHANGELOG.md")

# Pictures of what changed are shown in the notes, not attached as files. The
# changelog links them under docs/, and the notes need the address they have
# at the release's tag.
printf '%s\n' "$section" | grep -o '](docs/[^)]*' | cut -c3- | while read -r image; do
	[ -f "$root/$image" ] || {
		echo "release-notes.sh: $image is not in the repository" >&2
		exit 1
	}
done
files=https://raw.githubusercontent.com/${GITHUB_REPOSITORY:-itsallan/Sengi-Os}/v$version
printf '%s\n' "$section" | sed "s#](docs/#]($files/docs/#g"

cat <<NOTES

### Measured

| | Idle RAM | Boot |
|---|---|---|
| Live | $(value idle_ram_mb) MB | $(value boot_s) s |
NOTES

# The live image starts with a question on screen; what is used then too.
if [ -n "$(value live_dialog_ram_mb)" ]; then
	echo "| Live, before the install question is answered | $(value live_dialog_ram_mb) MB | |"
fi

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
