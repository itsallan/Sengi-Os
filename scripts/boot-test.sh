#!/bin/sh
# Boot an ISO headless in QEMU, wait for the desktop, take a screenshot and
# record boot time and idle RAM. Fails if either is over budget.
#
#   boot-test.sh ISO [OUTDIR]
#
# Set SENGI_UEFI to an OVMF firmware image to boot through UEFI instead of BIOS.
# Set SENGI_PREVIEW=1 to boot the sengi-compositor preview entry instead of
# the default one.
#
# The ISO is attached as a disk, the way a flashed USB stick shows up. The
# guest reports through its serial port (see sengi-boot-report). Boot time is
# wall clock from QEMU start to the desktop being up, so it includes the
# firmware and boot loader.
set -eu

iso=${1:?usage: boot-test.sh ISO [OUTDIR]}
out=${2:-test-output}
budget_boot_s=${SENGI_BUDGET_BOOT_S:-10}
budget_ram_mb=${SENGI_BUDGET_RAM_MB:-400}
timeout_s=${SENGI_BOOT_TIMEOUT_S:-300}

# Guest display size, needed to turn pixels into pointer coordinates.
screen_w=1280
screen_h=800

mkdir -p "$out"
out=$(cd "$out" && pwd)
rm -f "$out/serial.log" "$out"/*.png "$out/metrics.txt" \
	"$out/mon.in" "$out/qmp.in" "$out/qmp.out" "$out/firmware.log"
: >"$out/serial.log"
mkfifo "$out/mon.in" "$out/qmp.in" "$out/qmp.out"

accel=tcg cpu=max
if [ -w /dev/kvm ]; then accel=kvm cpu=host; fi

now() { date +%s.%N; }

# The preview entry is reached the way a person reaches it: by its hotkey in
# the hidden boot menu. GRUB looks for a key once, and a key pressed before
# GRUB runs does not count, so the firmware's own log is watched and the key
# is pressed many times a second from the moment SeaBIOS hands over to the
# disk. It has to stop before Linux looks for the keyboard, or the guest can
# end up without one; the kernel is known to have started when the CPU is in
# long mode. The guest says which compositor it was asked for, and that is
# checked at the end. None of this has a counterpart under UEFI yet.
preview=${SENGI_PREVIEW:-0}
want=labwc
handover=
if [ "$preview" = 1 ]; then
	if [ -n "${SENGI_UEFI:-}" ]; then
		echo "the preview entry can only be picked under BIOS so far" >&2
		exit 2
	fi
	want=sengi-compositor
	handover='Booting from 0000:7c00'
	: >"$out/firmware.log"
fi

# Read the ISO once so the host's disk cache does not count against the guest.
cat "$iso" >/dev/null

# The sound card plays into nothing. It is there so PipeWire finds a sink and
# the bar shows its volume item, which it hides on a machine with no audio.
#
# The monitor's mouse_move only sends relative motion, and the guest ignores
# that anyway once it has switched the PS/2 mouse to the absolute VMware
# protocol, which QEMU's PC machines offer by default. So the pointer is placed
# with absolute coordinates over a second, QMP, channel. No extra device needed.
start=$(now)
qemu-system-x86_64 -machine q35,accel=$accel -cpu $cpu -smp 2 -m 4096 \
	-drive file="$iso",format=raw,if=virtio,readonly=on \
	${SENGI_UEFI:+-bios "$SENGI_UEFI"} \
	-device virtio-vga -display none \
	-audiodev none,id=snd -device intel-hda -device hda-output,audiodev=snd \
	-nic user,model=virtio-net-pci \
	-smbios type=11,value=io.systemd.credential:sengi.test=1 \
	${SENGI_TEST_DEBUG:+-smbios "type=11,value=io.systemd.credential:sengi.test.debug=$SENGI_TEST_DEBUG"} \
	-serial file:"$out/serial.log" \
	${handover:+-chardev file,id=firmware,path="$out/firmware.log" -device isa-debugcon,iobase=0x402,chardev=firmware} \
	-qmp pipe:"$out/qmp" \
	-monitor stdio <"$out/mon.in" >"$out/monitor.log" 2>&1 &
qemu=$!
exec 3>"$out/mon.in"
# QEMU blocks once the reply pipe fills up, so keep it drained.
cat "$out/qmp.out" >"$out/qmp.log" &
exec 4>"$out/qmp.in"
echo '{"execute":"qmp_capabilities"}' >&4

stop() {
	echo quit >&3 2>/dev/null || true
	exec 3>&-
	exec 4>&-
	wait "$qemu" 2>/dev/null || true
	rm -f "$out/mon.in" "$out/qmp.in" "$out/qmp.out"
}

wait_for() {
	while ! grep -q "$1" "$out/serial.log"; do
		kill -0 "$qemu" 2>/dev/null || { echo "qemu exited early" >&2; return 1; }
		elapsed=$(echo "$(now) $start" | awk '{printf "%d", $1 - $2}')
		[ "$elapsed" -lt "$timeout_s" ] || { echo "timed out waiting for $1" >&2; return 1; }
		sleep 0.1
	done
}

if [ "$preview" = 1 ]; then
	i=0
	until grep -q "$handover" "$out/firmware.log" || [ $i -ge 1500 ]; do
		sleep 0.02
		i=$((i + 1))
	done
	i=0
	while [ $i -lt 300 ]; do
		echo "sendkey p" >&3
		echo "info registers" >&3
		sleep 0.03
		# EFER is zero in GRUB and never again once the kernel runs.
		if grep -q 'EFER=0*[1-9a-f]' "$out/monitor.log"; then break; fi
		i=$((i + 1))
	done
fi

if ! wait_for SENGI-BOOT; then
	echo "screendump $out/desktop.png -f png" >&3
	sleep 2
	stop
	exit 1
fi
boot_s=$(echo "$(now) $start" | awk '{printf "%.1f", $1 - $2}')

wait_for SENGI-DONE || { stop; exit 1; }

# The measurements are in. Now drive the desktop a little so the reviewer has
# more than an empty screen to look at.
shot() {
	echo "screendump $out/$1.png -f png" >&3
	sleep 1
}
key() {
	echo "sendkey $1" >&3
	sleep "${2:-2}"
}
# Put the pointer on a pixel. QMP wants each axis scaled to 0..32767.
pointer() {
	printf '{"execute":"input-send-event","arguments":{"events":[{"type":"abs","data":{"axis":"x","value":%d}},{"type":"abs","data":{"axis":"y","value":%d}}]}}\n' \
		$(($1 * 32767 / (screen_w - 1))) $(($2 * 32767 / (screen_h - 1))) >&4
	sleep 1
}
click() {
	echo "mouse_button 1" >&3
	sleep 0.2
	echo "mouse_button 0" >&3
	sleep 2
}
shot desktop

# Without an absolute pointer the next two shots would quietly show nothing.
echo "info mice" >&3
sleep 1
grep -q '(absolute)' "$out/monitor.log" ||
	echo "warning: guest has no absolute pointer, hover and click shots will miss" >&2

# The power icon at the right end of the bar: hover it, then open its menu.
pointer $((screen_w - 20)) 14
shot hover
click
shot session-menu
key esc 1

# The network item, a little left of the clock: it opens quick settings.
pointer $((screen_w - 293)) 14
click
sleep 1
shot quick-settings
key esc 1
# Park the pointer on empty desktop so the hover does not follow us around.
pointer $((screen_w / 8)) $((screen_h / 2))

key meta_l-ret 3
shot terminal
key meta_l-spc
shot launcher
key esc 1
key meta_l-l
shot lock
stop

guest_s=$(sed -n 's/.*SENGI-BOOT desktop_s=\([0-9.]*\).*/\1/p' "$out/serial.log" | head -1)
ram_mb=$(sed -n 's/.*SENGI-IDLE ram_mb=\([0-9]*\).*/\1/p' "$out/serial.log" | head -1)
got=$(sed -n 's/.*SENGI-INFO compositor: \([a-z-]*\).*/\1/p' "$out/serial.log" | head -1)

{
	echo "boot_s=$boot_s"
	echo "boot_guest_s=$guest_s"
	echo "idle_ram_mb=$ram_mb"
	echo "accel=$accel"
	echo "compositor=$got"
} >"$out/metrics.txt"
cat "$out/metrics.txt"
grep SENGI-INFO "$out/serial.log" | sed 's/.*SENGI-INFO //' >"$out/guest-info.txt" || true

[ -s "$out/desktop.png" ] || { echo "no screenshot was saved" >&2; exit 1; }

fail=0
if [ "$got" != "$want" ]; then
	echo "FAIL: the guest booted for ${got:-an unknown compositor}, not $want" >&2
	fail=1
fi
if grep -q SENGI-FAIL "$out/serial.log"; then
	sed -n 's/.*SENGI-FAIL /FAIL: /p' "$out/serial.log" >&2
	fail=1
fi
if [ "$ram_mb" -ge "$budget_ram_mb" ]; then
	echo "FAIL: idle RAM ${ram_mb} MB is over the ${budget_ram_mb} MB budget" >&2
	fail=1
fi
if [ "$(echo "$boot_s $budget_boot_s" | awk '{print ($1 >= $2)}')" = 1 ]; then
	echo "FAIL: boot took ${boot_s} s, budget is ${budget_boot_s} s" >&2
	fail=1
fi
exit $fail
