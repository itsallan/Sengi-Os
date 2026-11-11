#!/bin/sh
# Styles come from tokens/sengi.toml and nowhere else. This fails if a config
# template, UI source or image file carries a literal colour, or sets a
# corner radius or font family by hand.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
fail=0

report() {
	if [ -n "$2" ]; then
		echo "$1:" >&2
		echo "$2" | sed 's/^/  /' >&2
		fail=1
	fi
}

# Generated Slint files are checked through their .in sources.
files=$(find image/rootfs ui crates/*/ui -type f \
	! -name '*.png' ! -name tokens.slint ! -name components.slint ! -path '*/target/*' 2>/dev/null)

# shellcheck disable=SC2086
report "literal colour (use a colour token)" \
	"$(grep -nE '#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?\b|rgba?\(' $files /dev/null || true)"
# shellcheck disable=SC2086
report "hand-written radius (use radius.base)" \
	"$(grep -nEi '(radius|rx|ry)["=: ]+[1-9]' $files /dev/null || true)"
# shellcheck disable=SC2086
report "hand-written font family (use type.family or type.mono)" \
	"$(grep -nEi '(font-family|font)[=: ]+"?(inter|dejavu|sans|serif|mono)' $files /dev/null || true)"

[ $fail -eq 0 ] && echo "styles ok"
exit $fail
