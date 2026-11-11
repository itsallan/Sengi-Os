#!/bin/sh
# Runs the real sengi-notify on a private session bus under a headless
# labwc and checks what the unit tests cannot: that it answers on the bus,
# that a client always gets the reply to Notify before the signal that the
# notification closed, that one client cannot replace another's
# notification, and the exit statuses a supervisor relies on.
#
#   cargo build -p sengi-notify
#   crates/sengi-notify/tests/smoke.sh target/debug/sengi-notify
#
# Needs labwc, dbus-daemon, dbus-run-session, dbus-monitor, gdbus and
# notify-send.
set -u

if [ "${1:-}" != --inside ]; then
    bin=${1:?usage: smoke.sh path/to/sengi-notify}
    case $bin in /*) ;; *) bin=$PWD/$bin ;; esac
    [ -x "$bin" ] || { echo "smoke.sh: $bin is not a program" >&2; exit 2; }
    for tool in labwc dbus-daemon dbus-run-session dbus-monitor gdbus notify-send; do
        command -v "$tool" >/dev/null || { echo "smoke.sh: $tool is not installed" >&2; exit 2; }
    done
    work=$(mktemp -d)
    export XDG_RUNTIME_DIR="$work" XDG_CONFIG_HOME="$work/config"
    export WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1
    unset WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS
    dbus-run-session -- "$0" --inside "$bin" "$work"
    status=$?
    rm -rf "$work"
    exit $status
fi

bin=$2
work=$3
failed=0
started=""

ok() { echo "ok    $1"; }
fail() { echo "FAIL  $1"; failed=1; }
# check <name> <command...>: passes if the command succeeds.
check() {
    name=$1
    shift
    if "$@"; then ok "$name"; else fail "$name"; fi
}
# status <name> <expected> <command...>: passes on that exit status.
status() {
    name=$1
    expected=$2
    shift 2
    "$@" >/dev/null 2>&1
    got=$?
    if [ "$got" -eq "$expected" ]; then ok "$name"; else fail "$name: exit status $got, expected $expected"; fi
}
finish() {
    # shellcheck disable=SC2086
    kill $started 2>/dev/null
    exit $failed
}
notify() {
    gdbus call --session --dest org.freedesktop.Notifications \
        --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.Notify "$@"
}
# The id out of a reply like "(uint32 7,)".
id_of() { echo "$1" | sed 's/[^0-9 ]//g' | awk '{ print $2 }'; }
# Asking the bus who owns the name, not the name itself: a call to a name
# nobody owns yet would have the bus start whichever notification service
# is installed on this machine.
owned() {
    gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.NameHasOwner org.freedesktop.Notifications 2>/dev/null |
        grep -q true
}
answers() {
    gdbus call --session --dest org.freedesktop.Notifications \
        --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.GetServerInformation >/dev/null 2>&1
}
wait_for() {
    tries=0
    until "$@" || [ $tries -ge 50 ]; do
        sleep 0.1
        tries=$((tries + 1))
    done
    "$@"
}

labwc >"$work/labwc.log" 2>&1 &
started="$started $!"
wait_for test -S "$work/wayland-0" || { fail "labwc did not start"; finish; }
export WAYLAND_DISPLAY=wayland-0

# Before anything is running.
status "a bad option is a usage error" 2 "$bin" --no-such-option
status "no session bus is a failure" 1 env DBUS_SESSION_BUS_ADDRESS=unix:path="$work/none" "$bin"

dbus-monitor --session "member='RequestName'" >"$work/request.log" 2>/dev/null &
watcher=$!
started="$started $watcher"
sleep 0.5
"$bin" 2>"$work/notify.err" &
service=$!
started="$started $service"
wait_for owned && answers || { fail "the service did not answer on the bus"; cat "$work/notify.err"; finish; }
ok "the service answers on the bus"

# The name is asked for with DBUS_NAME_FLAG_DO_NOT_QUEUE (4) and nothing
# else: not ALLOW_REPLACEMENT (1), which would let another program take
# the name, and not REPLACE_EXISTING (2), which would take it from one.
sleep 0.3
kill $watcher 2>/dev/null
flags=$(grep -A2 "member=RequestName" "$work/request.log" |
    grep -A1 'string "org.freedesktop.Notifications"' | awk '/uint32/ { print $2 }')
if [ "$flags" = 4 ]; then ok "the name is requested with DO_NOT_QUEUE alone"; else fail "RequestName flags were '$flags', expected 4"; fi

# A second start loses the name, says so with its own status, and never
# needs the compositor to find that out.
status "a second start exits with 3" 3 "$bin"
status "and does so without a compositor" 3 env WAYLAND_DISPLAY=no-such-display "$bin"

check "do-not-disturb starts off" test "$("$bin" --dnd status)" = off
"$bin" --dnd on
check "and reads on once it is set" test "$("$bin" --dnd status)" = on
"$bin" --dnd off

# Another client cannot take over a notification whose sender is still on
# the bus. notify-send --wait stays connected until its notification closes.
notify-send --print-id --wait -t 0 -a Pairing 'Pair with phone?' >"$work/first" 2>/dev/null &
started="$started $!"
wait_for test -s "$work/first"
first=$(head -n 1 "$work/first")
other=$(id_of "$(notify Updates "$first" '' 'Update ready' '' "['allow','Later']" '{}' 0)")
if [ -n "$first" ] && [ -n "$other" ] && [ "$first" != "$other" ]; then
    ok "a replace by another client gets an id of its own"
else
    fail "a replace by another client kept id '$first' (got '$other')"
fi
"$bin" --dismiss-all

# Once the sender has gone, its id can be reused, as notify-send -r does.
gone=$(id_of "$(notify Volume 0 '' 'Volume 40%' '' '[]' '{}' 0)")
again=$(id_of "$(notify Volume "$gone" '' 'Volume 45%' '' '[]' '{}' 0)")
if [ -n "$gone" ] && [ "$gone" = "$again" ]; then
    ok "a notification whose sender has left can be replaced"
else
    fail "replacing id '$gone' of a departed sender gave '$again'"
fi
"$bin" --dismiss-all

# Reply before signal. Many clients at once, each with a notification that
# is closed as soon as the service is able to: by its timeout, by being
# pushed out of a full queue, and while do-not-disturb holds it back.
owner=$(gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
    --method org.freedesktop.DBus.GetNameOwner org.freedesktop.Notifications | sed "s/[^:.0-9]//g")
dbus-monitor --session >"$work/monitor.log" 2>/dev/null &
monitor=$!
started="$started $monitor"
sleep 0.5
burst() {
    n=0
    while [ $n -lt 45 ]; do
        notify Burst 0 '' "Burst $n" '' '[]' '{}' 1 >/dev/null 2>&1 &
        n=$((n + 1))
    done
    wait_notifies
}
# Waits for the gdbus calls only; the service and the monitor run on.
wait_notifies() {
    while pgrep -P $$ gdbus >/dev/null 2>&1; do sleep 0.1; done
}
burst
sleep 3
"$bin" --dnd on
burst
sleep 3
"$bin" --dnd off
kill $monitor 2>/dev/null
wait $monitor 2>/dev/null

verdict=$(awk -v owner="$owner" '
    /^(method call|method return|signal|error)/ {
        reply = ($1 == "method" && $2 == "return" && index($0, "sender=" owner " "))
        closing = ($1 == "signal" && index($0, "member=NotificationClosed"))
        first = 1
        next
    }
    first && /^ +uint32 / {
        if (reply) { replied[$2] = 1; replies++ }
        if (closing) {
            closed++
            if (!($2 in replied)) { early++; if (example == "") example = $2 }
        }
    }
    { first = 0 }
    END { printf "%d %d %d %s\n", replies, closed, early, example }
' "$work/monitor.log")
set -- $verdict
if [ "$2" -lt 40 ]; then
    fail "only $2 notifications were seen to close ($1 replies); the check proves nothing"
elif [ "$3" -ne 0 ]; then
    fail "$3 of $2 notifications closed before their Notify call was answered, id $4 for one"
else
    ok "all $2 closed notifications were announced after their reply"
fi

check "the service is still running" kill -0 $service

# When the bus goes away the service goes too, with a status of its own.
dbus-daemon --session --nofork --print-address=3 3>"$work/address" >/dev/null 2>&1 &
bus=$!
started="$started $bus"
wait_for test -s "$work/address"
(
    DBUS_SESSION_BUS_ADDRESS=$(cat "$work/address")
    export DBUS_SESSION_BUS_ADDRESS
    "$bin" 2>/dev/null &
    orphan=$!
    wait_for owned && answers || exit 100
    kill $bus
    wait $orphan
)
got=$?
if [ $got -eq 5 ]; then ok "losing the bus exits with 5"; else fail "losing the bus: exit status $got, expected 5"; fi

finish
