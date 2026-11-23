#!/bin/sh
# Tests image/rootfs/usr/lib/sengi-os/compositor.sh: which compositor is
# wanted, and what own_compositor does when ours fails, hangs or ends. It
# runs the real functions against a fake sengi-compositor and a fake
# systemd-cat on a temporary PATH. Nothing outside the temporary directory
# is touched.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
lib=$root/image/rootfs/usr/lib/sengi-os/compositor.sh
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin" "$tmp/run"

# systemd-cat -t TAG [-p PRIORITY] [COMMAND...]: runs the command, or else
# takes what is piped in and drops it.
cat >"$tmp/bin/systemd-cat" <<'FAKE'
#!/bin/sh
while [ "${1:-}" = -t ] || [ "${1:-}" = -p ]; do
	shift 2
done
if [ $# -gt 0 ]; then
	exec "$@"
fi
cat >/dev/null
FAKE

# The compositor: FAKE_READY=1 writes the ready file first, then it lives
# for FAKE_LIFE seconds and ends with FAKE_STATUS. A hang is a long life.
cat >"$tmp/bin/sengi-compositor" <<'FAKE'
#!/bin/sh
if [ "${FAKE_READY:-0}" = 1 ]; then
	: >"$XDG_RUNTIME_DIR/sengi-compositor.ready"
fi
sleep "${FAKE_LIFE:-0}"
rm -f "$XDG_RUNTIME_DIR/sengi-compositor.ready"
exit "${FAKE_STATUS:-0}"
FAKE
chmod +x "$tmp/bin/systemd-cat" "$tmp/bin/sengi-compositor"

PATH=$tmp/bin:$PATH
XDG_RUNTIME_DIR=$tmp/run
SENGI_FAILED_FILE=$tmp/failed
SENGI_COMPOSITOR_FILES="$tmp/run-choice $tmp/etc-choice"
SENGI_READY_POLL=0.05
SENGI_READY_TICKS=20
export PATH XDG_RUNTIME_DIR SENGI_FAILED_FILE SENGI_COMPOSITOR_FILES SENGI_READY_POLL SENGI_READY_TICKS

failures=0
check() {
	if [ "$2" = "$3" ]; then
		echo "ok   $1"
	else
		echo "FAIL $1: wanted '$3', got '$2'" >&2
		failures=$((failures + 1))
	fi
}

reset() {
	rm -f "$tmp/failed" "$tmp/run-choice" "$tmp/etc-choice" "$tmp/cmdline"
	printf 'quiet splash\n' >"$tmp/cmdline"
	SENGI_CMDLINE=$tmp/cmdline
	export SENGI_CMDLINE
}

wanted() {
	(. "$lib" && wanted_compositor)
}

# Runs own_compositor in a subshell and prints what became of it: "exit N"
# if it ended the script with status N, "FELL_BACK" if it returned, which is
# where the caller would start labwc.
own() {
	status=0
	out=$(
		. "$lib"
		own_compositor --test
		echo FELL_BACK
	) || status=$?
	if [ -n "$out" ]; then
		echo "$out"
	else
		echo "exit $status"
	fi
}

reset
check "labwc by default" "$(wanted)" labwc

printf 'quiet sengi.compositor=own splash\n' >"$tmp/cmdline"
check "own by the kernel word" "$(wanted)" own

printf 'quiet sengi.compositor=labwc\n' >"$tmp/cmdline"
check "labwc by the kernel word" "$(wanted)" labwc

printf 'sengi.compositor=own sengi.compositor=labwc\n' >"$tmp/cmdline"
check "the labwc word wins over the own word" "$(wanted)" labwc

printf 'sengi.compositor=ownish\n' >"$tmp/cmdline"
check "a longer word is not the word" "$(wanted)" labwc

reset
echo own >"$tmp/etc-choice"
check "own by the file in /etc" "$(wanted)" own
echo labwc >"$tmp/run-choice"
check "labwc in the run file wins over own in /etc" "$(wanted)" labwc

reset
printf 'sengi.compositor=own\n' >"$tmp/cmdline"
echo labwc >"$tmp/etc-choice"
check "labwc in a file wins over the own word" "$(wanted)" labwc

reset
printf 'sengi.compositor=own\n' >"$tmp/cmdline"
: >"$tmp/failed"
check "a failed start earlier in the boot means labwc" "$(wanted)" labwc

reset
export FAKE_READY=1 FAKE_LIFE=0.3 FAKE_STATUS=0
check "a clean exit does not fall back" "$(own)" "exit 0"
check "a clean exit leaves no failure note" "$([ -e "$tmp/failed" ] && echo noted || echo none)" none
check "the ready file goes with the compositor" "$([ -e "$tmp/run/sengi-compositor.ready" ] && echo left || echo gone)" gone

reset
FAKE_READY=0 FAKE_LIFE=30 FAKE_STATUS=0 SENGI_READY_TICKS=10 SENGI_FAST_SECONDS=10
export FAKE_READY FAKE_LIFE FAKE_STATUS SENGI_READY_TICKS SENGI_FAST_SECONDS
check "a compositor that is never ready is ended and labwc takes over" "$(own)" FELL_BACK
check "a hang is noted" "$([ -e "$tmp/failed" ] && echo noted || echo none)" noted

reset
FAKE_READY=0 FAKE_LIFE=0 FAKE_STATUS=1 SENGI_READY_TICKS=20
export FAKE_READY FAKE_LIFE FAKE_STATUS SENGI_READY_TICKS
check "a failure before it was ready falls back" "$(own)" FELL_BACK
check "a fast failure is noted" "$([ -e "$tmp/failed" ] && echo noted || echo none)" noted

reset
FAKE_READY=1 FAKE_LIFE=0.2 FAKE_STATUS=1
export FAKE_READY FAKE_LIFE FAKE_STATUS
check "a failure soon after it was ready falls back" "$(own)" FELL_BACK

reset
printf 'sengi.compositor=own\n' >"$tmp/cmdline"
FAKE_READY=1 FAKE_LIFE=2 FAKE_STATUS=1 SENGI_FAST_SECONDS=1
export FAKE_READY FAKE_LIFE FAKE_STATUS SENGI_FAST_SECONDS
check "a late failure ends the script with its status" "$(own)" "exit 1"
check "a late failure is noted" "$([ -e "$tmp/failed" ] && echo noted || echo none)" noted
check "the next start in the boot picks labwc" "$(wanted)" labwc

if [ "$failures" -ne 0 ]; then
	echo "$failures check(s) failed" >&2
	exit 1
fi
echo "all checks passed"
