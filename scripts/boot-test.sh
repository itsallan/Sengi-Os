#!/bin/sh
# Boot an ISO headless in QEMU, wait for the desktop, take a screenshot and
# record boot time and idle RAM. Fails if either is over budget.
#
#   boot-test.sh ISO [OUTDIR]
#
# Set SENGI_UEFI to an OVMF firmware image to boot through UEFI instead of BIOS.
# The default entry is run, and the session is expected on sengi-compositor,
# the default compositor.
# Set SENGI_COMPOSITOR=labwc to run the session on labwc: the guest is handed
# the file that sengi-session looks for, under either firmware.
# Set SENGI_LABWC_ENTRY=1 to get to labwc through the boot menu's labwc
# entry (hotkey L) instead, the way a person does (BIOS only).
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
# SENGI_SCREEN=WxH runs the same test at another size, for small screens.
screen=${SENGI_SCREEN:-1280x800}
screen_w=${screen%x*}
screen_h=${screen#*x}
if ! printf "%s" "$screen" | grep -Eq "^[0-9]+x[0-9]+\$"; then
	echo "usage: SENGI_SCREEN=WIDTHxHEIGHT, for example 800x600 (got '$screen')" >&2
	exit 2
fi

mkdir -p "$out"
out=$(cd "$out" && pwd)
rm -f "$out/serial.log" "$out"/*.png "$out"/*.ppm "$out/metrics.txt" \
	"$out/mon.in" "$out/qmp.in" "$out/qmp.out" "$out/firmware.log" "$out/audio.wav"
: >"$out/serial.log"
mkfifo "$out/mon.in" "$out/qmp.in" "$out/qmp.out"

accel=tcg cpu=max
if [ -w /dev/kvm ]; then accel=kvm cpu=host; fi

now() { date +%s.%N; }

# The labwc entry is reached the way a person reaches it: by its hotkey in
# the hidden boot menu. GRUB looks for a key once, and a key pressed before
# GRUB runs does not count, so the firmware's own log is watched and the key
# is pressed many times a second from the moment SeaBIOS hands over to the
# disk. It has to stop before Linux looks for the keyboard, or the guest can
# end up without one; the kernel is known to have started when the CPU is in
# long mode. The guest says which compositor it was asked for, and that is
# checked at the end. None of this has a counterpart under UEFI yet.
preview=${SENGI_LABWC_ENTRY:-0}
want=sengi-compositor
handover=
# systemd-tmpfiles makes the file early in the start, from a credential
# the firmware tables carry. No comma may be in it.
choice=
case "${SENGI_COMPOSITOR:-}" in
'' | own) ;;
labwc)
	want=labwc
	choice='io.systemd.credential:tmpfiles.extra=f /run/sengi-os.compositor 0644 root root - labwc'
	;;
*)
	echo "SENGI_COMPOSITOR is own or labwc" >&2
	exit 2
	;;
esac
if [ "$preview" = 1 ]; then
	if [ -n "${SENGI_UEFI:-}" ]; then
		echo "the labwc entry can only be picked under BIOS so far" >&2
		exit 2
	fi
	want=labwc
	handover='Booting from 0000:7c00'
	: >"$out/firmware.log"
fi

# Read the ISO once so the host's disk cache does not count against the guest.
cat "$iso" >/dev/null

# The sound card is an Intel HDA one with line out and line in, and what the
# guest plays on it is written to a .wav, which is listened to at the end.
#
# The monitor's mouse_move only sends relative motion, and the guest ignores
# that anyway once it has switched the PS/2 mouse to the absolute VMware
# protocol, which QEMU's PC machines offer by default. So the pointer is placed
# with absolute coordinates over a second, QMP, channel. No extra device needed.
start=$(now)
qemu-system-x86_64 -machine q35,accel=$accel -cpu $cpu -smp 2 -m 4096 \
	-drive file="$iso",format=raw,if=virtio,readonly=on \
	${SENGI_UEFI:+-bios "$SENGI_UEFI"} \
	-device "virtio-vga${SENGI_SCREEN:+,xres=$screen_w,yres=$screen_h}" -display none \
	-audiodev wav,id=snd,path="$out/audio.wav",out.frequency=48000 \
	-device intel-hda -device hda-duplex,audiodev=snd \
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
		echo "sendkey l" >&3
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
# In mouse_button's mask 2 is the right button.
right_click() {
	echo "mouse_button 2" >&3
	sleep 0.2
	echo "mouse_button 0" >&3
	sleep 1
}
# A raw dump of the screen, and how many bytes of two of them differ.
dump() {
	echo "screendump $out/$1.ppm" >&3
	sleep 1
}
differs() {
	cmp -l "$out/$1.ppm" "$out/$2.ppm" 2>/dev/null | wc -l
}
# The same for $4 rows of the screen from row $3. Rows of a dump are three
# bytes a pixel after a short header.
strip() {
	size=$(wc -c <"$out/$1.ppm")
	header=$((size - screen_w * screen_h * 3))
	tail -c +$((header + $2 * screen_w * 3 + 1)) "$out/$1.ppm" |
		head -c $(($3 * screen_w * 3)) >"$out/$1.strip"
}
strip_differs() {
	strip "$1" "$3" "$4"
	strip "$2" "$3" "$4"
	cmp -l "$out/$1.strip" "$out/$2.strip" 2>/dev/null | wc -l
}

# The live image asks whether to install Sengi OS or try it first. The guest
# says when it has been up and left alone, with what memory is used then. It
# has to be a window in the middle of the screen that has the keyboard: Tab
# goes to "Try it first" and Return chooses it. The steps after this need the
# desktop to themselves.
dialog_fail=
# The box around what differs between dumps $1 and $2, clear of the bar's
# clock and of the dock, as "left right top bottom".
changed_box() {
	header=$(($(wc -c <"$out/$1.ppm") - screen_w * screen_h * 3))
	cmp -l "$out/$1.ppm" "$out/$2.ppm" 2>/dev/null |
		awk -v h="$header" -v w="$screen_w" -v low=30 -v high=$((screen_h - 100)) '
		{
			p = int(($1 - 1 - h) / 3)
			x = p % w
			y = int(p / w)
			if (p < 0 || y < low || y >= high) next
			if (!n++) { l = r = x; t = b = y }
			if (x < l) l = x
			if (x > r) r = x
			if (y > b) b = y
		}
		END { if (n) print l, r, t, b }'
}
# "SENGI-DIALOG up", or "missing", in which case the guest has said so.
wait_for 'SENGI-DIALOG [um]' || { shot live-dialog; stop; exit 1; }
if grep -q 'SENGI-DIALOG up' "$out/serial.log"; then
	pointer $((screen_w / 8)) $((screen_h / 2))
	shot live-dialog
	echo "screendump $out/asking.ppm" >&3
	sleep 1
	key tab 1
	key ret 1
	if wait_serial 'SENGI-DIALOG gone' 15; then
		sleep 2
		echo "screendump $out/answered.ppm" >&3
		sleep 1
		box=$(changed_box asking answered)
		echo "live question covered (left right top bottom): ${box:-nothing}" >&2
		# 440 by 192 from the tokens, with a title bar and a shadow, in the
		# middle across and near the middle down.
		if ! echo "$box" | awk -v w="$screen_w" -v h="$screen_h" '{
			wide = $2 - $1
			tall = $4 - $3
			across = ($1 + $2) / 2 - w / 2
			down = ($3 + $4) / 2 - h / 2
			exit !(NF == 4 && wide >= 400 && wide <= 520 && tall >= 180 && tall <= 320 &&
				across >= -20 && across <= 20 && down >= -80 && down <= 80)
		}'; then
			dialog_fail="the live question is not a window in the middle of the screen (changed box: ${box:-nothing})"
		fi
	else
		dialog_fail="Tab and Return did not close the live question, so it did not have the keyboard"
		shot live-dialog-stuck
		key esc 2
	fi
else
	shot live-dialog
	dialog_fail="the live question did not appear"
fi

# After the measurements the guest plays its tones (see sengi-boot-report),
# and then waits for the volume up key.
wait_for 'SENGI-SOUND keys ready\|SENGI-DONE' || { stop; exit 1; }
if ! grep -q SENGI-DONE "$out/serial.log"; then
	key volumeup 0.7
	shot volume-key
fi
wait_for SENGI-DONE || { stop; exit 1; }

# The measurements are in. Now drive the desktop a little so the reviewer has
# more than an empty screen to look at.
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

# The desktop's own menu. A right click on bare desktop, clear of the bar and
# the dock, opens New Folder, Open Terminal Here and Settings; a left click
# somewhere else closes it; New Folder makes a folder that is in rename, and
# Escape and Delete then take it away again (into the Trash). Only the screen
# can say, so raw dumps are compared. QEMU's mouse_button takes a bitmask in
# which 1 is the left button, 2 the right and 4 the middle one.
desktop_fail=
# Measured at 1280x800: the menu changes 42398 bytes of a dump, the new folder
# 20954, and each way back to the first dump 0 or, when the minute ticks over, about 4000 for the clock digits, so the limits are about half of
# what each should change and a few clock digits for what should not.
desktop_menu_bytes=20000
desktop_folder_bytes=10000
desktop_clock_bytes=8000
desktop_x=$((screen_w / 2))
desktop_y=$((screen_h / 3))
desktop_open_menu() {
	pointer "$desktop_x" "$desktop_y"
	echo "mouse_button 2" >&3
	sleep 0.2
	echo "mouse_button 0" >&3
	sleep 2
}
echo "screendump $out/before.ppm" >&3
sleep 1
desktop_open_menu
shot desktop-menu
echo "screendump $out/with.ppm" >&3
sleep 1
changed=$(cmp -l "$out/before.ppm" "$out/with.ppm" 2>/dev/null | wc -l)
echo "desktop menu: $changed bytes changed" >&2
if [ "$changed" -lt "$desktop_menu_bytes" ]; then
	desktop_fail="a right click on the desktop opened no menu ($changed bytes of the screen changed)"
fi
pointer $((screen_w * 3 / 4)) $((screen_h * 2 / 3))
click
# Back where the first dump had it, or the two cursors count as a change.
pointer $((screen_w / 8)) $((screen_h / 2))
echo "screendump $out/after.ppm" >&3
sleep 1
changed=$(cmp -l "$out/before.ppm" "$out/after.ppm" 2>/dev/null | wc -l)
echo "desktop menu closed: $changed bytes differ from before" >&2
if [ -z "$desktop_fail" ] && [ "$changed" -ge "$desktop_clock_bytes" ]; then
	desktop_fail="the desktop menu did not close on a left click ($changed bytes differ from before)"
fi
# New Folder and New File are the first two rows: 6 px of padding, then
# 22 px rows, so the middles are 17 and 39 px below the click.
for made in "folder 17" "file 39"; do
	kind=${made% *}
	row=${made#* }
	[ -z "$desktop_fail" ] || break
	desktop_open_menu
	pointer $((desktop_x + 40)) $((desktop_y + row))
	click
	shot "desktop-new-$kind"
	echo "screendump $out/with.ppm" >&3
	sleep 1
	changed=$(cmp -l "$out/before.ppm" "$out/with.ppm" 2>/dev/null | wc -l)
	echo "desktop new $kind: $changed bytes changed" >&2
	if [ "$changed" -lt "$desktop_folder_bytes" ]; then
		desktop_fail="New ${kind} in the desktop menu put nothing on the desktop ($changed bytes of the screen changed)"
	fi
	key esc 1
	key delete 2
	pointer $((screen_w / 8)) $((screen_h / 2))
	echo "screendump $out/after.ppm" >&3
	sleep 1
	changed=$(cmp -l "$out/before.ppm" "$out/after.ppm" 2>/dev/null | wc -l)
	echo "desktop $kind deleted: $changed bytes differ from before" >&2
	if [ -z "$desktop_fail" ] && [ "$changed" -ge "$desktop_clock_bytes" ]; then
		desktop_fail="Delete did not take the new $kind off the desktop ($changed bytes differ from before)"
	fi
done
rm -f "$out/before.ppm" "$out/with.ppm" "$out/after.ppm"
pointer $((screen_w / 8)) $((screen_h / 2))
# SENGI_STOP_AFTER=desktop ends the run here, to try this step on its own.
if [ "${SENGI_STOP_AFTER:-}" = desktop ]; then
	stop
	[ -z "$desktop_fail" ] || echo "FAIL: $desktop_fail" >&2
	[ -z "$desktop_fail" ]
	exit
fi

# Settings, found and started the way a person would, through the launcher,
# then closed again. The guest says when the process is there and when it
# has gone (see sengi-boot-report). A process is not a window, so the screen
# also has to have changed over an area that only a window accounts for:
# raw dumps of it before and after are compared byte by byte.
settings_fail=
window_bytes=300000
echo "screendump $out/before.ppm" >&3
sleep 1
# The pointer and dock maths assume the guest really runs at $screen.
size_fail=
if [ "$(head -n 2 "$out/before.ppm" 2>/dev/null | tr "\n" " ")" != "P6 $screen_w $screen_h " ]; then
	size_fail="the guest screen is not ${screen_w}x${screen_h}: $(head -n 2 "$out/before.ppm" 2>/dev/null | tr "\n" " ")"
fi
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

# Settings again, this time by clicking its icon in the dock, which is what
# a person with a mouse does. The dock is centred at the bottom edge with
# the search button, the installer, the terminal, Settings and the Store in a
# row, and Settings is the fourth of the five. The icons are one icon width plus one gap
# apart, both from tokens/sengi.toml; their middles are about 43 px above the
# bottom edge, measured on the screenshots (the shelf has the running dot to
# make room for under the icon). A click anywhere on the icon counts. The guest's
# report only watches the first time Settings runs, so here the screen is
# what says whether a window appeared.
dock_token() {
	awk -v key="$1" '/^\[/ { in_dock = ($0 == "[dock]"); next }
		in_dock && $1 == key { print $3; exit }' "$(dirname "$0")/../tokens/sengi.toml"
}
dock_fail=
dock_icon_pitch=$(($(dock_token icon) + $(dock_token gap)))
# The search button comes first and the pinned apps follow in the order of the
# default dock.toml. The row is centred, so icon number i of n sits
# (2 * i - (n - 1)) half a pitch from the middle, with n = pinned + 1.
dock_x_of() {
	dock_pos=$(awk -v id="$1" -F'[][",]+' '/^pinned/ {
		n = 0
		for (i = 2; i <= NF; i++) {
			gsub(/ /, "", $i)
			if ($i == "") continue
			n++
			if ($i == id) p = n
		}
		print p, n
		exit
	}' "$(dirname "$0")/../image/rootfs/etc/xdg/sengi-os/dock.toml")
	dock_idx=${dock_pos% *}
	dock_count=${dock_pos#* }
	echo $((screen_w / 2 + dock_icon_pitch * (2 * dock_idx - dock_count) / 2))
}
dock_x=$(dock_x_of sengi-settings)
dock_y=$((screen_h - 43))
echo "screendump $out/before.ppm" >&3
sleep 1
pointer "$dock_x" "$dock_y"
shot dock-hover
click
sleep 3
shot dock-settings
echo "screendump $out/with.ppm" >&3
sleep 1
changed=$(cmp -l "$out/before.ppm" "$out/with.ppm" 2>/dev/null | wc -l)
if [ "$changed" -lt "$window_bytes" ]; then
	dock_fail="a click on the dock's Settings icon opened no window ($changed bytes of the screen changed)"
	# Whatever the click did open should not stay in the way.
	key esc 1
fi
key meta_l-q 2
pointer $((screen_w / 8)) $((screen_h / 2))
echo "screendump $out/after.ppm" >&3
sleep 1
# The bar's clock may have moved on a minute; nothing else has.
if [ -z "$dock_fail" ] && [ "$(cmp -l "$out/before.ppm" "$out/after.ppm" 2>/dev/null | wc -l)" -ge 20000 ]; then
	dock_fail="the desktop did not come back after Settings was closed from the dock"
fi
rm -f "$out/before.ppm" "$out/with.ppm" "$out/after.ppm"

# The dock's own menu: a right click on an icon, then the pointer taken up to
# a row a little at a time, as a hand takes it, and left there. The menu has
# to stay until Escape; and, opened again, to stay with the pointer far away
# until a click there, which does nothing more. The menu is above the panel,
# which with its margin is 72 px high; its two rows are in the 56 px above a
# gap of 4.
menu_fail=
menu_bytes=5000
# The rows of the screen the menu is on, with nothing of the dock below it.
menu_differs() {
	strip_differs "$1" "$2" $((screen_h - 130)) 50
}
# Right-clicks the icon at $1 and goes up to the lower row of its menu.
open_dock_menu() {
	pointer "$1" "$dock_y"
	right_click
	for up in 66 74 84 93; do
		pointer "$1" $((screen_h - up))
	done
}
park() {
	pointer $((screen_w / 8)) $((screen_h / 2))
}
park
dump menu-none
open_dock_menu "$dock_x"
sleep 2
shot dock-menu
dump menu-open
changed=$(menu_differs menu-none menu-open)
echo "dock menu with the pointer on a row: $changed bytes changed" >&2
[ "$changed" -ge "$menu_bytes" ] ||
	menu_fail="the dock menu did not stay while the pointer went to its row ($changed bytes changed)"
key esc 1
park
dump menu-gone
changed=$(menu_differs menu-none menu-gone)
[ -n "$menu_fail" ] || [ "$changed" -lt 500 ] ||
	menu_fail="Escape did not close the dock menu ($changed bytes differ from before)"
open_dock_menu "$dock_x"
park
sleep 2
dump menu-away
changed=$(menu_differs menu-none menu-away)
[ -n "$menu_fail" ] || [ "$changed" -ge "$menu_bytes" ] ||
	menu_fail="the dock menu went when the pointer left it ($changed bytes changed)"
click
dump menu-clicked
changed=$(menu_differs menu-none menu-clicked)
[ -n "$menu_fail" ] || [ "$changed" -lt 500 ] ||
	menu_fail="a click elsewhere did not close the dock menu ($changed bytes differ from before)"
rm -f "$out"/menu-*.ppm

# The Store, from its icon next to Settings: the same click, one icon further
# on. Closed again with the window shortcut, as Settings is.
store_fail=
store_x=$(dock_x_of sengi-store)
echo "screendump $out/before.ppm" >&3
sleep 1
pointer "$store_x" "$dock_y"
click
sleep 4
shot store
echo "screendump $out/with.ppm" >&3
sleep 1
changed=$(cmp -l "$out/before.ppm" "$out/with.ppm" 2>/dev/null | wc -l)
if [ "$changed" -lt "$window_bytes" ]; then
	store_fail="a click on the dock's Store icon opened no window ($changed bytes of the screen changed)"
	key esc 1
fi

# Search for something. With a network the list of Flathub comes in and the
# matches are shown; without one the window says it cannot reach it. The
# shot shows which, and the test does not depend on it.
if [ -z "$store_fail" ]; then
	for letter in f i r e; do
		key "$letter" 0.3
	done
	sleep 20
	shot store-search
	sleep 30
	shot store-updated
fi
# Only when the window is there: the shortcut would close some other one.
if [ -z "$store_fail" ]; then key meta_l-q 2; fi
pointer $((screen_w / 8)) $((screen_h / 2))
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

# The lock asked for by running sengi-lock, as the idle timer and the
# session menu do. Under sengi-compositor that goes through the
# compositor, which the guest then asks what it holds.
forward_fail=
if wait_serial 'SENGI-FORWARD exit=' 40; then
	sleep 1
	shot lock-forwarded
	forwarded=$(tr -d '\r' <"$out/serial.log" | sed -n 's/.*SENGI-FORWARD exit=//p' | head -1)
	case $forwarded in
	0*) ;;
	*) forward_fail="sengi-lock -f run in the session ended with $forwarded" ;;
	esac
	if [ "$want" = sengi-compositor ]; then
		case $forwarded in
		*"session locked"*"locker told yes"*) ;;
		*) forward_fail=${forward_fail:-"after sengi-lock -f the compositor says: $forwarded"} ;;
		esac
	fi
	for letter in l i v e; do
		key "$letter" 0.3
	done
	key ret 1
	wait_serial 'SENGI-FORWARD unlocked' 20 ||
		forward_fail=${forward_fail:-"the screen locked by sengi-lock -f did not unlock"}
else
	forward_fail="sengi-lock -f run in the session did not come back"
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
	shot screen-off
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

# What programs expect of the compositor. foot says at start what it
# misses; under sengi-compositor those words are in the journal, and
# three of them must be gone while foot still spoke of others.
apps_fail=
apps() {
	apps_fail=${apps_fail:-$1}
}
if wait_serial 'SENGI-FOOT ' 20; then
	foot=$(tr -d '\r' <"$out/serial.log" | sed -n 's/.*SENGI-FOOT //p' | head -1)
	if [ "$want" = sengi-compositor ]; then
		case $foot in
		"spoke=0 "*) apps "foot said nothing of the compositor, so nothing is known: $foot" ;;
		*"missing=0") ;;
		*) apps "foot still misses primary selection, activation or fractional scale: $foot" ;;
		esac
	fi
else
	apps "the guest did not get as far as what foot said"
fi
# A terminal over the whole screen: the bar's rows and the dock's have to
# change, and be as they were once it is closed.
dump plain
if wait_serial 'SENGI-FULLSCREEN up' 30; then
	dump filled
	shot fullscreen
	bar_bytes=$(strip_differs plain filled 0 24)
	dock_bytes=$(strip_differs plain filled $((screen_h - 60)) 50)
	[ "$bar_bytes" -ge 1000 ] ||
		apps "the bar still shows over a fullscreen window ($bar_bytes bytes of it changed)"
	[ "$dock_bytes" -ge 3000 ] ||
		apps "the dock still shows over a fullscreen window ($dock_bytes bytes of it changed)"
	if wait_serial 'SENGI-FULLSCREEN down' 30; then
		sleep 1
		dump emptied
		shot fullscreen-left
		# The bar's clock may have moved on a minute; nothing else has.
		[ "$(differs plain emptied)" -lt 20000 ] ||
			apps "the desktop did not come back after a fullscreen window closed"
	else
		apps "the fullscreen terminal was not closed"
	fi
else
	apps "a terminal could not be started fullscreen"
fi
rm -f "$out"/*.strip

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
	# The login screen follows the same choice of compositor as the session.
	shown_in=$(tr -d '\r' <"$out/serial.log" |
		sed -n 's/.*SENGI-GREETER running .*compositor=\([a-z-]*\).*/\1/p' | head -1)
	[ "$shown_in" = "$want" ] ||
		greeter_fail=${greeter_fail:-"the login screen is shown in ${shown_in:-nothing known}, not $want"}
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
# The guest's last lines, on how the display was handed over. Before them it
# waits to see that the live question is not asked again.
wait_serial 'SENGI-HANDOVER reported' 20 || true
# Several windows, each minimised with its yellow button while another is
# open: this once froze the whole desktop, pointer and all. Only the window
# in front has coloured buttons, so its yellow one is found by that colour in
# a raw dump. The screen has to follow every click, and a window has to come
# back from the dock. Our compositor only: labwc draws buttons of its own.
# It runs last, in the session logged in just now: the guest waits only so
# long for the lock screen's steps, and takes the first Settings it sees for
# the one opened from the launcher.
minimise_fail=
yellow=$(awk '/^\[/ { in_color = ($0 == "[color]"); next }
	in_color && $1 == "window-minimise" { gsub(/[#"]/, "", $3); print $3; exit }' \
	"$(dirname "$0")/../tokens/sengi.toml")
# Where the yellow button is in the dump $1, as "x y"; nothing if there is none.
yellow_button() {
	tail -c $((screen_w * screen_h * 3)) "$out/$1.ppm" | od -An -v -tu1 |
		awk -v w="$screen_w" -v r="$(printf '%d' "0x$(echo "$yellow" | cut -c1-2)")" \
			-v g="$(printf '%d' "0x$(echo "$yellow" | cut -c3-4)")" \
			-v b="$(printf '%d' "0x$(echo "$yellow" | cut -c5-6)")" '
			{
				for (i = 1; i <= NF; i++) {
					c = n % 3
					if (c == 0) red = $i
					else if (c == 1) green = $i
					else if (red == r && green == g && $i == b) {
						x = int(n / 3) % w
						y = int(n / 3 / w)
						if (!found++) { left = right = x; top = bottom = y }
						if (x < left) left = x
						if (x > right) right = x
						if (y > bottom) bottom = y
					}
					n++
				}
			}
			END { if (found >= 20) print int((left + right) / 2), int((top + bottom) / 2) }'
}
# Clicks the dock icon of $1 and waits for the screen to change by at least
# $2 bytes and show a window in front; $3 says what was being done.
from_dock() {
	[ -z "$minimise_fail" ] || return 0
	dump min-before
	pointer "$(dock_x_of "$1")" "$dock_y"
	click
	park
	tries=0
	while :; do
		sleep 1
		dump min-after
		changed=$(differs min-before min-after)
		if [ "$changed" -ge "$2" ] && [ -n "$(yellow_button min-after)" ]; then
			break
		fi
		tries=$((tries + 1))
		if [ "$tries" -ge 8 ]; then
			minimise_fail="$3: the click on the dock icon of $1 brought no window to the front ($changed bytes of the screen changed)"
			break
		fi
	done
}
# Minimises the window in front, which is that of $1.
minimise_front() {
	[ -z "$minimise_fail" ] || return 0
	dump min-before
	at=$(yellow_button min-before)
	if [ -z "$at" ]; then
		minimise_fail="no yellow button on the window of $1"
		return 0
	fi
	pointer "${at% *}" "${at#* }"
	click
	park
	dump min-after
	changed=$(differs min-before min-after)
	# A frozen screen does not change at all. On a wide screen the button
	# must also have moved or gone; on a small one every window fills the
	# screen, so the window underneath has its button in the same place.
	if [ "$changed" -lt 20000 ] || { [ "$screen_w" -ge 1024 ] && [ "$(yellow_button min-after)" = "$at" ]; }; then
		minimise_fail="minimising $1 with its yellow button did nothing ($changed bytes of the screen changed): the desktop may be frozen"
	fi
	echo "minimise $1: $changed bytes changed" >&2
}
if [ "$want" = sengi-compositor ] && [ -z "$greeter_fail" ]; then
	park
	dump min-empty
	from_dock sengi-settings "$window_bytes" "opening Settings"
	from_dock sengi-files 50000 "opening Files"
	from_dock sengi-store 50000 "opening the Store"
	shot minimise-three-open
	minimise_front sengi-store
	minimise_front sengi-files
	from_dock sengi-store 50000 "restoring the Store"
	from_dock sengi-settings 20000 "bringing Settings to the front"
	minimise_front sengi-settings
	shot minimise-after
	# Alive after all of that: a minimised window comes back.
	from_dock sengi-files 50000 "restoring Files after minimising"
	shot minimise-restored
	# Close all three; Settings is still minimised.
	if [ -z "$minimise_fail" ]; then
		key meta_l-q 2
		key meta_l-q 2
		from_dock sengi-settings "$window_bytes" "restoring Settings to close it"
	fi
	key meta_l-q 2
	if [ -n "$minimise_fail" ]; then
		key meta_l-q 2
		key meta_l-q 2
	fi
	park
	dump min-after
	left=$(differs min-empty min-after)
	echo "minimise: $left bytes differ from the empty desktop afterwards" >&2
	# The bar's clock may have moved on a minute; nothing else has.
	if [ -z "$minimise_fail" ] && [ "$left" -ge 20000 ]; then
		minimise_fail="the desktop did not come back after the minimised windows were closed ($left bytes differ)"
	fi
	rm -f "$out"/min-*.ppm
fi
# A maximised window has the screen to itself: the bar and the dock go, come
# back while the pointer rests at their edge or while something opened from
# them is up, and are there again once it is unmaximised. The title bar moves
# below the bar while it is back. labwc keeps both in place, so this is for
# our compositor only. The bar is the top 28 rows of the screen.
maximise_fail=
whole() {
	maximise_fail=${maximise_fail:-$1}
}
# Where the yellow button of the window in front is down the screen, in dump $1.
button_row() {
	yellow_button "$1" | awk '{ print $2 }'
}
if [ "$want" = sengi-compositor ] && [ -z "$greeter_fail$minimise_fail" ]; then
	dock_settings=$(dock_x_of sengi-settings)
	dock_rows=$((screen_h - 60))
	park
	dump max-empty
	pointer "$dock_settings" "$dock_y"
	click
	park
	sleep 3
	dump max-window
	shot maximise-before
	[ "$(differs max-empty max-window)" -ge "$window_bytes" ] ||
		whole "Settings did not open to be maximised"
	key meta_l-a 3
	dump max-full
	shot maximised
	[ "$(strip_differs max-window max-full 0 28)" -ge 1000 ] ||
		whole "the bar still shows over a maximised window"
	[ "$(strip_differs max-window max-full "$dock_rows" 50)" -ge 3000 ] ||
		whole "the dock still shows over a maximised window"
	high=$(button_row max-full)
	{ [ -n "$high" ] && [ "$high" -lt 28 ]; } ||
		whole "the maximised window's title bar is not at the top (button at row ${high:-none})"
	# The top edge brings the bar, and the title bar moves below it.
	pointer $((screen_w / 2)) 0
	sleep 1
	dump max-top
	shot maximised-bar
	changed=$(strip_differs max-window max-top 0 28)
	[ "$changed" -lt 20000 ] ||
		whole "resting at the top edge did not bring the bar back ($changed bytes of it differ)"
	low=$(button_row max-top)
	{ [ -n "$low" ] && [ $((low - ${high:-0})) -ge 24 ] && [ $((low - ${high:-0})) -le 32 ]; } ||
		whole "the title bar did not move below the bar (button at row ${low:-none}, before ${high:-none})"
	# Quick settings from that bar: it stays while they are up, wherever
	# the pointer is.
	pointer $((screen_w - 293)) 14
	click
	park
	sleep 2
	dump max-quick
	shot maximised-quick
	[ "$(differs max-top max-quick)" -ge 20000 ] ||
		whole "quick settings did not open from the bar over a maximised window"
	changed=$(strip_differs max-window max-quick 0 28)
	[ "$changed" -lt 20000 ] ||
		whole "the bar left while quick settings were open ($changed bytes of it differ)"
	key esc 2
	dump max-closed
	[ "$(strip_differs max-full max-closed 0 28)" -lt 20000 ] ||
		whole "the bar did not leave once quick settings were closed"
	# The bottom edge brings the dock, and its menu keeps it.
	pointer $((screen_w / 8)) $((screen_h - 1))
	sleep 1
	dump max-bottom
	shot maximised-dock
	[ "$(strip_differs max-full max-bottom "$dock_rows" 50)" -ge 3000 ] ||
		whole "resting at the bottom edge did not bring the dock back"
	open_dock_menu "$dock_settings"
	sleep 2
	dump max-menu
	shot maximised-dock-menu
	[ "$(menu_differs max-full max-menu)" -ge "$menu_bytes" ] ||
		whole "no dock menu over a maximised window"
	key esc 1
	park
	sleep 1
	dump max-left
	[ "$(strip_differs max-full max-left "$dock_rows" 50)" -lt 3000 ] ||
		whole "the dock did not leave when the pointer left it"
	key meta_l-a 3
	dump max-restored
	shot maximise-restored
	changed=$(differs max-window max-restored)
	[ "$changed" -lt 20000 ] ||
		whole "the desktop did not come back after unmaximising ($changed bytes differ)"
	key meta_l-q 2
	rm -f "$out"/max-*.ppm
fi
rm -f "$out"/*.ppm "$out"/*.band
stop

# What the sound card was given to play, complete now that QEMU has ended.
# The guest played a tone of 400 Hz through PipeWire and one of 1000 Hz
# through ALSA's default device. For each tenth of a second of the left
# channel the pitch is counted from the rises through zero; silence, which is
# all zeros, has none. This prints how many tenths had the pitch $1.
heard() {
	[ -s "$out/audio.wav" ] || { echo 0; return; }
	tail -c +45 "$out/audio.wav" | od -An -v -td2 -w4 | awk -v want="$1" '
		{
			if ($1 > 100) { if (low) rises++; low = 0 } else if ($1 < -100) low = 1
			if (++n == 4800) {
				if (rises * 10 >= want * 0.9 && rises * 10 <= want * 1.1) tenths++
				n = rises = 0
			}
		}
		END { print tenths + 0 }'
}
sound_fail=
for pitch in 400 1000; do
	tenths=$(heard "$pitch")
	echo "sound: $tenths tenths of a second heard at $pitch Hz" >&2
	[ "$tenths" -ge 5 ] ||
		sound_fail=${sound_fail:-"the sound card was not given the $pitch Hz tone ($tenths tenths of a second of it heard)"}
done

guest_s=$(sed -n 's/.*SENGI-BOOT desktop_s=\([0-9.]*\).*/\1/p' "$out/serial.log" | head -1)
ram_mb=$(sed -n 's/.*SENGI-IDLE ram_mb=\([0-9]*\).*/\1/p' "$out/serial.log" | head -1)
# The same with the live question still on screen, and what its process costs.
dialog_ram_mb=$(sed -n 's/.*SENGI-DIALOG up ram_mb=\([0-9]*\).*/\1/p' "$out/serial.log" | head -1)
dialog_kb=$(sed -n 's/.*SENGI-DIALOG up .*pss_kb=\([0-9]*\).*/\1/p' "$out/serial.log" | head -1)
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
	echo "live_dialog_ram_mb=$dialog_ram_mb"
	echo "live_dialog_pss_kb=$dialog_kb"
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
if [ -n "$size_fail" ]; then
	echo "FAIL: $size_fail" >&2
	fail=1
fi
if [ -n "$dialog_fail" ]; then
	echo "FAIL: $dialog_fail" >&2
	fail=1
fi
if [ -n "$settings_fail" ]; then
	echo "FAIL: $settings_fail" >&2
	fail=1
fi
if [ -n "$desktop_fail" ]; then
	echo "FAIL: $desktop_fail" >&2
	fail=1
fi
if [ -n "$dock_fail" ]; then
	echo "FAIL: $dock_fail" >&2
	fail=1
fi
if [ -n "$menu_fail" ]; then
	echo "FAIL: $menu_fail" >&2
	fail=1
fi
if [ -n "$maximise_fail" ]; then
	echo "FAIL: $maximise_fail" >&2
	fail=1
fi
if [ -n "$store_fail" ]; then
	echo "FAIL: $store_fail" >&2
	fail=1
fi
if [ -n "$minimise_fail" ]; then
	echo "FAIL: $minimise_fail" >&2
	fail=1
fi
if [ -n "$lock_fail" ]; then
	echo "FAIL: $lock_fail" >&2
	fail=1
fi
if [ -n "$forward_fail" ]; then
	echo "FAIL: $forward_fail" >&2
	fail=1
fi
if [ -n "$apps_fail" ]; then
	echo "FAIL: $apps_fail" >&2
	fail=1
fi
if [ -n "$drives_fail" ]; then
	echo "FAIL: $drives_fail" >&2
	fail=1
fi
if [ -n "$sound_fail" ]; then
	echo "FAIL: $sound_fail" >&2
	fail=1
fi
if [ -n "$greeter_fail" ]; then
	echo "FAIL: $greeter_fail" >&2
	fail=1
fi
if [ "${ram_mb:-9999}" -ge "$budget_ram_mb" ]; then
	echo "FAIL: idle RAM ${ram_mb} MB is over the ${budget_ram_mb} MB budget" >&2
	fail=1
fi
if [ "${dialog_ram_mb:-9999}" -ge "$budget_ram_mb" ]; then
	echo "FAIL: idle RAM with the live question open, ${dialog_ram_mb:-not measured} MB, is over the ${budget_ram_mb} MB budget" >&2
	fail=1
fi
if [ "$(echo "${boot_s:-9999} $budget_boot_s" | awk '{print ($1 >= $2)}')" = 1 ]; then
	echo "FAIL: boot took ${boot_s} s, budget is ${budget_boot_s} s" >&2
	fail=1
fi
exit $fail
