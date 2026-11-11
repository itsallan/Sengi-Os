#!/bin/sh
# Checks keyboard input end to end: runs examples/typing under a headless
# labwc, types into it with wtype and compares the text it prints on Escape.
#
#   cargo build -p sengi-layer --example typing
#   crates/sengi-layer/tests/keyboard.sh target/debug/examples/typing
#
# Needs labwc and wtype. Compose sequences also need the X11 locale data
# (libx11-data on Debian).
set -u

typing=${1:?usage: keyboard.sh path/to/examples/typing}
for tool in labwc wtype; do
    command -v "$tool" >/dev/null || { echo "keyboard.sh: $tool is not installed" >&2; exit 2; }
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export XDG_RUNTIME_DIR="$work"
export WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1
# The compose table is chosen by locale.
export LC_ALL=C.UTF-8
unset WAYLAND_DISPLAY DISPLAY

failed=0

# wait_for <tenths of a second> <command...>: until the command succeeds.
wait_for() {
    tries=$1
    shift
    until "$@" 2>/dev/null || [ "$tries" -le 0 ]; do
        sleep 0.1
        tries=$((tries - 1))
    done
}

# check <name> <expected text> <shell commands that type>
# An expected text starting with ~ is an extended regular expression.
check() {
    name=$1
    expected=$2
    keys=$3
    rm -f "$work/out" "$work/err"

    # labwc runs the command without a shell, so bring one for the redirects.
    labwc -s "sh -c '$typing >$work/out 2>$work/err'" >"$work/labwc.log" 2>&1 &
    labwc=$!
    wait_for 100 test -S "$work/wayland-0"

    (
        export WAYLAND_DISPLAY=wayland-0
        # A headless seat has no keyboard until wtype brings one. Keep one
        # alive for the whole case so the keys are not typed at a seat
        # that is still gaining its first keyboard.
        wtype -s 60000 -k Shift_L &
        holder=$!
        # The example says so when it has the keyboard, and prints the
        # text when Escape has reached it. Waiting for both, rather than
        # for a fixed time, keeps a slow machine from failing the test.
        wait_for 100 grep -q "focus true" "$work/err"
        sh -c "$keys"
        sleep 0.3
        wtype -k Escape
        wait_for 50 test -s "$work/out"
        kill $holder 2>/dev/null
    )

    kill $labwc 2>/dev/null
    wait $labwc 2>/dev/null
    actual=$(cat "$work/out" 2>/dev/null)
    rm -f "$work"/wayland-0*
    case $expected in
        "~"*) printf '%s\n' "$actual" | grep -Eq -- "${expected#"~"}" && matched=1 || matched=0 ;;
        *) [ "$actual" = "$expected" ] && matched=1 || matched=0 ;;
    esac
    if [ $matched -eq 1 ]; then
        echo "ok    $name"
    else
        echo "FAIL  $name: expected '$expected', got '$actual'"
        sed 's/^/      /' "$work/err"
        failed=1
    fi
}

check "plain text and Backspace" "Hello Sengi OS!" \
    'wtype "Hello Sengi OS!x"; wtype -k BackSpace'

check "Ctrl+A replaces everything" "Z" \
    'wtype abc; wtype -M ctrl a -m ctrl; wtype Z'

check "arrows, Home, End and Delete" "<ab>" \
    'wtype abcd; wtype -k Left -k Left -k Delete; wtype -k Home; wtype "<"; wtype -k End; wtype ">"; wtype -k Left -k BackSpace'

check "text outside ASCII" "żółć" \
    'wtype "żółć"'

check "a compose sequence types only its result" "é" \
    'wtype -k Multi_key -k apostrophe -k e'

# 25 a second after 600 ms is the labwc default; allow for a slow machine.
check "a held key repeats" "~^a{8,24}$" \
    'wtype -P a -s 1200 -p a'

check "hidden and shown again keeps text and keyboard" "abc" \
    'wtype ab; wtype -k Tab; sleep 1.6; wtype c'

exit $failed
