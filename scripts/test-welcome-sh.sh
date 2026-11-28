#!/bin/sh
# Tests how image/rootfs/usr/lib/sengi-os/session-start.in starts
# sengi-welcome: that the first-run welcome waits its turn, and that what is
# opened from it, through the real sengi-run, runs at the usual priority.
# Fakes on a temporary PATH stand in for systemd and the programs; nothing
# outside the temporary directory is touched. Meant to run under dash as
# well as bash:
#   sh scripts/test-welcome-sh.sh; dash scripts/test-welcome-sh.sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin" "$tmp/home"

# The session, reading the kernel line and the scripts it sources from here.
sh "$root/scripts/render-tokens.sh" "$root/image/rootfs/usr/lib/sengi-os/session-start.in" |
	sed -e "s|/proc/cmdline|$tmp/cmdline|" \
		-e "s|/usr/lib/sengi-os/wallpaper.sh|$tmp/wallpaper.sh|" \
		-e "s|/usr/lib/sengi-os/sound.sh|$tmp/sound.sh|" >"$tmp/session-start"
echo 'sengi_wallpaper_apply() { :; }' >"$tmp/wallpaper.sh"
echo 'sengi_sound_watch() { :; }' >"$tmp/sound.sh"
cp "$root/image/rootfs/usr/bin/sengi-run" "$tmp/bin/sengi-run"

FAKE_DIR=$tmp
FAKE_SLEEP=$(command -v sleep)
export FAKE_DIR FAKE_SLEEP

# Notes the slice it was asked for under the program's name, then runs it.
cat >"$tmp/bin/systemd-run" <<'FAKE'
#!/bin/sh
slice=
while [ "$1" != -- ]; do
	case $1 in --slice=*) slice=${1#--slice=} ;; esac
	shift
done
shift
echo "$slice" >"$FAKE_DIR/$(basename "$1").slice"
exec "$@"
FAKE
# The welcome, which opens a program the way its buttons do.
cat >"$tmp/bin/sengi-welcome" <<'FAKE'
#!/bin/sh
nice >"$FAKE_DIR/sengi-welcome.nice"
sengi-run opened
: >"$FAKE_DIR/done"
FAKE
cat >"$tmp/bin/opened" <<'FAKE'
#!/bin/sh
nice >"$FAKE_DIR/opened.nice"
FAKE
for name in dbus-update-activation-environment sengi-focus sengi-settings \
	systemd-cat swayidle sleep; do
	printf '#!/bin/sh\n' >"$tmp/bin/$name"
done
chmod +x "$tmp/bin/"*

failures=0
check() {
	if [ "$2" = "$3" ]; then
		echo "ok   $1"
	else
		echo "FAIL $1: wanted '$3', got '$2'" >&2
		failures=$((failures + 1))
	fi
}
said() { cat "$tmp/$1" 2>/dev/null || echo nothing; }

# Runs the session with this kernel line and waits for what it starts.
session() {
	rm -f "$tmp/done" "$tmp"/*.nice "$tmp"/*.slice
	echo "$1" >"$tmp/cmdline"
	env -u LABWC_PID -u SENGI_COMPOSITOR_PID -u XDG_STATE_HOME \
		HOME="$tmp/home" PATH="$tmp/bin:$PATH" sh "$tmp/session-start"
	tries=0
	while [ ! -e "$tmp/done" ] && [ "$tries" -lt "$2" ]; do
		"$FAKE_SLEEP" 0.1
		tries=$((tries + 1))
	done
}

usual=$(nice)

session 'quiet' 50
check "the first login starts the welcome" "$([ -e "$tmp/done" ] && echo yes || echo no)" yes
check "the welcome waits its turn" "$(said sengi-welcome.slice)" background.slice
check "what the welcome opens is an app like any other" "$(said opened.slice)" app.slice
check "and runs at the usual priority" "$(said opened.nice)" "$usual"

session 'quiet boot=sengi' 50
check "the live question runs as an app" "$(said sengi-welcome.slice)" app.slice
check "and what it opens at the usual priority" "$(said opened.nice)" "$usual"

mkdir -p "$tmp/home/.local/state/sengi-os"
: >"$tmp/home/.local/state/sengi-os/welcome-done"
session 'quiet' 10
check "a welcome that was seen is not started again" "$(said sengi-welcome.slice)" nothing

[ "$failures" -eq 0 ] || {
	echo "$failures check(s) failed" >&2
	exit 1
}
