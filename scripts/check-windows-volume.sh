#!/bin/sh
# Whether an NTFS volume still holds, byte for byte, everything that
# scripts/make-windows-volume.sh put in it, and whether it is consistent.
# For the install test, which reads the disk from outside the guest.
#
#   check-windows-volume.sh VOLUME FIXTUREDIR
#
# VOLUME is a file that holds the volume (the partition cut out of a disk
# image, at the size of the partition it is meant for) and is only read. Needs no root: the files are read with ntfscat,
# without mounting. Prints a line for each thing checked and fails if any
# of them is wrong.
set -eu

volume=${1:?usage: check-windows-volume.sh VOLUME FIXTUREDIR}
fixture=${2:?usage: check-windows-volume.sh VOLUME FIXTUREDIR}
for tool in ntfscat ntfsresize ntfsfix sha256sum; do
	command -v "$tool" >/dev/null 2>&1 || { echo "FAIL: $tool is missing" >&2; exit 2; }
done
[ -f "$volume" ] || { echo "FAIL: $volume is not a file" >&2; exit 2; }
export LC_ALL=C.UTF-8
work=$(mktemp -d "${TMPDIR:-/tmp}/sengi-ntfs.XXXXXX")
trap 'rm -rf "$work"' EXIT
trap 'exit 1' INT TERM
failed=0
fail() {
	echo "FAIL: $1"
	failed=1
}

# The checks of ntfsresize and ntfsfix refuse a volume that is marked for a
# disk check, which a resized one is, on purpose. The mark is cleared on a
# copy, for the checking only, and the checks run on that copy.
cp --reflink=auto "$volume" "$work/copy.ntfs"
if ntfsfix --clear-dirty "$work/copy.ntfs" >"$work/clear" 2>&1; then
	:
else
	fail "ntfsfix could not open the volume: $(tail -3 "$work/clear" | tr '\n' ' ')"
fi
if ntfsresize --info --no-progress-bar "$work/copy.ntfs" >"$work/info" 2>&1 &&
	grep -q '^You might resize at' "$work/info"; then
	echo "ok: ntfsresize checks the whole volume and finds it consistent"
else
	fail "ntfsresize does not find the volume consistent: $(tail -4 "$work/info" | tr '\n' ' ')"
fi
# (ntfsresize --check ends well whatever it finds once it has opened the
# volume; the check above is the thorough one.)
if ntfsresize --check --no-progress-bar "$work/copy.ntfs" >"$work/check" 2>&1; then
	echo "ok: ntfsresize --check passes"
else
	fail "ntfsresize --check: $(tail -4 "$work/check" | tr '\n' ' ')"
fi
# Among the rest, it looks for the copy of the first sector in the last
# sector of what it is given: VOLUME is to be the size of the partition the
# volume is meant for.
if ntfsfix --no-action "$work/copy.ntfs" >"$work/fix" 2>&1; then
	echo "ok: ntfsfix --no-action passes"
else
	fail "ntfsfix --no-action: $(tail -4 "$work/fix" | tr '\n' ' ')"
fi
rm -f "$work/copy.ntfs"

# Every file, read from the volume as it is.
files=0
wrong=0
while IFS= read -r line; do
	want=${line%% *}
	path=${line#*  ./}
	files=$((files + 1))
	if ntfscat -f "$volume" "$path" >"$work/file" 2>"$work/err" &&
		[ "$(sha256sum <"$work/file" | cut -d' ' -f1)" = "$want" ]; then
		:
	else
		wrong=$((wrong + 1))
		[ "$wrong" -gt 10 ] || echo "FAIL: $path is not as it was: $(head -1 "$work/err")"
	fi
done <"$fixture/manifest.sha256"
if [ "$files" -gt 0 ] && [ "$wrong" = 0 ]; then
	echo "ok: all $files files are byte for byte as they were"
else
	fail "$wrong of $files files are not as they were"
fi

streams=0
tab=$(printf '\t')
while IFS=$tab read -r path name want; do
	streams=$((streams + 1))
	if ntfscat -f -n "$name" "$volume" "$path" >"$work/file" 2>"$work/err" &&
		[ "$(sha256sum <"$work/file" | cut -d' ' -f1)" = "$want" ]; then
		echo "ok: the data stream $name of $path is as it was"
	else
		fail "the data stream $name of $path is not as it was: $(head -1 "$work/err")"
	fi
done <"$fixture/manifest.streams"
[ "$streams" -gt 0 ] || fail "the list of data streams is empty"
exit $failed
