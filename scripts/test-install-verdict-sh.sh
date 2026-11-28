#!/bin/sh
# Tests the part of scripts/install-test.sh that turns what the guest said and
# how its boot menu behaved into pass or fail: either one alone must fail the
# run. The lines are taken out of the script as they stand, so no ISO and no
# virtual machine are needed. Meant to run under dash as well as bash:
#   sh scripts/test-install-verdict-sh.sh; dash scripts/test-install-verdict-sh.sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

sed -n '/^if grep -q SENGI-FAIL/,/^root_node=/p' "$root/scripts/install-test.sh" |
	sed '/^root_node=/d' >"$tmp/verdict.sh"
[ -s "$tmp/verdict.sh" ] || {
	echo "FAIL: the verdict is no longer where this test looks for it" >&2
	exit 1
}

failures=0
# check NAME GUEST-LINE MENU-FAILURE WANTED
check() {
	echo "$2" >"$tmp/serial-installed.log"
	# shellcheck disable=SC2016
	got=$(out=$tmp menu_fail=$3 sh -euc '
		fail=0
		annotate() { :; }
		. "$out/verdict.sh"
		echo "$fail"' 2>/dev/null) || got="did not run"
	if [ "$got" = "$4" ]; then
		echo "ok   $1"
	else
		echo "FAIL $1: wanted '$4', got '$got'" >&2
		failures=$((failures + 1))
	fi
}

check "a clean run passes" "SENGI-LOGIN" "" 0
check "a complaint from the guest fails" "SENGI-FAIL no sound" "" 1
check "a boot menu that does not work fails on its own" "SENGI-LOGIN" "no menu" 1
check "both together fail" "SENGI-FAIL no sound" "no menu" 1

[ "$failures" -eq 0 ] || {
	echo "$failures check(s) failed" >&2
	exit 1
}
