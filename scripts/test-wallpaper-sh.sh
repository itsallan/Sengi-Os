#!/bin/sh
# Tests image/rootfs/usr/lib/sengi-os/wallpaper.sh.in: that the session
# starts swaybg with the picture sengi-settings names, with the plain colour
# when it names none, replaces the running one without a gap, falls back
# when swaybg dies at once on a picture, and leaves alone a process number
# that is not a swaybg. A fake swaybg and a fake sengi-settings on a
# temporary PATH stand in; nothing outside the temporary directory is
# touched. Meant to run under dash as well as bash:
#   sh scripts/test-wallpaper-sh.sh; dash scripts/test-wallpaper-sh.sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'kill $(cat "$tmp"/started 2>/dev/null) 2>/dev/null || true; rm -rf "$tmp"' EXIT
mkdir "$tmp/bin" "$tmp/run"
: >"$tmp/started"

sh "$root/scripts/render-tokens.sh" "$root/image/rootfs/usr/lib/sengi-os/wallpaper.sh.in" >"$tmp/wallpaper.sh"
desktop=$(sh "$root/scripts/render-tokens.sh" "$root/image/rootfs/usr/lib/sengi-os/wallpaper.sh.in" | sed -n "s/.*-m fill -c '\\(#[0-9a-f]*\\)'.*/\\1/p")

# A swaybg that writes its arguments to a file named for its own process
# number, then lives until killed, or ends at once when FAKE_DIE is set and
# it was given a picture.
cat >"$tmp/bin/swaybg" <<'FAKE'
#!/bin/sh
echo "$*" >"$FAKE_DIR/args.$$"
echo $$ >>"$FAKE_DIR/started"
case " $* " in
*" -i "*) [ -z "${FAKE_DIE:-}" ] || exit 1 ;;
esac
"$FAKE_SLEEP" 600 &
child=$!
trap 'kill "$child" 2>/dev/null; exit 0' TERM
wait "$child"
FAKE
# swaybg is a shell script here: its name in /proc is the script's, which
# is what the real script looks at.
cat >"$tmp/bin/sengi-settings" <<'FAKE'
#!/bin/sh
[ "$1" = --wallpaper ] || exit 2
[ -z "${FAKE_PICTURE:-}" ] || echo "$FAKE_PICTURE"
FAKE
chmod +x "$tmp/bin/swaybg" "$tmp/bin/sengi-settings"

FAKE_DIR=$tmp
FAKE_SLEEP=$(command -v sleep)
PATH=$tmp/bin:$PATH
XDG_RUNTIME_DIR=$tmp/run
export FAKE_DIR FAKE_SLEEP PATH XDG_RUNTIME_DIR

failures=0
check() {
	if [ "$2" = "$3" ]; then
		echo "ok   $1"
	else
		echo "FAIL $1: wanted '$3', got '$2'" >&2
		failures=$((failures + 1))
	fi
}

alive() { kill -0 "$1" 2>/dev/null && echo yes || echo no; }
pid_of_file() { cat "$tmp/run/sengi-wallpaper.pid"; }
args_of() { cat "$tmp/args.$1"; }
run_apply() { sh "$tmp/wallpaper.sh" apply; }

# 1. No picture: the plain colour.
unset FAKE_PICTURE FAKE_DIE || true
run_apply
first=$(pid_of_file)
sleep 1
check "plain colour when there is no picture" "$(args_of "$first")" "-c $desktop"
check "that swaybg is running" "$(alive "$first")" yes

# 2. A picture: filled, over the colour. The old one goes after the new is up.
FAKE_PICTURE=$tmp/photo.png
export FAKE_PICTURE
run_apply
second=$(pid_of_file)
check "a picture is filled over the colour" "$(args_of "$second")" "-m fill -c $desktop -i $tmp/photo.png"
check "the old swaybg is gone" "$(alive "$first")" no
check "the new one runs" "$(alive "$second")" yes
check "a new process" "$([ "$first" != "$second" ] && echo yes)" yes

# 3. A path with a space is one argument.
FAKE_PICTURE="$tmp/my photo.png"
export FAKE_PICTURE
run_apply
third=$(pid_of_file)
check "a path with a space stays whole" "$(args_of "$third")" "-m fill -c $desktop -i $tmp/my photo.png"
check "the one before is gone" "$(alive "$second")" no

# 4. Back to the plain colour.
unset FAKE_PICTURE
run_apply
fourth=$(pid_of_file)
check "plain colour again" "$(args_of "$fourth")" "-c $desktop"
check "the picture's swaybg is gone" "$(alive "$third")" no

# 5. swaybg that cannot show the picture: the colour stays.
FAKE_PICTURE=$tmp/broken.png FAKE_DIE=1
export FAKE_PICTURE FAKE_DIE
run_apply
fifth=$(pid_of_file)
check "falls back to the colour when the picture kills swaybg" "$(args_of "$fifth")" "-c $desktop"
check "and that one is running" "$(alive "$fifth")" yes
check "and the one before is gone" "$(alive "$fourth")" no
unset FAKE_PICTURE FAKE_DIE

# 6. A pid file that names something that is not a swaybg: not killed.
sleep 600 &
bystander=$!
echo "$bystander" >"$tmp/run/sengi-wallpaper.pid"
run_apply
check "another program's process number is left alone" "$(alive "$bystander")" yes
kill "$bystander" 2>/dev/null || true

# 7. A pid file with rubbish in it, and none at all.
echo "12; rm -rf" >"$tmp/run/sengi-wallpaper.pid"
run_apply
check "rubbish in the pid file is no harm" "$(alive "$(pid_of_file)")" yes
rm -f "$tmp/run/sengi-wallpaper.pid"
XDG_RUNTIME_DIR= run_apply
check "no runtime directory, no pid file, still starts" "$(ls "$tmp/run")" ""

# 8. Sourced, as session-start does, it defines the function and runs nothing.
before=$(wc -l <"$tmp/started")
(
	# shellcheck source=/dev/null
	. "$tmp/wallpaper.sh"
	command -v sengi_wallpaper_apply >/dev/null
)
sleep 1
check "sourcing starts nothing" "$(wc -l <"$tmp/started")" "$before"

if [ "$failures" -gt 0 ]; then
	echo "$failures failed" >&2
	exit 1
fi
echo "all passed"
