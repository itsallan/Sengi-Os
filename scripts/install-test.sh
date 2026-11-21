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
# The only disk that is ever written is OUTDIR/disk.qcow2, which this
# script makes. The ISO is attached read-only.
#
# The window is driven with the keyboard, the same way boot-test.sh drives
# the desktop, and the installation that is timed is the one the window
# started. The guest reports through its serial port (see
# sengi-install-report).
set -eu
# A monitor whose QEMU has gone is a broken pipe; the script says what
# happened and does not die of it.
trap "" PIPE

iso=${1:?usage: install-test.sh ISO [OUTDIR]}
out=${2:-test-output/install}
budget_boot_s=${SENGI_BUDGET_BOOT_S:-10}
budget_ram_mb=${SENGI_BUDGET_RAM_MB:-400}
timeout_s=${SENGI_BOOT_TIMEOUT_S:-300}
install_timeout_s=${SENGI_INSTALL_TIMEOUT_S:-900}
disk_size=${SENGI_INSTALL_DISK_SIZE:-16G}

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
	"$out/mon.in" "$out/disk.qcow2" "$out/vars.fd" "$out"/monitor-*.log "$out"/guest-info-*.txt
disk=$out/disk.qcow2
qemu-img create -q -f qcow2 "$disk" "$disk_size"

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
sleep 3

# Found and started the way a person would, through the launcher.
key meta_l-spc 2
type_text install
key ret 1
wait_serial 'SENGI-INSTALLER running' 30 || give_up "the installer did not start from the launcher" installer
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
key tab
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

ram_mb=$(tr -d '\r' <"$serial" | sed -n 's/.*SENGI-IDLE ram_mb=\([0-9]*\).*/\1/p' | head -1)
installed=$(tr -d '\r' <"$serial" | sed -n 's/.*SENGI-INSTALLED //p' | head -1)
total_s=$(echo "$login_s $desktop_s" | awk '{printf "%.1f", $1 + $2}')
grep SENGI-INFO "$serial" | sed 's/.*SENGI-INFO //' >"$out/guest-info-installed.txt" || true
{
	echo "install_s=$install_s"
	echo "installed_boot_to_login_s=$login_s"
	echo "installed_login_to_desktop_s=$desktop_s"
	echo "installed_boot_s=$total_s"
	echo "installed_idle_ram_mb=$ram_mb"
	echo "install_live_boot_s=$live_s"
	echo "install_firmware=$firmware"
	echo "install_accel=$accel"
} >"$out/install-metrics.txt"
cat "$out/install-metrics.txt"
echo "installed: $installed"

fail=0
if grep -q SENGI-FAIL "$out"/serial-*.log; then
	sed -n 's/.*SENGI-FAIL /FAIL: /p' "$out"/serial-*.log >&2
	fail=1
fi
expected="user=$user host=$host zone=$zone root=/dev/vda3 type=ext4 firmware=$firmware keyboard=$keyboard"
if [ "$installed" != "$expected" ]; then
	echo "FAIL: the installed system is '$installed', and what was asked for is '$expected'" >&2
	fail=1
fi
for shot in installer-welcome installer-disk installer-review installer-progress installer-done \
	installed-login installed-desktop; do
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
