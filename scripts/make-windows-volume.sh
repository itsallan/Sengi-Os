#!/bin/sh
# Make an NTFS volume that looks like a used Windows C: drive, for the part
# of scripts/install-test.sh that installs beside Windows by making it
# smaller: thousands of files of all sizes, some in many pieces, some far
# out near the end, and the things NTFS holds that a resize could lose
# (sparse and compressed files, hard links, other data streams, a link,
# long names and names in several scripts).
#
#   make-windows-volume.sh OUTDIR [BYTES]
#
# Writes OUTDIR/windows.ntfs, a sparse file, and the list of what is in it:
# OUTDIR/manifest.sha256 (every file), manifest.streams, manifest.info.
#
# Needs root, because the volume is mounted to fill it: run it in a virtual
# machine, a container or on a CI runner. The only thing it mounts or writes
# is the file it makes.
set -eu

out=${1:?usage: make-windows-volume.sh OUTDIR [BYTES]}
bytes=${2:-32212254720}
[ "$(id -u)" = 0 ] || { echo "make-windows-volume.sh mounts the volume it makes, which needs root" >&2; exit 2; }
PATH=$PATH:/usr/sbin:/sbin
for tool in mkfs.ntfs ntfs-3g ntfsresize ntfsinfo ntfscp setfattr sha256sum fallocate; do
	command -v "$tool" >/dev/null 2>&1 || { echo "$tool is missing" >&2; exit 2; }
done
mkdir -p "$out"
out=$(cd "$out" && pwd)
img=$out/windows.ntfs
if [ -e "$img" ] && [ ! -f "$img" ]; then
	echo "$img is not a file" >&2
	exit 2
fi
work=$(mktemp -d)
mnt=$work/volume
mkdir "$mnt"
mounted=
cleanup() {
	[ -z "$mounted" ] || umount "$mnt" 2>/dev/null || true
	rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 1' INT TERM

attach() {
	ntfs-3g -o "$1streams_interface=xattr,compression" "$img" "$mnt"
	mounted=yes
}
# Unmounting returns before the driver has written everything: wait for it
# to end.
detach() {
	sync
	umount "$mnt"
	mounted=
	n=0
	while pgrep -f "ntfs-3g.*$img" >/dev/null; do
		n=$((n + 1))
		[ "$n" -lt 600 ] || { echo "ntfs-3g did not end" >&2; exit 1; }
		sleep 0.2
	done
}

rm -f "$img"
truncate -s "$bytes" "$img"
mkfs.ntfs -q -F -Q -L Windows -s 512 -c 4096 -p 239616 -H 255 -S 63 "$img" >/dev/null

# Bytes to cut files out of, so that no two files are alike and making
# thousands of them is quick.
pool=$work/pool
head -c 67108864 /dev/urandom >"$pool"
seed=20261127
# A number from 0 to $1 - 1, the same at every run.
pick() {
	seed=$(((seed * 1103515245 + 12345) % 2147483648))
	picked=$(((seed / 64) % $1))
}
# A file $1 of $2 bytes.
cut_file() {
	pick 50000000
	tail -c +"$((picked + 1))" "$pool" | head -c "$2" >"$1"
}

attach ""
mkdir -p "$mnt/Windows/System32" "$mnt/Windows/WinSxS" "$mnt/Program Files/Example App" \
	"$mnt/Users/Test/Documents" "$mnt/Users/Test/Pictures" "$mnt/Users/Zoë/Ünïcödé 文件" \
	"$mnt/Empty folder" "$mnt/Pieces" "$mnt/Late"
echo "files of all sizes"
i=0
while [ "$i" -lt 3000 ]; do
	pick 49152
	cut_file "$mnt/Windows/WinSxS/component-$i.dll" "$picked"
	i=$((i + 1))
done
i=0
while [ "$i" -lt 500 ]; do
	pick 524288
	cut_file "$mnt/Users/Test/Documents/report $i.docx" $((picked + 1))
	i=$((i + 1))
done
i=0
while [ "$i" -lt 120 ]; do
	pick 7340032
	cut_file "$mnt/Users/Test/Pictures/IMG_$i.jpg" $((picked + 1048576))
	i=$((i + 1))
done
cut_file "$mnt/Windows/System32/ntoskrnl.exe" 11534336
# As a program begins, which is how the installer knows a Windows is here.
printf 'MZ' | dd of="$mnt/Windows/System32/ntoskrnl.exe" conv=notrunc status=none
cut_file "$mnt/Users/Zoë/Ünïcödé 文件/résumé — final (2).txt" 70000
cut_file "$mnt/Users/Test/Documents/$(head -c 200 /dev/zero | tr '\0' a).txt" 5000
: >"$mnt/Users/Test/Documents/empty.txt"
echo "files in pieces"
round=0
while [ "$round" -lt 24 ]; do
	n=0
	while [ "$n" -lt 20 ]; do
		pick 50000000
		tail -c +"$((picked + 1))" "$pool" | head -c 131072 >>"$mnt/Pieces/growing-$n.bin"
		n=$((n + 1))
	done
	sync "$mnt/Pieces/growing-0.bin"
	round=$((round + 1))
done
echo "holes, and files in them"
i=0
while [ "$i" -lt 3000 ]; do
	rm "$mnt/Windows/WinSxS/component-$i.dll"
	i=$((i + 2))
done
sync
i=0
while [ "$i" -lt 60 ]; do
	pick 1572864
	cut_file "$mnt/Program Files/Example App/data$i.pak" $((picked + 524288))
	i=$((i + 1))
done
echo "what NTFS holds besides"
# Sparse: a large file that is mostly nothing.
truncate -s 268435456 "$mnt/Users/Test/disk image.vhdx"
cut_file "$work/chunk" 300000
dd if="$work/chunk" of="$mnt/Users/Test/disk image.vhdx" bs=4096 seek=100 conv=notrunc status=none
dd if="$work/chunk" of="$mnt/Users/Test/disk image.vhdx" bs=4096 seek=50000 conv=notrunc status=none
# Compressed, as Windows compresses a folder.
mkdir "$mnt/Compressed"
setfattr -h -v 0x00000810 -n system.ntfs_attrib_be "$mnt/Compressed"
i=0
while [ "$i" -lt 20 ]; do
	{
		yes "line $i of a log that compresses very well" | head -c 400000
		tail -c +"$((i * 5000 + 1))" "$pool" | head -c 20000
	} >"$mnt/Compressed/log-$i.txt"
	i=$((i + 1))
done
ln "$mnt/Windows/System32/ntoskrnl.exe" "$mnt/Windows/System32/same file.exe"
ln -s "System32/ntoskrnl.exe" "$mnt/Windows/kernel link"
setfattr -n user.Zone.Identifier -v "[ZoneTransfer] ZoneId=3" "$mnt/Users/Test/Documents/report 1.docx"
cut_file "$mnt/Users/Test/photo.heic" 100000
detach

# A large data stream beside a file.
head -c 3000000 "$pool" >"$work/stream"
ntfscp -q -N thumbnail "$img" "$work/stream" Users/Test/photo.heic

echo "files near the end, in pieces"
# NTFS keeps an eighth of the volume for the file table to grow into.
reserved_kb=$((bytes / 1024 / 8 + 65536))
# Takes the free space of the volume, but for $2 kilobytes, as files called
# $1-N, without writing it: what is written next has to go elsewhere. The
# kernel's driver gives a file its room without writing it, which ntfs-3g
# does not do through its mount. Smaller pieces are tried when a larger
# one does not fit.
fill() {
	mount -t ntfs3 -o loop "$img" "$mnt"
	mounted=yes
	part=0
	take_kb=1048576
	while [ "$take_kb" -ge 256 ]; do
		free_kb=$(df -k --output=avail "$mnt" | tail -1 | tr -d ' ')
		left_kb=$((free_kb - $2))
		[ "$left_kb" -ge 256 ] || break
		[ "$left_kb" -ge "$take_kb" ] || take_kb=$left_kb
		if fallocate -l $((take_kb * 1024)) "$mnt/$1-$part" 2>/dev/null; then
			part=$((part + 1))
		else
			rm -f "$mnt/$1-$part"
			take_kb=$((take_kb / 2))
		fi
	done
	[ "$part" -gt 0 ] || { echo "no room could be taken for $1" >&2; exit 1; }
	echo "$1: $part files, $(df -k --output=avail "$mnt" | tail -1 | tr -d ' ') kB left free"
	sync
	umount "$mnt"
	mounted=
}
# Most of what is free is taken, but not the table's share, so that what
# comes next lands far out.
fill filler-1 $((reserved_kb + 600000))
attach ""
i=0
while [ "$i" -lt 100 ]; do
	pick 3145728
	cut_file "$mnt/Late/late-$i.bin" $((picked + 262144))
	i=$((i + 1))
done
mkdir "$mnt/Late/gaps"
i=0
while [ "$i" -lt 600 ]; do
	cut_file "$mnt/Late/gaps/$i.bin" 131072
	i=$((i + 1))
done
detach
# Then all but a little of the rest, and every other small file is taken
# away: the room left near the end is in small holes, and files that are
# written then are in many pieces.
fill filler-2 1024
attach ""
i=0
while [ "$i" -lt 600 ]; do
	rm "$mnt/Late/gaps/$i.bin"
	i=$((i + 2))
done
sync
n=0
while [ "$n" -lt 20 ]; do
	cut_file "$mnt/Late/fragmented-$n.bin" 1572864
	pick 50000000
	tail -c +"$((picked + 1))" "$pool" | head -c 262144 >>"$mnt/Pieces/growing-$n.bin"
	n=$((n + 1))
done
sync
rm "$mnt"/filler-1-* "$mnt"/filler-2-*
detach

echo "the list of what is there"
attach "ro,"
(
	cd "$mnt"
	find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum
) >"$out/manifest.sha256"
{
	printf '%s\t%s\t%s\n' "Users/Test/Documents/report 1.docx" Zone.Identifier \
		"$(printf '%s' "[ZoneTransfer] ZoneId=3" | sha256sum | cut -d' ' -f1)"
	printf '%s\t%s\t%s\n' Users/Test/photo.heic thumbnail \
		"$(sha256sum "$work/stream" | cut -d' ' -f1)"
} >"$out/manifest.streams"
link=$(readlink "$mnt/Windows/kernel link")
detach

ntfsresize --info --no-progress-bar "$img" >"$work/info"
{
	echo "bytes=$bytes"
	echo "files=$(wc -l <"$out/manifest.sha256")"
	echo "link=$link"
	sed -n 's/^Space in use *: /used=/p' "$work/info"
	# How many clusters would have to move to make it as small as the test
	# makes it, 12 GB smaller: what lies near the end.
	ntfsresize --no-action --no-progress-bar --size $((bytes - 12000000000)) "$img" |
		sed -n 's/^Needed relocations *: /near-the-end=/p'
	# The most pieces any of the files written into the holes is in.
	n=0
	while [ "$n" -lt 20 ]; do
		ntfsinfo -v -F "Late/fragmented-$n.bin" "$img" |
			sed -n 's/^Total runs: [0-9]* (fragments: \([0-9]*\)).*/\1/p'
		n=$((n + 1))
	done | sort -n | tail -1 | sed 's/^/pieces=/'
} >"$out/manifest.info"
cat "$out/manifest.info"
# What the test relies on: data near the end, and files in pieces.
beyond=$(sed -n 's/^near-the-end=\([0-9]*\).*/\1/p' "$out/manifest.info")
pieces=$(sed -n 's/^pieces=//p' "$out/manifest.info")
[ "${beyond:-0}" -gt 10000 ] || { echo "too little lies near the end: ${beyond:-0} clusters" >&2; exit 1; }
[ "${pieces:-0}" -gt 3 ] || { echo "no file is in more than ${pieces:-0} pieces" >&2; exit 1; }
