#!/bin/sh
# Install Sengi OS from an ISO onto a blank disk image in QEMU, through the
# installer's own window, then start the disk image alone and check that
# what is on it is an installed system. Fails if it is not, or if it is
# over budget.
#
#   install-test.sh ISO [OUTDIR]
#
# Set SENGI_UEFI to an OVMF firmware image to do both halves through UEFI
# instead of BIOS. With SENGI_UEFI_VARS as well (the matching variables
# template) the firmware's variables are kept from the first half to the
# second, as a real machine keeps them, so the boot entry the installer
# makes is the one that is used.
#
# The live system and the installed one run on sengi-compositor, the default.
# Set SENGI_COMPOSITOR=labwc to run both on labwc: every start is handed the
# file that sengi-session looks for, through a credential, since the
# installed disk carries no choice.
#
# SENGI_INSTALL_MODE=beside tests "install beside the existing system"
# instead (it needs SENGI_UEFI). The disk is made first, on the host and
# without root, to look like a computer that came with another system: a
# GPT with an EFI system partition that holds files of the other system
# (among them stand-ins for the Windows boot manager and its BCD, which is what os-prober looks for), a partition of the
# other system with data in it, and about 12 GiB free. After the
# installation, and before the installed disk is started, the disk image
# is read from outside the guest and everything is checked: the old
# partitions, their types and identifiers, the data in the other system's
# partition, every file that was on the EFI system partition and every
# gap between partitions are as they were; there is one new Linux
# partition, inside the free space, with an ext4 file system that is
# clean; GRUB is in a directory of its own; the boot menu lists the other
# system. Then the disk is started alone, as in the other mode.
#
# The only disk that is ever written is OUTDIR/disk.qcow2, which this
# script makes. The ISO is attached read-only.
#
# The window is driven with the keyboard, the same way boot-test.sh drives
# the desktop, and the installation that is timed is the one the window
# started. The guest reports through its serial port (see
# sengi-install-report).
#
# SENGI_INSTALL_FROM=dialog starts the installer with the button of the live
# image's question (the default when erasing); launcher answers it with
# Escape, to try first, and then starts the installer from the launcher (the
# default beside another system).
set -eu
# A monitor whose QEMU has gone is a broken pipe; the script says what
# happened and does not die of it.
trap "" PIPE
# On a CI runner a failure is also an annotation, which can be read without
# the log.
annotate() {
	[ -z "${GITHUB_ACTIONS:-}" ] || echo "::error::$*"
	return 0
}
on_exit() {
	status=$?
	[ "$status" -eq 0 ] || annotate "install-test.sh ended with status $status"
}
trap on_exit EXIT

iso=${1:?usage: install-test.sh ISO [OUTDIR]}
out=${2:-test-output/install}
budget_boot_s=${SENGI_BUDGET_BOOT_S:-10}
budget_ram_mb=${SENGI_BUDGET_RAM_MB:-400}
timeout_s=${SENGI_BOOT_TIMEOUT_S:-300}
install_timeout_s=${SENGI_INSTALL_TIMEOUT_S:-900}
mode=${SENGI_INSTALL_MODE:-erase}
case $mode in
erase)
	disk_size=${SENGI_INSTALL_DISK_SIZE:-16G}
	from=${SENGI_INSTALL_FROM:-dialog}
	;;
beside)
	disk_size=${SENGI_INSTALL_DISK_SIZE:-14G}
	from=${SENGI_INSTALL_FROM:-launcher}
	;;
*)
	echo "SENGI_INSTALL_MODE is erase or beside" >&2
	exit 2
	;;
esac
case $from in
dialog | launcher) ;;
*)
	echo "SENGI_INSTALL_FROM is dialog or launcher" >&2
	exit 2
	;;
esac

# What the test answers the installer with.
full_name="Test User"
user="test"
password=sengitest
host=sengi
zone=Europe/Paris
keyboard=gb

mkdir -p "$out"
out=$(cd "$out" && pwd)
rm -f "$out"/serial-*.log "$out"/*.png "$out"/*.ppm "$out/install-metrics.txt" \
	"$out/mon.in" "$out/disk.qcow2" "$out/vars.fd" "$out"/monitor-*.log "$out"/guest-info-*.txt \
	"$out"/before.* "$out"/after.* "$out"/esp.img "$out/dualboot-checks.txt"
disk=$out/disk.qcow2

# Sectors of 512 bytes. The EFI system partition and the other system's
# partition are made first; what is left to the end of the disk is free.
esp_start=2048
esp_sectors=532480
other_start=534528
other_sectors=2097152
# Two more systems' partitions, which a boot loader must only look at: an
# ext4 volume that was not shut down (its journal needs recovery, which a
# mount that is allowed to write would do), and what an NTFS volume of a
# Windows that hibernated looks like at its start.
ext_start=$((other_start + other_sectors))
ext_sectors=131072
ntfs_start=$((ext_start + ext_sectors))
ntfs_sectors=131072
free_start=$((ntfs_start + ntfs_sectors))
marker_other="SENGI-OTHER-SYSTEM-DATA do not touch"
marker_boot="SENGI-OTHER-SYSTEM-BOOT-FILE do not touch"

# A disk as a computer with another system on it comes: made as a raw
# file with the tools of the host, none of which needs root, then turned
# into the qcow2 the guest gets.
prepare_beside() {
	[ -n "${SENGI_UEFI:-}" ] || {
		echo "SENGI_INSTALL_MODE=beside needs SENGI_UEFI: installing beside another system is only offered under UEFI" >&2
		exit 2
	}
	for tool in sfdisk mkfs.vfat mmd mcopy mdir sha256sum dd debugfs e2fsck truncate; do
		command -v "$tool" >/dev/null 2>&1 || {
			echo "SENGI_INSTALL_MODE=beside needs $tool on this computer" >&2
			exit 2
		}
	done
	raw=$out/before.img
	truncate -s "$disk_size" "$raw"
	printf 'label: gpt\nstart=%s, size=%s, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="EFI System Partition"\nstart=%s, size=%s, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, name="Basic data partition"\nstart=%s, size=%s, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="Linux filesystem"\nstart=%s, size=%s, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, name="Windows data"\n' \
		"$esp_start" "$esp_sectors" "$other_start" "$other_sectors" "$ext_start" "$ext_sectors" "$ntfs_start" "$ntfs_sectors" | sfdisk -q "$raw"
	# The EFI system partition, with what another system keeps there.
	truncate -s $((esp_sectors * 512)) "$out/esp.img"
	mkfs.vfat -F 32 -n ESP "$out/esp.img" >/dev/null
	echo "$marker_boot" >"$out/before.marker"
	mmd -i "$out/esp.img" ::/EFI ::/EFI/Microsoft ::/EFI/Microsoft/Boot ::/EFI/Boot
	mcopy -i "$out/esp.img" "$out/before.marker" ::/EFI/Microsoft/Boot/bootmgfw.efi
	mcopy -i "$out/esp.img" "$out/before.marker" ::/EFI/Microsoft/Boot/BCD
	mcopy -i "$out/esp.img" "$out/before.marker" ::/EFI/Boot/bootx64.efi
	dd if="$out/esp.img" of="$raw" bs=512 seek="$esp_start" conv=notrunc status=none
	rm -f "$out/esp.img"
	# The other system's partition: data that has to come out the same.
	{
		printf '%s\n' "$marker_other"
		dd if=/dev/zero bs=512 count=7 status=none
		i=0
		while [ "$i" -lt 64 ]; do
			printf '%s %s\n' "$marker_other" "$i"
			i=$((i + 1))
		done
	} | dd of="$raw" bs=512 seek="$other_start" conv=notrunc status=none
	# The ext4 volume, with its journal marked as needing recovery.
	truncate -s $((ext_sectors * 512)) "$out/ext.img"
	mkfs.ext4 -q -F -O has_journal "$out/ext.img"
	echo "$marker_other" >"$out/before.marker2"
	debugfs -w -R "write $out/before.marker2 marker.txt" "$out/ext.img" >/dev/null 2>&1
	debugfs -w -R "feature needs_recovery" "$out/ext.img" >/dev/null 2>&1
	dd if="$out/ext.img" of="$raw" bs=512 seek="$ext_start" conv=notrunc status=none
	rm -f "$out/ext.img" "$out/before.marker2"
	# The start of an NTFS volume, and the signature of a hibernation file
	# further in.
	{
		printf '\353\122\220NTFS    '
		dd if=/dev/zero bs=1 count=500 status=none
	} | dd of="$raw" bs=512 seek="$ntfs_start" conv=notrunc status=none
	printf 'hibr%s' "$marker_other" | dd of="$raw" bs=1M seek=$((ntfs_start / 2048 + 1)) conv=notrunc status=none
	sfdisk -d "$raw" >"$out/before.dump"
	mdir -i "$raw@@$((esp_start * 512))" -/ -b :: | sort >"$out/before.files"
	qemu-img convert -O qcow2 "$raw" "$disk"
}

if [ "$mode" = beside ]; then
	prepare_beside
else
	qemu-img create -q -f qcow2 "$disk" "$disk_size"
fi

accel=tcg cpu=max
if [ -w /dev/kvm ]; then accel=kvm cpu=host; fi
firmware=bios
if [ -n "${SENGI_UEFI:-}" ]; then
	firmware=uefi
	if [ -n "${SENGI_UEFI_VARS:-}" ]; then
		cp "$SENGI_UEFI_VARS" "$out/vars.fd"
		chmod u+w "$out/vars.fd"
	fi
fi

# systemd-tmpfiles makes the file early in the start, from a credential the
# firmware tables carry. No comma may be in it.
choice=
case "${SENGI_COMPOSITOR:-}" in
'' | own) ;;
labwc) choice='io.systemd.credential:tmpfiles.extra=f /run/sengi-os.compositor 0644 root root - labwc' ;;
*)
	echo "SENGI_COMPOSITOR is own or labwc" >&2
	exit 2
	;;
esac

now() { date +%s.%N; }
since() { echo "$(now) $1" | awk '{printf "%.1f", $1 - $2}'; }

# Read the ISO once so the host's disk cache does not count against the guest.
cat "$iso" >/dev/null

# Start QEMU with the drives given as arguments. $serial is where the
# guest's reports go.
start_qemu() {
	serial=$1
	monitor=$2
	shift 2
	: >"$serial"
	rm -f "$out/mon.in"
	mkfifo "$out/mon.in"
	if [ "$firmware" = uefi ] && [ -f "$out/vars.fd" ]; then
		set -- "$@" -drive "if=pflash,format=raw,readonly=on,file=$SENGI_UEFI" \
			-drive "if=pflash,format=raw,file=$out/vars.fd"
	elif [ "$firmware" = uefi ]; then
		set -- "$@" -bios "$SENGI_UEFI"
	fi
	start=$(now)
	qemu-system-x86_64 -machine q35,accel=$accel -cpu $cpu -smp 2 -m 4096 \
		"$@" \
		-no-reboot \
		-device virtio-vga -display none \
		-audiodev none,id=snd -device intel-hda -device hda-output,audiodev=snd \
		-nic user,model=virtio-net-pci \
		-smbios type=11,value=io.systemd.credential:sengi.test.install=1 \
		${choice:+-smbios "type=11,value=$choice"} \
		-serial file:"$serial" \
		-monitor stdio <"$out/mon.in" >"$monitor" 2>&1 &
	qemu=$!
	exec 3>"$out/mon.in"
}

stop() {
	echo quit >&3 2>/dev/null || true
	exec 3>&-
	wait "$qemu" 2>/dev/null || true
	rm -f "$out/mon.in"
}

# Wait for a line of the guest's report: at most $2 seconds.
wait_serial() {
	tries=$(($2 * 10))
	while ! grep -q "$1" "$serial"; do
		kill -0 "$qemu" 2>/dev/null || { echo "qemu exited while waiting for $1" >&2; return 1; }
		tries=$((tries - 1))
		[ "$tries" -gt 0 ] || { echo "timed out waiting for $1" >&2; return 1; }
		sleep 0.1
	done
}

shot() {
	echo "screendump $out/$1.png -f png" >&3
	sleep 1
}
key() {
	echo "sendkey $1" >&3
	sleep "${2:-0.6}"
}
# Type text: small letters, capitals, digits and spaces.
type_text() {
	rest=$1
	while [ -n "$rest" ]; do
		char=${rest%"${rest#?}"}
		rest=${rest#?}
		case $char in
		' ') key spc 0.25 ;;
		[A-Z]) key "shift-$(echo "$char" | tr '[:upper:]' '[:lower:]')" 0.25 ;;
		*) key "$char" 0.25 ;;
		esac
	done
}

give_up() {
	echo "FAIL: $1" >&2
	annotate "FAIL: $1"
	shot "${2:-failure}"
	stop
	sed -n 's/.*SENGI-FAIL /FAIL: /p' "$out"/serial-*.log >&2
	exit 1
}

# First half: the live image, with the blank disk beside it.
start_qemu "$out/serial-install.log" "$out/monitor-install.log" \
	-drive "file=$iso,format=raw,if=none,id=medium,readonly=on" \
	-device virtio-blk-pci,drive=medium,bootindex=1 \
	-drive "file=$disk,format=qcow2,if=none,id=target,cache=unsafe" \
	-device virtio-blk-pci,drive=target,bootindex=2
wait_serial SENGI-LIVE "$timeout_s" || give_up "the live desktop did not come up" live
live_s=$(since "$start")

# The live image asks whether to install or try it first, and the question
# has the keyboard, on "Install Sengi OS".
wait_serial 'SENGI-DIALOG [um]' 90 || give_up "the guest said nothing of the live question" live-dialog
grep -q 'SENGI-DIALOG up' "$serial" || give_up "the live question did not appear" live-dialog
sleep 3
shot installer-live-dialog
if [ "$from" = dialog ]; then
	key ret 1
	wait_serial 'SENGI-INSTALLER running' 30 ||
		give_up "Install Sengi OS in the live question did not start the installer" installer
	wait_serial 'SENGI-DIALOG gone' 15 || give_up "the live question stayed after Install Sengi OS" installer
else
	# Try it first, and later the installer found and started the way a
	# person would, through the launcher.
	key esc 1
	wait_serial 'SENGI-DIALOG gone' 15 || give_up "Escape did not close the live question" live-dialog
	sleep 2
	if grep -q 'SENGI-INSTALLER running' "$serial"; then
		give_up "choosing to try it first started the installer" installer
	fi
	key meta_l-spc 2
	type_text install
	key ret 1
	wait_serial 'SENGI-INSTALLER running' 30 || give_up "the installer did not start from the launcher" installer
fi
sleep 4
shot installer-welcome
key ret 1.5

# Region. The keyboard layout: search, the layout, then its usual variant.
shot installer-region
key tab
key ret 1.5
type_text "english uk"
sleep 1
shot installer-keyboard
key ret 1.5
key ret 2
# The time zone, three stops further on: the layout, the field to try
# it in, and the zone.
key tab
key tab
key tab
key ret 1.5
type_text paris
sleep 1
key ret 1.5
shot installer-region-chosen
key ret 1.5

# Disk. The list has the keyboard, on the first disk that can be chosen;
# the medium the live system runs from is listed and refused.
shot installer-disk
key ret 1
shot installer-disk-chosen
# The way to use the disk is a list of its own after the list of disks:
# erasing is the one that is chosen, and the second is installing beside
# the other system.
key tab
if [ "$mode" = beside ]; then
	key down
	key ret 1
	shot installer-disk-beside
fi
key tab
key tab
key tab
key ret 1.5

# You.
type_text "$full_name"
key tab
key tab
type_text "$password"
key tab
type_text "$password"
key tab
sleep 0.5
shot installer-you
key ret 1.5

# Review. The keyboard starts on Back; the button that erases is one stop
# on, and its question starts on Cancel.
shot installer-review
if grep -q 'SENGI-INSTALL begin' "$serial"; then
	give_up "the installation started before it was confirmed" installer-review
fi
key tab
key ret 1.5
shot installer-confirm
if grep -q 'SENGI-INSTALL begin' "$serial"; then
	give_up "the installation started before the question was answered" installer-confirm
fi
# The question that erases starts on Cancel, and Install is one stop on.
# The one beside another system is not dangerous: it starts on Install.
if [ "$mode" != beside ]; then key tab; fi
key ret 0.5
wait_serial 'SENGI-INSTALL begin check' 30 || give_up "the installation did not start" installer-progress
if wait_serial 'SENGI-INSTALL begin copy' 120; then
	sleep 2
	shot installer-progress
fi
tries=$((install_timeout_s * 2))
until grep -q -E 'SENGI-INSTALL (done|failed) ' "$serial"; do
	kill -0 "$qemu" 2>/dev/null || give_up "qemu exited during the installation"
	tries=$((tries - 1))
	[ "$tries" -gt 0 ] || give_up "the installation did not finish in $install_timeout_s s" installer-progress
	sleep 0.5
done
wait_serial SENGI-INSTALL-REPORTED 30 || true
sleep 2
if grep -q 'SENGI-INSTALL failed ' "$serial"; then
	why=$(tr -d '\r' <"$serial" | sed -n 's/.*SENGI-INSTALL failed //p' | head -1)
	give_up "the installation failed: $why" installer-failed
fi
install_s=$(tr -d '\r' <"$serial" | sed -n 's/.*SENGI-INSTALL done \([0-9]*\).*/\1/p' | head -1)
shot installer-done
grep SENGI-INFO "$serial" | sed 's/.*SENGI-INFO //' >"$out/guest-info-install.txt" || true

# Restart, which has the keyboard on the last page. QEMU was told to end
# where the guest restarts.
key ret 1
tries=600
while kill -0 "$qemu" 2>/dev/null; do
	tries=$((tries - 1))
	[ "$tries" -gt 0 ] || give_up "Restart did not restart the computer" installer-restart
	sleep 0.1
done
exec 3>&-
wait "$qemu" 2>/dev/null || true
rm -f "$out/mon.in"

# Beside another system, read from outside the guest: the disk as it is
# now against the disk as it was made. Nothing is trusted that the guest
# says about itself.
dual_fail=0
# Seconds GRUB's menu waits for a person to choose between systems. Where
# another system is found the menu is shown, and the wait is the
# person's, not the system's: it is left out of the boot figure and
# reported on its own.
menu_wait=0
check_fail() {
	echo "FAIL: $1" >&2
	annotate "FAIL: $1"
	echo "FAIL: $1" >>"$out/dualboot-checks.txt"
	dual_fail=1
}
check_ok() {
	echo "ok: $1" >>"$out/dualboot-checks.txt"
}
# sha256 of $3 sectors of image $1 from sector $2.
hash_sectors() {
	dd if="$1" bs=512 skip="$2" count="$3" status=none | sha256sum | cut -d" " -f1
}
# A field of the line that describes partition $2 in the dump $1.
part_field() {
	grep -E "^[^ ]*$2 :" "$1" | sed -n "s/.*[ ,]$3= *\\([^,]*\\).*/\\1/p" | head -1
}
same_sectors() {
	what=$1 start=$2 count=$3
	[ "$count" -gt 0 ] || return 0
	b=$(hash_sectors "$out/before.img" "$start" "$count")
	a=$(hash_sectors "$out/after.img" "$start" "$count")
	if [ "$a" = "$b" ]; then
		check_ok "$what is byte for byte as it was"
	else
		check_fail "$what changed (sectors $start to $((start + count - 1)))"
	fi
}

verify_beside() {
	: >"$out/dualboot-checks.txt"
	qemu-img convert -O raw "$disk" "$out/after.img"
	sfdisk -d "$out/after.img" >"$out/after.dump"

	# The table: the same disk, the same two partitions to the letter.
	for key in label label-id unit first-lba last-lba sector-size; do
		b=$(grep "^$key:" "$out/before.dump")
		a=$(grep "^$key:" "$out/after.dump")
		[ "$a" = "$b" ] && check_ok "table $key is as it was" || check_fail "table $key changed: '$b' became '$a'"
	done
	for n in 1 2 3 4; do
		b=$(grep -E "^[^ ]*$n :" "$out/before.dump" | sed 's/^[^:]*://')
		a=$(grep -E "^[^ ]*$n :" "$out/after.dump" | sed 's/^[^:]*://')
		[ -n "$b" ] && [ "$a" = "$b" ] && check_ok "partition $n (start, size, type, identifier, name) is as it was" ||
			check_fail "partition $n changed: '$b' became '$a'"
	done
	count=$(grep -c -E '^[^ ]+ :' "$out/after.dump")
	[ "$count" = 5 ] && check_ok "there is one more partition, five in all" || check_fail "there are $count partitions, not 5"

	# The new one: Linux, inside the free space, and nowhere else.
	new_start=$(part_field "$out/after.dump" 5 start)
	new_size=$(part_field "$out/after.dump" 5 size)
	new_type=$(part_field "$out/after.dump" 5 type)
	last_lba=$(sed -n 's/^last-lba: //p' "$out/after.dump")
	new_end=$((new_start + new_size))
	[ "$new_type" = 0FC63DAF-8483-4772-8E79-3D69D8477DE4 ] && check_ok "the new partition is of the Linux type" ||
		check_fail "the new partition is of type '$new_type'"
	if [ "$new_start" -ge "$free_start" ] && [ "$new_end" -le $((last_lba + 1)) ]; then
		check_ok "the new partition (sectors $new_start to $((new_end - 1))) is inside the free space (from $free_start)"
	else
		check_fail "the new partition ($new_start, $new_size sectors) is not inside the free space (from $free_start to $last_lba)"
	fi
	[ $((new_size * 512)) -ge 10737418240 ] && check_ok "the new partition has the 10 GiB or more that was free" ||
		check_fail "the new partition is only $new_size sectors"

	# Every byte outside the new partition and the EFI system
	# partition: in front of the first partition, the other system's
	# partition, and what is left over around the new one.
	same_sectors "the gap in front of the first partition" 34 $((esp_start - 34))
	same_sectors "the other system's partition" "$other_start" "$other_sectors"
	same_sectors "the other system's ext4 volume that needs recovery" "$ext_start" "$ext_sectors"
	same_sectors "the other system's NTFS volume" "$ntfs_start" "$ntfs_sectors"
	same_sectors "the space between the other system and the new partition" "$free_start" $((new_start - free_start))
	same_sectors "the space after the new partition" "$new_end" $((last_lba + 1 - new_end))
	# And it is what was written there: the marker is still the first line.
	first=$(dd if="$out/after.img" bs=512 skip="$other_start" count=1 status=none | head -1)
	[ "$first" = "$marker_other" ] && check_ok "the other system's data can be read" || check_fail "the other system's data is gone"

	# The EFI system partition: every file that was there is there and
	# unchanged; what is new is under EFI/sengios and nowhere else.
	esp="$out/after.img@@$((esp_start * 512))"
	mdir -i "$esp" -/ -b :: | sort >"$out/after.files"
	if grep -v -x -F -f "$out/before.files" "$out/after.files" | grep -v -i '^::/EFI/sengios' | grep -q .; then
		check_fail "the EFI system partition has new files outside EFI/sengios: $(grep -v -x -F -f "$out/before.files" "$out/after.files" | grep -v -i '^::/EFI/sengios' | tr '\n' ' ')"
	else
		check_ok "the only new files on the EFI system partition are under EFI/sengios"
	fi
	if grep -v -x -F -f "$out/after.files" "$out/before.files" | grep -q .; then
		check_fail "files of the other system are gone from the EFI system partition"
	else
		check_ok "no file of the other system is gone from the EFI system partition"
	fi
	for file in ::/EFI/Microsoft/Boot/bootmgfw.efi ::/EFI/Microsoft/Boot/BCD ::/EFI/Boot/bootx64.efi; do
		content=$(mcopy -i "$esp" "$file" - 2>/dev/null | head -1)
		[ "$content" = "$marker_boot" ] && check_ok "$file is as it was" || check_fail "$file changed"
	done
	grep -q -i '^::/EFI/sengios/grubx64.efi$' "$out/after.files" && check_ok "GRUB is in EFI/sengios" ||
		check_fail "there is no EFI/sengios/grubx64.efi"

	# The new file system.
	target="$out/after.img?offset=$((new_start * 512))"
	if e2fsck -fn "$target" >"$out/after.fsck" 2>&1; then
		check_ok "the new ext4 file system is clean"
	else
		check_fail "e2fsck does not find the new file system clean: $(tail -3 "$out/after.fsck" | tr '\n' ' ')"
	fi
	menu=$(debugfs -R "cat /boot/grub/grub.cfg" "$target" 2>/dev/null)
	case $menu in
	*"Windows Boot Manager"*) check_ok "the boot menu lists the other system" ;;
	*) check_fail "the boot menu does not list the other system" ;;
	esac
	case $menu in
	*"menuentry 'Sengi OS"*) check_ok "the boot menu lists Sengi OS" ;;
	*) check_fail "the boot menu does not list Sengi OS" ;;
	esac
	# Sengi OS comes first in the menu.
	grub_default=$(debugfs -R "cat /etc/default/grub" "$target" 2>/dev/null)
	case $grub_default in
	*GRUB_TIMEOUT_STYLE=menu*) menu_wait=$(printf '%s\n' "$grub_default" | sed -n 's/^GRUB_TIMEOUT=//p' | head -1) ;;
	esac
	first_entry=$(printf '%s\n' "$menu" | sed -n "s/^menuentry '\\([^']*\\)'.*/\\1/p" | head -1)
	case $first_entry in
	"Sengi OS"*) check_ok "Sengi OS is the first entry of the menu" ;;
	*) check_fail "the first entry of the menu is '$first_entry'" ;;
	esac
	sed 's/^/dualboot: /' "$out/dualboot-checks.txt"
}

if [ "$mode" = beside ]; then
	verify_beside
fi

# Second half: the disk alone. What the first half wrote is still on its
# way to the host's disk, since QEMU was told not to wait for it. Left to
# finish during the second start it is the host that is slow, not the
# guest, so it is let finish first.
sync "$disk" 2>/dev/null || sync
start_qemu "$out/serial-installed.log" "$out/monitor-installed.log" \
	-drive "file=$disk,format=qcow2,if=virtio,cache=unsafe"
wait_serial SENGI-LOGIN "$timeout_s" || give_up "the installed system did not reach the login screen" installed-login
login_s=$(since "$start")
sleep 3
shot installed-login
if grep -q SENGI-DESKTOP "$serial"; then
	give_up "the installed system logged somebody in by itself" installed-login
fi
type_text "$password"
typed=$(now)
key ret 0
wait_serial SENGI-DESKTOP 60 || give_up "the password chosen in the installer did not log in" installed-desktop
desktop_s=$(since "$typed")
sleep 2
shot installed-desktop
wait_serial SENGI-DONE "$timeout_s" || give_up "the installed system did not finish its report" installed-desktop
stop
report=$serial

# Third start: the way in when the desktop does not start. GRUB's menu is
# hidden and waits a second for Esc. Esc is pressed all through the hand-
# over from the firmware; the menu then has to be up, which shows as the
# system not starting by itself, and on the screen. Return starts it.
menu_fail=
start_qemu "$out/serial-menu.log" "$out/monitor-menu.log" \
	-drive "file=$disk,format=qcow2,if=virtio,cache=unsafe"
tries=60
while [ "$tries" -gt 0 ]; do
	echo "sendkey esc" >&3
	tries=$((tries - 1))
	sleep 0.2
done
sleep 8
shot installed-boot-menu
if grep -q SENGI-LOGIN "$serial"; then
	menu_fail="Esc at start-up did not show GRUB's menu: the system started by itself"
else
	key ret 0
	wait_serial SENGI-LOGIN "$timeout_s" ||
		menu_fail="the system did not start from GRUB's menu after Return"
	grep -q 'sengi.text' "$report" && menu_fail="the kernel's command line of an ordinary start says text mode"
fi
stop
serial=$report

ram_mb=$(tr -d '\r' <"$serial" | sed -n 's/.*SENGI-IDLE ram_mb=\([0-9]*\).*/\1/p' | head -1)
installed=$(tr -d '\r' <"$serial" | sed -n 's/.*SENGI-INSTALLED //p' | head -1)
total_s=$(echo "$login_s $desktop_s ${menu_wait:-0}" | awk '{printf "%.1f", $1 + $2 - $3}')
grep SENGI-INFO "$serial" | sed 's/.*SENGI-INFO //' >"$out/guest-info-installed.txt" || true
{
	echo "install_s=$install_s"
	echo "installed_boot_to_login_s=$login_s"
	echo "installed_login_to_desktop_s=$desktop_s"
	echo "installed_boot_s=$total_s"
	echo "installed_menu_wait_s=${menu_wait:-0}"
	echo "installed_idle_ram_mb=$ram_mb"
	echo "install_live_boot_s=$live_s"
	echo "install_firmware=$firmware"
	echo "install_accel=$accel"
	echo "install_from=$from"
	if grep -q sengi-composito "$out/guest-info-installed.txt"; then
		echo "installed_compositor=sengi-compositor"
	elif grep -q labwc "$out/guest-info-installed.txt"; then
		echo "installed_compositor=labwc"
	else
		echo "installed_compositor="
	fi
} >"$out/install-metrics.txt"
cat "$out/install-metrics.txt"
echo "installed: $installed"

fail=0
[ "${dual_fail:-0}" = 0 ] || fail=1
# The firmware, as the installed system lists it: the entry it was started
# from (the fallback path holds the other system's file, so only Sengi OS's
# own entry can have started it) is the first thing the firmware tries. (Entries of the computer's own, Windows' among them, are kept by
# the installer and checked in its unit tests: the firmware of this test starts
# with none.)
if [ "$mode" = beside ]; then
	order=$(sed -n 's/^BootOrder: *\([0-9A-Fa-f]*\).*/\1/p' "$out/guest-info-installed.txt" | head -1)
	current=$(sed -n 's/^BootCurrent: *\([0-9A-Fa-f]*\).*/\1/p' "$out/guest-info-installed.txt" | head -1)
	if [ -n "$order" ] && [ "$order" = "$current" ]; then
		echo "ok: the entry the installed system was started from, which is the only one that can start it, is first in the firmware's boot order ($order)"
	else
		echo "FAIL: Sengi OS is not first in the firmware's boot order ('$order')" >&2
		annotate "FAIL: Sengi OS is not first in the firmware's boot order"
		fail=1
	fi
fi
if grep -q SENGI-FAIL "$out"/serial-*.log; then
	sed -n 's/.*SENGI-FAIL /FAIL: /p' "$out"/serial-*.log >&2
	fail=1
fi
if [ -n "$menu_fail" ]; then
	echo "FAIL: $menu_fail" >&2
	fail=1
fi
root_node=/dev/vda3
[ "$mode" = beside ] && root_node=/dev/vda5
expected="user=$user host=$host zone=$zone root=$root_node type=ext4 firmware=$firmware keyboard=$keyboard"
if [ "$installed" != "$expected" ]; then
	echo "FAIL: the installed system is '$installed', and what was asked for is '$expected'" >&2
	fail=1
fi
# The installed system lists its biggest processes; the compositor asked for
# has to be among them, and the other one not.
if [ "${SENGI_COMPOSITOR:-}" = labwc ]; then
	grep -q labwc "$out/guest-info-installed.txt" || { echo "FAIL: the installed system did not run labwc" >&2; fail=1; }
	if grep -q sengi-composito "$out/guest-info-installed.txt"; then echo "FAIL: the installed system ran sengi-compositor" >&2; fail=1; fi
else
	grep -q sengi-composito "$out/guest-info-installed.txt" || { echo "FAIL: the installed system did not run sengi-compositor" >&2; fail=1; }
	if grep -q labwc "$out/guest-info-installed.txt"; then echo "FAIL: the installed system ran labwc" >&2; fail=1; fi
fi
for shot in installer-live-dialog installer-welcome installer-disk installer-review installer-progress installer-done \
	installed-login installed-desktop installed-boot-menu; do
	[ -s "$out/$shot.png" ] || { echo "FAIL: no screenshot $shot.png was saved" >&2; fail=1; }
done
if [ "${ram_mb:-9999}" -ge "$budget_ram_mb" ]; then
	echo "FAIL: idle RAM of the installed system is ${ram_mb} MB, over the ${budget_ram_mb} MB budget" >&2
	fail=1
fi
# The login screen waits for a person; the time they take to type is not
# the system's. What counts against the budget is the two stretches the
# system is busy: power-on to the login screen, and password to desktop.
if [ "$(echo "$total_s $budget_boot_s" | awk '{print ($1 >= $2)}')" = 1 ]; then
	echo "FAIL: the installed system took ${total_s} s to boot and log in, budget is ${budget_boot_s} s" >&2
	fail=1
fi
exit $fail
