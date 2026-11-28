# shellcheck shell=sh
# The variables and functions of the script that sources this are used.
# shellcheck disable=SC2154
# The part of install-test.sh for SENGI_INSTALL_MODE=shrink: installing
# beside a Windows that has all of the disk, which is made smaller first.
# Sourced by install-test.sh, and uses its functions and variables.
#
# SENGI_NTFS is the directory that scripts/make-windows-volume.sh filled:
# a real NTFS volume with thousands of files, some of them far out near the
# end. The disk made of it (an EFI system partition, the reserved
# partition, Windows, and a recovery partition at the end, with no free
# space) is read from outside the guest afterwards: every file of Windows
# must be as it was, the volume must pass the checks of ntfsresize and
# ntfsfix, every other partition must be byte for byte as it was, and the
# installed system is then started as in the other modes.
#
# SENGI_SHRINK_FAULT injects a failure instead, and the test passes only if
# nothing is lost:
#   dirty       the volume is marked for a disk check: refused
#   hibernated  it has a hibernation file, as after Fast Startup: refused
#   bitlocker   its first sector says BitLocker: refused
#   otherhibernated
#               another NTFS partition of the disk has a hibernation file:
#               refused
#   powercut    the power is cut after the file system was made smaller and
#               before the partition was: the disk is consistent
# After a refusal the disk must be byte for byte as it was.

fault=${SENGI_SHRINK_FAULT:-}
case $fault in
'' | dirty | hibernated | otherhibernated | bitlocker | powercut) ;;
*)
	echo "SENGI_SHRINK_FAULT is dirty, hibernated, otherhibernated, bitlocker or powercut" >&2
	exit 2
	;;
esac

# What the test gives Sengi OS, in gigabytes: one step above the least.
shrink_gb=12
# The partitions, in sectors of 512 bytes. Windows is made as large as the
# volume that was made for it.
msr_start=$((esp_start + esp_sectors))
msr_sectors=32768
win_start=$((msr_start + msr_sectors))
rec_sectors=1048576

# Put into the NTFS volume $1 a hibernation file as Windows leaves one.
hibernation_file() {
	{
		printf 'hibr'
		head -c 8188 /dev/zero
	} >"$out/hiberfil"
	ntfscp -q "$1" "$out/hiberfil" hiberfil.sys
}

prepare_shrink() {
	[ -n "${SENGI_UEFI:-}" ] || {
		echo "SENGI_INSTALL_MODE=shrink needs SENGI_UEFI: it is only offered under UEFI" >&2
		exit 2
	}
	for tool in sfdisk mkfs.vfat mmd mcopy mdir sha256sum dd debugfs e2fsck truncate \
		mkfs.ntfs ntfscp ntfsfix ntfsresize ntfscat; do
		command -v "$tool" >/dev/null 2>&1 || {
			echo "SENGI_INSTALL_MODE=shrink needs $tool on this computer" >&2
			exit 2
		}
	done
	fixture=${SENGI_NTFS:?SENGI_NTFS names the directory that make-windows-volume.sh filled}
	[ -f "$fixture/windows.ntfs" ] && [ -f "$fixture/manifest.sha256" ] || {
		echo "there is no volume in $fixture" >&2
		exit 2
	}
	win_bytes=$(stat -c %s "$fixture/windows.ntfs")
	win_sectors=$((win_bytes / 512))
	rec_start=$((win_start + win_sectors))
	raw=$out/before.img
	truncate -s $(((rec_start + rec_sectors + 2048) * 512)) "$raw"
	printf 'label: gpt\nstart=%s, size=%s, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="EFI system partition"\nstart=%s, size=%s, type=E3C9E316-0B5C-4DB8-817D-F92DF00215AE, name="Microsoft reserved partition"\nstart=%s, size=%s, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, name="Basic data partition"\nstart=%s, size=%s, type=DE94BBA4-06D1-4D40-A16A-BFD50179D6AC, name="Recovery", attrs="RequiredPartition GUID:63"\n' \
		"$esp_start" "$esp_sectors" "$msr_start" "$msr_sectors" "$win_start" "$win_sectors" \
		"$rec_start" "$rec_sectors" | sfdisk -q "$raw"
	windows_mbr "$raw"
	write_esp "$raw"
	# The reserved partition holds no file system: bytes to compare.
	head -c $((msr_sectors * 512)) /dev/urandom |
		dd of="$raw" bs=512 seek="$msr_start" conv=notrunc status=none
	# The recovery partition: a volume of its own with a file in it.
	truncate -s $((rec_sectors * 512)) "$out/recovery.img"
	mkfs.ntfs -q -F -Q -L Recovery "$out/recovery.img" >/dev/null 2>&1
	echo "$marker_other" >"$out/before.marker"
	ntfscp -q "$out/recovery.img" "$out/before.marker" Winre.wim
	if [ "$fault" = otherhibernated ]; then
		hibernation_file "$out/recovery.img"
	fi
	dd if="$out/recovery.img" of="$raw" bs=1M seek=$((rec_start / 2048)) conv=notrunc,sparse status=none
	rm -f "$out/recovery.img"
	# Windows, with what the fault does to it.
	cp --reflink=auto "$fixture/windows.ntfs" "$out/windows.ntfs"
	case $fault in
	dirty)
		ntfsfix "$out/windows.ntfs" >/dev/null
		;;
	hibernated)
		hibernation_file "$out/windows.ntfs"
		;;
	esac
	dd if="$out/windows.ntfs" of="$raw" bs=1M seek=$((win_start / 2048)) conv=notrunc,sparse status=none
	rm -f "$out/windows.ntfs"
	if [ "$fault" = bitlocker ]; then
		# The start of a BitLocker volume, as far as anything looks before
		# it asks for a key: its name, and where its metadata is.
		at=$((win_start * 512))
		printf '\353\130\220-FVE-FS-\000\002\010' | dd of="$raw" bs=1 seek="$at" conv=notrunc status=none
		printf '\000\000\020\000\000\000\000\000\000\000\040\000\000\000\000\000\000\000\060\000\000\000\000\000' |
			dd of="$raw" bs=1 seek=$((at + 176)) conv=notrunc status=none
		printf -- '-FVE-FS-\000\001\002\000' | dd of="$raw" bs=1 seek=$((at + 1048576)) conv=notrunc status=none
	fi
	sfdisk -d "$raw" >"$out/before.dump"
	mdir -i "$raw@@$((esp_start * 512))" -/ -b :: | sort >"$out/before.files"
	cp --reflink=auto "$raw" "$disk"
}

# The disk page, from the list of ways to use the disk, which has the
# keyboard: choose "Install beside Windows", wait for the look at Windows,
# choose the size and say there is a backup. Ends on the Continue button's
# page having been left, or, where Windows is refused, ends the test.
shrink_disk_page() {
	key down
	key ret 1
	shot installer-disk-looking
	case $fault in
	bitlocker)
		# Seen without looking inside: the choice cannot be made.
		sleep 3
		;;
	dirty | hibernated | otherhibernated)
		wait_serial "SENGI-INSTALL analyzed refused ${fault#other}" 600 ||
			give_up "Windows was not refused as $fault" installer-disk-refused
		sleep 3
		;;
	*)
		wait_serial 'SENGI-INSTALL analyzed ok' 600 ||
			give_up "the look at Windows did not allow making it smaller" installer-disk-shrink
		sleep 3
		;;
	esac
	case $fault in
	bitlocker | dirty | hibernated | otherhibernated)
		shot installer-disk-refused
		# Nothing goes on from here but by choosing to erase.
		if grep -q 'SENGI-INSTALL begin' "$serial"; then
			give_up "an installation started on a refused disk" installer-disk-refused
		fi
		stop
		verify_untouched
		return
		;;
	esac
	shot installer-disk-shrink
	# The size: the least, and one step more.
	key tab
	key home
	key right 1
	# The tick that there is a backup, without which Continue is off.
	key tab
	shot installer-disk-no-backup
	key ret 1
	shot installer-disk-backup
	# Look again, Back, Continue.
	key tab
	key tab
	key tab
	key ret 1.5
}

verify_untouched() {
	: >"$out/dualboot-checks.txt"
	cp --reflink=auto "$disk" "$out/after.img"
	if cmp -s "$out/before.img" "$out/after.img"; then
		check_ok "after the refusal ($fault) the whole disk is byte for byte as it was"
	else
		check_fail "the disk changed although Windows was refused ($fault)"
	fi
	sed 's/^/dualboot: /' "$out/dualboot-checks.txt"
	if [ "$fault" = bitlocker ] && grep -q 'SENGI-INSTALL analyzed' "$serial"; then
		check_fail "a BitLocker volume was looked into"
	fi
	[ "$dual_fail" = 0 ]
	exit
}

# The volume of partition 3 of $out/after.img, cut to the $1 sectors of the
# partition it was made for, checked file by file.
check_windows() {
	dd if="$out/after.img" of="$out/windows-after.ntfs" bs=1M skip=$((win_start / 2048)) \
		count=$((($1 + 2047) / 2048)) conv=sparse status=none
	truncate -s $(($1 * 512)) "$out/windows-after.ntfs"
	# The volume ends inside its partition.
	volume_sectors=$(od -An -t u8 -j 40 -N 8 "$out/windows-after.ntfs" | tr -d ' ')
	if [ $((volume_sectors + 1)) -le "$1" ]; then
		check_ok "the Windows volume has $volume_sectors sectors, inside a partition of $1"
	else
		check_fail "the Windows volume has $volume_sectors sectors, more than fit in $1"
	fi
	# Marked for a disk check by ntfsresize, and left marked.
	if ntfsresize --info --no-progress-bar "$out/windows-after.ntfs" 2>&1 |
		grep -q 'scheduled for check'; then
		check_ok "the volume is left marked for a disk check at the next Windows start"
	else
		check_fail "the volume is not marked for a disk check"
	fi
	if "$scripts/check-windows-volume.sh" "$out/windows-after.ntfs" "$fixture" >"$out/windows-checks.txt" 2>&1; then
		while IFS= read -r line; do
			check_ok "${line#ok: }"
		done <"$out/windows-checks.txt"
	else
		cat "$out/windows-checks.txt"
		check_fail "the Windows volume lost or changed something, or is not consistent"
	fi
	rm -f "$out/windows-after.ntfs"
}

# The line of the dump $1 that describes partition $2, from after the name
# of its node.
part_line() {
	grep -E "^[^ ]*$2 :" "$1" | sed 's/^[^:]*://'
}

# The table's own lines, and partitions that must not have changed at all.
same_table_but() {
	for key in label label-id unit first-lba last-lba sector-size; do
		b=$(grep "^$key:" "$out/before.dump")
		a=$(grep "^$key:" "$out/after.dump")
		[ "$a" = "$b" ] && check_ok "table $key is as it was" || check_fail "table $key changed: '$b' became '$a'"
	done
	for n in "$@"; do
		b=$(part_line "$out/before.dump" "$n")
		a=$(part_line "$out/after.dump" "$n")
		[ -n "$b" ] && [ "$a" = "$b" ] && check_ok "partition $n (start, size, type, identifier, name, attributes) is as it was" ||
			check_fail "partition $n changed: '$b' became '$a'"
	done
}

# After the power was cut between the two writes.
verify_powercut() {
	: >"$out/dualboot-checks.txt"
	cp --reflink=auto "$disk" "$out/after.img"
	sfdisk -d "$out/after.img" >"$out/after.dump"
	same_table_but 1 2 3 4
	[ "$(grep -c -E '^[^ ]+ :' "$out/after.dump")" = 4 ] && check_ok "there are still four partitions" ||
		check_fail "the number of partitions changed"
	# Everything outside Windows, to the byte.
	same_sectors "sector 0, with the protective MBR as Windows wrote it" 0 1
	same_sectors "the gap in front of the first partition" 34 $((esp_start - 34))
	same_sectors "the EFI system partition" "$esp_start" "$esp_sectors"
	same_sectors "the reserved partition" "$msr_start" "$msr_sectors"
	same_sectors "the recovery partition" "$rec_start" "$rec_sectors"
	# The file system, made to the size the partition was going to get.
	check_windows "$(shrunk_sectors)"
	sed 's/^/dualboot: /' "$out/dualboot-checks.txt"
	[ "$dual_fail" = 0 ]
	exit
}

# The size partition 3 must have for Sengi OS to get $shrink_gb: Sengi OS
# is put on whole mebibytes at the end of what Windows had.
sengi_sectors() {
	echo $(((shrink_gb * 1000000000 + 1048575) / 1048576 * 2048))
}
shrunk_sectors() {
	echo $(((win_start + win_sectors) / 2048 * 2048 - $(sengi_sectors) - win_start))
}

verify_shrink() {
	: >"$out/dualboot-checks.txt"
	cp --reflink=auto "$disk" "$out/after.img"
	sfdisk -d "$out/after.img" >"$out/after.dump"
	same_table_but 1 2 4
	[ "$(grep -c -E '^[^ ]+ :' "$out/after.dump")" = 5 ] && check_ok "there is one more partition, five in all" ||
		check_fail "there are not 5 partitions"
	# Windows: the same partition, shorter by exactly what was chosen.
	b=$(part_line "$out/before.dump" 3 | sed 's/size= *[0-9]*,//')
	a=$(part_line "$out/after.dump" 3 | sed 's/size= *[0-9]*,//')
	[ "$a" = "$b" ] && check_ok "the Windows partition has its start, type, identifier, name and attributes" ||
		check_fail "the Windows partition changed: '$b' became '$a'"
	size=$(part_field "$out/after.dump" 3 size)
	want=$(shrunk_sectors)
	[ "$size" = "$want" ] && check_ok "the Windows partition has $size sectors, down from $win_sectors" ||
		check_fail "the Windows partition has $size sectors, not $want"
	new_start=$(part_field "$out/after.dump" 5 start)
	new_size=$(part_field "$out/after.dump" 5 size)
	new_type=$(part_field "$out/after.dump" 5 type)
	[ "$new_type" = 0FC63DAF-8483-4772-8E79-3D69D8477DE4 ] && check_ok "the new partition is of the Linux type" ||
		check_fail "the new partition is of type '$new_type'"
	if [ "$new_start" = $((win_start + want)) ] && [ "$new_size" = "$(sengi_sectors)" ] &&
		[ $((new_start + new_size)) -le $((win_start + win_sectors)) ]; then
		check_ok "the new partition (sectors $new_start to $((new_start + new_size - 1))) is in what Windows gave up"
	else
		check_fail "the new partition ($new_start, $new_size sectors) is not where Windows gave up room"
	fi
	check_mbr
	same_sectors "the gap in front of the first partition" 34 $((esp_start - 34))
	same_sectors "the reserved partition" "$msr_start" "$msr_sectors"
	same_sectors "the recovery partition" "$rec_start" "$rec_sectors"
	same_sectors "the space after the recovery partition" $((rec_start + rec_sectors)) \
		$(($(sed -n 's/^last-lba: //p' "$out/after.dump") + 1 - rec_start - rec_sectors))
	check_windows "$size"
	check_esp
	check_new_system "$new_start"
	sed 's/^/dualboot: /' "$out/dualboot-checks.txt"
	# The checks copied gigabytes, which must not still be on their way to
	# the host's disk while the installed system's start is timed.
	sync
}
