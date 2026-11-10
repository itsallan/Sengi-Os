#!/bin/sh
# Boot an ISO headless in QEMU, wait for the desktop, take a screenshot and
# record boot time and idle RAM. Fails if either is over budget.
#
#   boot-test.sh ISO [OUTDIR]
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

mkdir -p "$out"
out=$(cd "$out" && pwd)
rm -f "$out/serial.log" "$out/desktop.png" "$out/metrics.txt" "$out/mon.in"
: >"$out/serial.log"
mkfifo "$out/mon.in"

accel=tcg cpu=max
if [ -w /dev/kvm ]; then accel=kvm cpu=host; fi

now() { date +%s.%N; }

start=$(now)
qemu-system-x86_64 -machine q35,accel=$accel -cpu $cpu -smp 2 -m 4096 \
	-drive file="$iso",format=raw,if=virtio,readonly=on \
	-device virtio-vga -display none \
	-nic user,model=virtio-net-pci \
	-smbios type=11,value=io.systemd.credential:sengi.test=1 \
	-serial file:"$out/serial.log" \
	-monitor stdio <"$out/mon.in" >"$out/monitor.log" 2>&1 &
qemu=$!
exec 3>"$out/mon.in"

stop() {
	echo quit >&3 2>/dev/null || true
	exec 3>&-
	wait "$qemu" 2>/dev/null || true
	rm -f "$out/mon.in"
}

wait_for() {
	while ! grep -q "$1" "$out/serial.log"; do
		kill -0 "$qemu" 2>/dev/null || { echo "qemu exited early" >&2; return 1; }
		elapsed=$(echo "$(now) $start" | awk '{printf "%d", $1 - $2}')
		[ "$elapsed" -lt "$timeout_s" ] || { echo "timed out waiting for $1" >&2; return 1; }
		sleep 0.1
	done
}

if ! wait_for SENGI-BOOT; then
	echo "screendump $out/desktop.png -f png" >&3
	sleep 2
	stop
	exit 1
fi
boot_s=$(echo "$(now) $start" | awk '{printf "%.1f", $1 - $2}')

wait_for SENGI-DONE || { stop; exit 1; }
echo "screendump $out/desktop.png -f png" >&3
sleep 2
stop

guest_s=$(sed -n 's/.*SENGI-BOOT desktop_s=\([0-9.]*\).*/\1/p' "$out/serial.log" | head -1)
ram_mb=$(sed -n 's/.*SENGI-IDLE ram_mb=\([0-9]*\).*/\1/p' "$out/serial.log" | head -1)

{
	echo "boot_s=$boot_s"
	echo "boot_guest_s=$guest_s"
	echo "idle_ram_mb=$ram_mb"
	echo "accel=$accel"
} >"$out/metrics.txt"
cat "$out/metrics.txt"
grep SENGI-INFO "$out/serial.log" | sed 's/.*SENGI-INFO //' >"$out/guest-info.txt" || true

[ -s "$out/desktop.png" ] || { echo "no screenshot was saved" >&2; exit 1; }

fail=0
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
