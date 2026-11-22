#!/bin/sh
# Boot an ISO headless in QEMU, wait for the desktop, take a screenshot and
# record boot time and idle RAM. Fails if either is over budget.
#
#   boot-test.sh ISO [OUTDIR]
#
# Set SENGI_UEFI to an OVMF firmware image to boot through UEFI instead of BIOS.
# Set SENGI_COMPOSITOR=own to run the session on sengi-compositor: the guest
# is handed the file that sengi-session looks for, under either firmware.
# Set SENGI_PREVIEW=1 to get there through the boot menu's preview entry
# instead, the way a person does (BIOS only).
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
rm -f "$out/serial.log" "$out"/*.png "$out"/*.ppm "$out/metrics.txt" \
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
# systemd-tmpfiles makes the file early in the start, from a credential
# the firmware tables carry. No comma may be in it.
choice=
if [ "${SENGI_COMPOSITOR:-}" = own ]; then
	want=sengi-compositor
	choice='io.systemd.credential:tmpfiles.extra=f /run/sengi-os.compositor 0644 root root - own'
fi
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
	${choice:+-smbios "type=11,value=$choice"} \
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

# The same for something that should take seconds: wait at most $2 of them.
wait_serial() {
	tries=$(($2 * 10))
	while ! grep -q "$1" "$out/serial.log"; do
		tries=$((tries - 1))
		[ "$tries" -gt 0 ] || return 1
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

# Settings, found and started the way a person would, through the launcher,
# then closed again. The guest says when the process is there and when it
# has gone (see sengi-boot-report). A process is not a window, so the screen
# also has to have changed over an area that only a window accounts for:
# raw dumps of it before and after are compared byte by byte.
settings_fail=
window_bytes=300000
echo "screendump $out/before.ppm" >&3
sleep 1
key meta_l-spc 2
for letter in s e t t i n g s; do
	key "$letter" 0.3
done
key ret 1
if wait_serial 'SENGI-SETTINGS running' 20; then
	sleep 3
	shot settings
	echo "screendump $out/with.ppm" >&3
	sleep 1
	changed=$(cmp -l "$out/before.ppm" "$out/with.ppm" 2>/dev/null | wc -l)
	if [ "$changed" -lt "$window_bytes" ]; then
		settings_fail="sengi-settings started but no window appeared ($changed bytes of the screen changed)"
	fi
	key meta_l-q 1
	wait_serial 'SENGI-SETTINGS gone' 10 ||
		settings_fail="sengi-settings was still running 10 s after its window was closed"
else
	shot settings
	settings_fail="sengi-settings did not start from the launcher"
	key esc 1
fi
rm -f "$out/before.ppm" "$out/with.ppm"

key meta_l-ret 3
shot terminal
key meta_l-spc
shot launcher
key esc 1

# The lock screen: lock, get it wrong, get it right. The guest says when
# sengi-lock is there and when it has gone. What the screen shows is
# compared with a dump of the desktop from just before: locked, most of it
# has to differ, and unlocked it has to be the desktop again.
lock_fail=
# The lock screen has the desktop's own colour behind it. What tells the
# two apart is the bar, the dock and the terminal that is open, and the
# bar alone is over a hundred thousand bytes of a dump.
covered_bytes=100000
differs() {
	cmp -l "$out/$1.ppm" "$out/$2.ppm" 2>/dev/null | wc -l
}
dump() {
	echo "screendump $out/$1.ppm" >&3
	sleep 1
}
# The lock screen's clock redraws itself when the minute turns, so "the
# screen changed" says nothing about the prompt. This looks only at a band
# across the screen below the date, where the name, the field and the line
# under the field are. A dump is a short header and then three bytes a
# pixel, row after row.
band_top=$((screen_h * 54 / 100))
band_rows=$((screen_h * 24 / 100))
band() {
	size=$(wc -c <"$out/$1.ppm")
	header=$((size - screen_w * screen_h * 3))
	tail -c +$((header + band_top * screen_w * 3 + 1)) "$out/$1.ppm" |
		head -c $((band_rows * screen_w * 3)) >"$out/$1.band"
}
prompt_differs() {
	band "$1"
	band "$2"
	cmp -l "$out/$1.band" "$out/$2.band" 2>/dev/null | wc -l
}
# PAM logs a refusal and then sits on it for a couple of seconds before it
# tells the program that asked. So the line in the journal is not yet the
# message on the screen, and keys typed in between are dropped. This
# watches the band until it no longer looks as it did while the password
# was being checked ($1), for ten seconds at most. The dump it ends on is
# $2.
settle() {
	waits=10
	while dump "$2" && [ "$(prompt_differs "$1" "$2")" -eq 0 ] && [ "$waits" -gt 0 ]; do
		waits=$((waits - 1))
	done
}
dump unlocked
key meta_l-l 1
if wait_serial 'SENGI-LOCK running' 10; then
	sleep 1
	shot lock
	dump locked
	if [ "$(differs unlocked locked)" -lt "$covered_bytes" ]; then
		lock_fail="sengi-lock is running but the desktop is still on screen"
	fi
	key caps_lock 1
	shot lock-caps
	dump caps
	key caps_lock 1
	if [ "$(prompt_differs locked caps)" -eq 0 ]; then
		lock_fail="the lock screen does not show that Caps Lock is on"
	fi
	dump locked
	for letter in w r o n g; do
		key "$letter" 0.3
	done
	key ret 0.5
	dump checking
	# How long PAM takes to say no is PAM's business. The guest says when
	# it has (see sengi-boot-report), and the screen says when the lock
	# screen has heard; only then is it looked at, and only then is the
	# next password typed.
	wait_serial 'SENGI-LOCK refused' 30 ||
		lock_fail="PAM did not turn the wrong password down within 30 s"
	settle checking wrong
	shot lock-wrong
	if grep -q 'SENGI-LOCK gone' "$out/serial.log"; then
		lock_fail="a wrong password unlocked the screen"
	elif [ "$(differs unlocked wrong)" -lt "$covered_bytes" ]; then
		lock_fail="the desktop is on screen after a wrong password"
	elif [ "$(prompt_differs locked wrong)" -eq 0 ]; then
		lock_fail="the lock screen shows nothing about a wrong password"
	fi
	for letter in l i v e; do
		key "$letter" 0.3
	done
	key ret 1
	if wait_serial 'SENGI-LOCK gone' 15; then
		sleep 1
		shot unlocked
		dump again
		# The bar's clock may have moved on a minute; nothing else has.
		if [ -z "$lock_fail" ] && [ "$(differs unlocked again)" -ge 20000 ]; then
			lock_fail="the desktop did not come back after unlocking"
		fi
	else
		shot unlocked
		lock_fail=${lock_fail:-"the password of the live user did not unlock the screen"}
	fi
else
	shot lock
	lock_fail="sengi-lock did not start on Super+L"
fi

# What Settings drives, changed without logging out. The guest makes each
# change the way the Settings window does and says what came of it (see
# sengi-boot-report); here the keys are pressed and the answers judged.
drives_fail=
drives() {
	drives_fail=${drives_fail:-$1}
}
# A second keyboard layout, German, and Alt+Shift to switch to it. The
# terminal still has the keyboard. On the German layout the Z key types
# a y, and the guest sees which file "touch z" made.
if wait_serial 'SENGI-KEYBOARD set exit=0' 60; then
	sleep 1
	key alt-shift 1
	for letter in t o u c h spc z; do
		key "$letter" 0.3
	done
	key ret 1
	shot keyboard-layout
	wait_serial 'SENGI-KEYBOARD typed=' 40 || true
	typed=$(tr -d '\r' <"$out/serial.log" | sed -n 's/.*SENGI-KEYBOARD typed=//p' | head -1)
	[ "$typed" = y ] ||
		drives "with a German layout switched to, the Z key typed ${typed:-nothing}, not y"
else
	drives "a second keyboard layout could not be set through sengi-settings"
fi
# A pointer setting. Only sengi-compositor can be asked what its devices
# are set to.
if wait_serial 'SENGI-POINTER done' 40; then
	grep -q 'SENGI-POINTER set exit=0' "$out/serial.log" ||
		drives "natural scrolling could not be set through sengi-settings"
	if [ "$want" = sengi-compositor ] &&
		! grep -q 'SENGI-POINTER device .*natural scroll on' "$out/serial.log"; then
		drives "no pointing device took natural scrolling up"
	fi
else
	drives "the guest did not get as far as the pointer setting"
fi
# The screen off and on. Under sengi-compositor a key has to light it.
if wait_serial 'SENGI-SCREEN asked' 40; then
	asked=$(tr -d '\r' <"$out/serial.log" | sed -n 's/.*SENGI-SCREEN asked //p' | head -1)
	case $asked in
	"exit=0 dpms=Off"*) ;;
	*) drives "asked to switch the screen off: $asked" ;;
	esac
	sleep 1
	shot screen-off
	key shift 1
	wait_serial 'SENGI-SCREEN lit' 30 || true
	lit=$(tr -d '\r' <"$out/serial.log" | sed -n 's/.*SENGI-SCREEN lit //p' | head -1)
	case $lit in
	"by=key dpms=On"*) ;;
	"by=request dpms=On"*)
		[ "$want" = labwc ] || drives "a key did not light the screen again: $lit"
		;;
	*) drives "the screen did not come back on: ${lit:-no answer}" ;;
	esac
	sleep 1
	shot screen-on
else
	drives "the guest did not get as far as switching the screen off"
fi

# The login screen: log out through the session menu, get the password
# wrong, get it right. The guest says when the login screen and its
# compositor are up, when PAM has refused, and what is running once the
# login screen has gone (see sengi-boot-report). The screen is judged by
# the same band as the lock screen's: it has to change with the first key
# typed, which a clock that turned a minute, a blank screen or a desktop
# that never went away would not do, and again with the refusal.
greeter_fail=
dump session
pointer $((screen_w - 20)) 14
click
	# The key only when the guest has seen the screen stay off by itself.
	if wait_serial 'SENGI-SCREEN still' 20; then
		still=$(tr -d '\r' <"$out/serial.log" | sed -n 's/.*SENGI-SCREEN still //p' | head -1)
		case $still in
		dpms=Off*) ;;
		*) drives "the screen did not stay off: $still" ;;
		esac
	else
		drives "the guest did not say whether the screen stayed off"
	fi
for letter in l o g; do
	key "$letter" 0.3
done
key ret 1
pointer $((screen_w / 8)) $((screen_h / 2))
if wait_serial 'SENGI-GREETER ready' 60; then
	sleep 1
	shot greeter
	dump greeter
	if [ "$(differs session greeter)" -lt "$covered_bytes" ]; then
		greeter_fail="sengi-greeter is running but the desktop is still on screen"
	fi
	key w 0.5
	dump first
	if [ "$(prompt_differs greeter first)" -eq 0 ]; then
		greeter_fail="the login screen shows nothing for the first key typed"
	fi
	for letter in r o n g; do
		key "$letter" 0.3
	done
	key ret 0.5
	dump checking
	wait_serial 'SENGI-GREETER refused' 30 ||
		greeter_fail="PAM did not turn the wrong password down within 30 s"
	settle checking wrong
	shot greeter-wrong
	if grep -q 'SENGI-GREETER gone' "$out/serial.log"; then
		greeter_fail="a wrong password logged in"
	elif [ "$(prompt_differs greeter wrong)" -eq 0 ]; then
		greeter_fail="the login screen shows nothing about a wrong password"
	fi
	for letter in l i v e; do
		key "$letter" 0.3
	done
	key ret 1
	if wait_serial 'SENGI-GREETER after' 60; then
		sleep 1
		shot logged-in
		dump back
		# The serial line ends in a carriage return.
		after=$(tr -d '\r' <"$out/serial.log" | sed -n 's/.*SENGI-GREETER after //p' | head -1)
		if [ -z "$greeter_fail" ] && [ "$after" != "desktop=up session=live left=nothing" ]; then
			greeter_fail="after logging in as live: $after"
		elif [ -z "$greeter_fail" ] && [ "$(differs greeter back)" -lt "$covered_bytes" ]; then
			greeter_fail="the desktop is not on screen after logging in"
		fi
	else
		shot logged-in
		greeter_fail=${greeter_fail:-"the password of the live user did not log in"}
	fi
else
	shot greeter
	greeter_fail="the login screen did not appear after logging out"
fi
rm -f "$out"/*.ppm "$out"/*.band
stop

guest_s=$(sed -n 's/.*SENGI-BOOT desktop_s=\([0-9.]*\).*/\1/p' "$out/serial.log" | head -1)
ram_mb=$(sed -n 's/.*SENGI-IDLE ram_mb=\([0-9]*\).*/\1/p' "$out/serial.log" | head -1)
got=$(sed -n 's/.*SENGI-INFO compositor: \([a-z-]*\).*/\1/p' "$out/serial.log" | head -1)
# What two things cost that are not the desktop's own. The login screen
# and its compositor are there only while nobody is logged in, so they
# are no part of the idle figure. polkitd is: logind calls for it when
# the session starts (see sengi-boot-report), and it stays.
greeter_kb=$(sed -n 's/.*SENGI-GREETER running pss_kb=\([0-9]*\).*/\1/p' "$out/serial.log" | head -1)
polkit_kb=$(sed -n 's/.*SENGI-POLKIT pss_kb=\([0-9]*\).*/\1/p' "$out/serial.log" | head -1)

{
	echo "boot_s=$boot_s"
	echo "boot_guest_s=$guest_s"
	echo "idle_ram_mb=$ram_mb"
	echo "greeter_pss_kb=$greeter_kb"
	echo "polkitd_pss_kb=$polkit_kb"
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
if [ -n "$settings_fail" ]; then
	echo "FAIL: $settings_fail" >&2
	fail=1
fi
if [ -n "$lock_fail" ]; then
	echo "FAIL: $lock_fail" >&2
	fail=1
fi
if [ -n "$drives_fail" ]; then
	echo "FAIL: $drives_fail" >&2
	fail=1
fi
if [ -n "$greeter_fail" ]; then
	echo "FAIL: $greeter_fail" >&2
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
if [ -n "$forward_fail" ]; then
	echo "FAIL: $forward_fail" >&2
	fail=1
fi
