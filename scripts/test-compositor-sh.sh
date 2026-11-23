#!/bin/sh
# Tests image/rootfs/usr/lib/sengi-os/compositor.sh: which compositor is
# wanted, and what own_compositor does when ours fails, hangs or ends. It
# runs the real functions against a fake sengi-compositor and a fake
# systemd-cat on a temporary PATH. Nothing outside the temporary directory
# is touched. SENGI_COMPOSITOR_LIB names another copy of the script to test,
# and the whole file is meant to be run under dash as well as bash:
#   sh scripts/test-compositor-sh.sh; dash scripts/test-compositor-sh.sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
lib=${SENGI_COMPOSITOR_LIB:-$root/image/rootfs/usr/lib/sengi-os/compositor.sh}
real_sleep=$(command -v sleep)
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

# The compositor: FAKE_READY=1 writes the ready file first (when there is a
# runtime directory), then it lives for FAKE_LIFE seconds and ends with
# FAKE_STATUS. A hang is a long life. It leaves its pid and its sleep's in
# files, ends on SIGTERM unless FAKE_IGNORE_TERM=1, and takes its sleep along.
cat >"$tmp/bin/sengi-compositor" <<'FAKE'
#!/bin/sh
ready=${XDG_RUNTIME_DIR:+$XDG_RUNTIME_DIR/sengi-compositor.ready}
echo $$ >"$FAKE_DIR/compositor.pid"
if [ "${FAKE_READY:-0}" = 1 ] && [ -n "$ready" ]; then
	: >"$ready"
fi
"$FAKE_SLEEP" "${FAKE_LIFE:-0}" &
child=$!
echo "$child" >"$FAKE_DIR/sleep.pid"
if [ "${FAKE_IGNORE_TERM:-0}" = 1 ]; then
	trap '' TERM
else
	trap 'kill "$child" 2>/dev/null; [ -z "$ready" ] || rm -f "$ready"; exit 143' TERM
fi
wait "$child" || true
[ -z "$ready" ] || rm -f "$ready"
exit "${FAKE_STATUS:-0}"
FAKE
# A sleep that takes whole seconds only, for a system whose sleep does.
cat >"$tmp/whole-sleep" <<'FAKE'
#!/bin/sh
case $1 in
*[!0-9]*) echo "sleep: invalid time interval" >&2; exit 1 ;;
esac
exec "$FAKE_SLEEP" "$@"
FAKE
chmod +x "$tmp/bin/systemd-cat" "$tmp/bin/sengi-compositor" "$tmp/whole-sleep"

PATH=$tmp/bin:$PATH
XDG_RUNTIME_DIR=$tmp/run
SENGI_FAILED_FILE=$tmp/failed
SENGI_COMPOSITOR_FILES="$tmp/run-choice $tmp/etc-choice"
SENGI_READY_POLL=0.05
SENGI_READY_TICKS=20
FAKE_DIR=$tmp
FAKE_SLEEP=$real_sleep
export FAKE_DIR FAKE_SLEEP
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
	# shellcheck source=/dev/null
	(. "$lib" && wanted_compositor)
}

# Runs own_compositor in a subshell and prints what became of it: "exit N"
# if it ended the script with status N, "FELL_BACK" if it returned, which is
# where the caller would start labwc.
own() {
	status=0
	out=$(
		# shellcheck source=/dev/null
		. "$lib"
		own_compositor --test
		echo FELL_BACK
	) 2>"$tmp/stderr" || status=$?
	if [ -n "$out" ]; then
		echo "$out"
	else
		echo "exit $status"
	fi
}

reset
check "ours by default" "$(wanted)" own

printf 'quiet sengi.compositor=own splash\n' >"$tmp/cmdline"
check "ours by the kernel word, still accepted" "$(wanted)" own

printf 'quiet sengi.compositor=labwc\n' >"$tmp/cmdline"
check "labwc by the kernel word" "$(wanted)" labwc

printf 'sengi.compositor=own sengi.compositor=labwc\n' >"$tmp/cmdline"
check "the labwc word wins over the own word" "$(wanted)" labwc

printf 'sengi.compositor=labwcish\n' >"$tmp/cmdline"
check "a longer word is not the word" "$(wanted)" own

reset
echo own >"$tmp/etc-choice"
check "ours by the file in /etc, still accepted" "$(wanted)" own
echo labwc >"$tmp/etc-choice"
check "labwc by the file in /etc" "$(wanted)" labwc
rm "$tmp/etc-choice"
echo labwc >"$tmp/run-choice"
check "labwc by the run file" "$(wanted)" labwc
echo own >"$tmp/etc-choice"
check "labwc in the run file wins over own in /etc" "$(wanted)" labwc

reset
printf 'sengi.compositor=own\n' >"$tmp/cmdline"
echo labwc >"$tmp/etc-choice"
check "labwc in a file wins over the own word" "$(wanted)" labwc

reset
: >"$tmp/failed"
check "a failed start earlier in the boot means labwc" "$(wanted)" labwc
printf 'sengi.compositor=own\n' >"$tmp/cmdline"
check "and also against the own word" "$(wanted)" labwc

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


noted() {
	if [ -e "$tmp/failed" ]; then echo noted; else echo none; fi
}

gone() {
	if kill -0 "$(cat "$1")" 2>/dev/null; then echo alive; else echo gone; fi
}

# The compositor is still ours to end when the hang was given up on: it must
# be gone, and with it the sleep it started.
reset
FAKE_READY=0 FAKE_LIFE=30 FAKE_STATUS=0 SENGI_READY_TICKS=4 SENGI_FAST_SECONDS=10
export FAKE_READY FAKE_LIFE FAKE_STATUS SENGI_READY_TICKS SENGI_FAST_SECONDS
check "the hang test again" "$(own)" FELL_BACK
check "a hung compositor is really gone" "$(gone "$tmp/compositor.pid")" gone
check "and leaves no sleep behind" "$(gone "$tmp/sleep.pid")" gone
FAKE_IGNORE_TERM=1
export FAKE_IGNORE_TERM
check "one that ignores TERM is killed" "$(own)" FELL_BACK
check "and is gone" "$(gone "$tmp/compositor.pid")" gone
unset FAKE_IGNORE_TERM
"$real_sleep" 1

# SIGTERM, as greetd ends a session with it. Prints what became of the
# script, after the signal was sent once the ready file exists (or, for
# "early", straight away).
terminated() {
	(
		# shellcheck source=/dev/null
		. "$lib"
		own_compositor --test
		echo FELL_BACK
	) >"$tmp/out" 2>"$tmp/stderr" &
	pid=$!
	if [ "$1" = late ]; then
		n=0
		while [ ! -e "$tmp/run/sengi-compositor.ready" ] && [ "$n" -lt 100 ]; do
			"$real_sleep" 0.05
			n=$((n + 1))
		done
	else
		"$real_sleep" 0.5
	fi
	kill -TERM "$pid"
	status=0
	wait "$pid" || status=$?
	echo "$(cat "$tmp/out") exit $status"
}

reset
FAKE_READY=0 FAKE_LIFE=30 FAKE_STATUS=0 SENGI_READY_TICKS=150
export FAKE_READY FAKE_LIFE FAKE_STATUS SENGI_READY_TICKS
check "SIGTERM before ready ends with success, not 127, and no fallback" "$(terminated early)" " exit 0"
check "SIGTERM before ready is not a failure" "$(noted)" none
check "SIGTERM before ready takes the compositor along" "$(gone "$tmp/compositor.pid")" gone

reset
FAKE_READY=1
export FAKE_READY
check "SIGTERM after ready ends with success and no fallback" "$(terminated late)" " exit 0"
check "SIGTERM after ready is not a failure" "$(noted)" none
check "SIGTERM after ready leaves no ready file" "$([ -e "$tmp/run/sengi-compositor.ready" ] && echo left || echo gone)" gone

reset
FAKE_READY=0 FAKE_LIFE=0 FAKE_STATUS=0
export FAKE_READY FAKE_LIFE FAKE_STATUS
check "a fast exit with status 0 does not fall back" "$(own)" "exit 0"
check "and is not a failure" "$(noted)" none

reset
saved=$XDG_RUNTIME_DIR
unset XDG_RUNTIME_DIR
FAKE_READY=1 FAKE_LIFE=1 FAKE_STATUS=0
export FAKE_READY FAKE_LIFE FAKE_STATUS
check "without XDG_RUNTIME_DIR nothing is waited for and a clean exit is clean" "$(own)" "exit 0"
FAKE_STATUS=1 FAKE_LIFE=0
check "without XDG_RUNTIME_DIR a fast failure falls back" "$(own)" FELL_BACK
XDG_RUNTIME_DIR=$saved
export XDG_RUNTIME_DIR

reset
SENGI_FAILED_FILE=$tmp/nowhere/failed
export SENGI_FAILED_FILE
FAKE_READY=0 FAKE_LIFE=0 FAKE_STATUS=1
export FAKE_READY FAKE_LIFE FAKE_STATUS
check "an unwritable failure note does not stop the fallback" "$(own)" FELL_BACK
check "and says nothing on stderr" "$(cat "$tmp/stderr")" ""
check "and then the next start still tries ours" "$(printf 'sengi.compositor=own\n' >"$tmp/cmdline"; wanted)" own
SENGI_FAILED_FILE=$tmp/failed
export SENGI_FAILED_FILE

# Only whole seconds in sleep: the poll must not run out at once.
reset
printf 'sengi.compositor=own\n' >"$tmp/cmdline"
mkdir "$tmp/whole"
cp "$tmp/whole-sleep" "$tmp/whole/sleep"
FAKE_READY=1 FAKE_LIFE=2 FAKE_STATUS=0 SENGI_READY_TICKS=20 SENGI_READY_POLL=0.05
export FAKE_READY FAKE_LIFE FAKE_STATUS SENGI_READY_TICKS SENGI_READY_POLL
check "with a whole-second sleep a healthy compositor is left alone" "$(PATH=$tmp/whole:$PATH own)" "exit 0"
check "and nothing is noted" "$(noted)" none

if [ "$failures" -ne 0 ]; then
	echo "$failures check(s) failed" >&2
	exit 1
fi
echo "all checks passed"
