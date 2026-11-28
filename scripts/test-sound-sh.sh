#!/bin/sh
# Tests image/rootfs/usr/lib/sengi-os/sound.sh: that an audio device that
# came up without its profiles gets WirePlumber restarted, and that nothing
# else does. A fake pw-cli, systemctl and systemd-cat on a temporary PATH
# stand in; nothing outside the temporary directory is touched. Meant to
# run under dash as well as bash:
#   sh scripts/test-sound-sh.sh; dash scripts/test-sound-sh.sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
FAKE_DIR=$tmp
export FAKE_DIR

# A pw-cli that lists the devices of the file "devices", one "number class"
# to a line, and has an answer for every parameter of a device except those
# named by a file "empty.NUMBER.PARAM" or "fails.NUMBER.PARAM". It prints
# what the real one does. "gone" lists a device for one look only, as one
# being unplugged is.
cat >"$tmp/bin/pw-cli" <<'FAKE'
#!/bin/sh
case $1 in
ls)
	[ ! -e "$FAKE_DIR/no-daemon" ] || exit 1
	while read -r id class; do
		printf '\tid %s, type PipeWire:Interface:Device/3\n' "$id"
		printf ' \t\tobject.serial = "%s"\n \t\tdevice.api = "alsa"\n' "$id"
		printf ' \t\tmedia.class = "%s"\n' "$class"
	done <"$FAKE_DIR/devices"
	if [ -e "$FAKE_DIR/gone" ]; then
		printf '\tid %s, type PipeWire:Interface:Device/3\n \t\tmedia.class = "Audio/Device"\n' "$(cat "$FAKE_DIR/gone")"
		mv "$FAKE_DIR/gone" "$FAKE_DIR/left"
	fi
	;;
enum-params)
	echo "$2 $3" >>"$FAKE_DIR/asked"
	[ ! -e "$FAKE_DIR/fails.$2.$3" ] || exit 1
	[ ! -e "$FAKE_DIR/empty.$2.$3" ] || exit 0
	if [ -e "$FAKE_DIR/left" ] && [ "$(cat "$FAKE_DIR/left")" = "$2" ]; then
		echo "Error: \"enum-params: unknown global '$2'\"" >&2
		exit 0
	fi
	printf '  Object: size 160, type Spa:Pod:Object:Param:Profile (262151), id %s\n' "$3"
	;;
*) exit 2 ;;
esac
FAKE
# Restarting WirePlumber mends every device, unless "stubborn" is there.
cat >"$tmp/bin/systemctl" <<'FAKE'
#!/bin/sh
echo "$*" >>"$FAKE_DIR/systemctl"
[ -e "$FAKE_DIR/stubborn" ] || rm -f "$FAKE_DIR"/empty.*
FAKE
cat >"$tmp/bin/systemd-cat" <<'FAKE'
#!/bin/sh
cat >>"$FAKE_DIR/journal"
FAKE
chmod +x "$tmp/bin/"*
PATH=$tmp/bin:$PATH

# shellcheck source=image/rootfs/usr/lib/sengi-os/sound.sh
. "$root/image/rootfs/usr/lib/sengi-os/sound.sh"

failures=0
check() {
	if [ "$2" = "$3" ]; then
		echo "ok   $1"
	else
		echo "FAIL $1: wanted '$3', got '$2'" >&2
		failures=$((failures + 1))
	fi
}

# A fresh machine: one sound card and one camera.
machine() {
	rm -f "$tmp"/empty.* "$tmp"/fails.* "$tmp"/systemctl "$tmp"/asked "$tmp"/journal \
		"$tmp"/stubborn "$tmp"/gone "$tmp"/left "$tmp"/no-daemon
	printf '42 Audio/Device\n60 Video/Device\n' >"$tmp/devices"
}
watch() { SENGI_SOUND_WAITS="${1:-0 0}" sengi_sound_watch; }
restarts() { grep -c . "$tmp/systemctl" 2>/dev/null || true; }

machine
check "a whole device is not broken" "$(sengi_sound_broken)" ""
check "a camera is not asked for profiles" "$(grep -c '^60 ' "$tmp/asked" || true)" 0
watch
check "and nothing is restarted for it" "$(restarts)" ""

for param in EnumProfile Profile; do
	machine
	: >"$tmp/empty.42.$param"
	check "a device with no $param is broken" "$(sengi_sound_broken)" 42
	watch
	check "WirePlumber is restarted, once" "$(cat "$tmp/systemctl")" \
		"--user try-restart wireplumber.service"
	check "and the journal says why" "$(cat "$tmp/journal")" \
		"sound device 42 came up without its profiles, restarting WirePlumber"
done

machine
printf '42 Audio/Device\n43 Audio/Device\n' >"$tmp/devices"
: >"$tmp/empty.43.EnumProfile"
: >"$tmp/empty.43.Profile"
check "a second card is found, and named once" "$(sengi_sound_broken)" 43

machine
: >"$tmp/empty.42.EnumProfile"
: >"$tmp/stubborn"
watch "0 0 0"
check "one that stays broken is not restarted for ever" "$(restarts)" 3

machine
echo 77 >"$tmp/gone"
watch 0
check "a device that is going away is left alone" "$(restarts)" ""

machine
: >"$tmp/fails.42.EnumProfile"
: >"$tmp/fails.42.Profile"
watch
check "a question that fails says nothing" "$(restarts)" ""

machine
: >"$tmp/no-daemon"
watch
check "nothing is done without PipeWire" "$(restarts)" ""

[ "$failures" -eq 0 ]
