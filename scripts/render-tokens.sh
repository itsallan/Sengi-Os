#!/bin/sh
# Render @{token} placeholders from tokens/sengi.toml.
#
#   render-tokens.sh FILE.in              print FILE.in rendered to stdout
#   render-tokens.sh --tree SRC DST       render SRC/**.in into DST, copy the rest
#   render-tokens.sh                      regenerate committed outputs (ui/tokens.slint)
#   render-tokens.sh --check              fail if committed outputs are stale
#
# Filters: @{color.bg|hex8} -> 16171bff, @{color.bg|nohash} -> 16171b(ff),
#          @{color.bar|rgba} -> rgba(22, 23, 27, 0.90)
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
tokens=${SENGI_TOKENS:-$root/tokens/sengi.toml}

render() {
	awk -v tokfile="$tokens" -v src="$1" '
	function hv(c) { return index("0123456789abcdef", tolower(c)) - 1 }
	function byte(s, i) { return hv(substr(s, i, 1)) * 16 + hv(substr(s, i + 1, 1)) }
	function filt(v, f,    h) {
		if (f == "") return v
		if (v !~ /^#[0-9a-fA-F]+$/ || (length(v) != 7 && length(v) != 9)) {
			printf "%s: filter %s needs a colour, got %s\n", src, f, v > "/dev/stderr"; exit 2
		}
		h = tolower(substr(v, 2))
		if (f == "nohash") return h
		if (length(h) == 6) h = h "ff"
		if (f == "hex8") return h
		if (f == "rgba") return sprintf("rgba(%d, %d, %d, %.2f)", byte(h, 1), byte(h, 3), byte(h, 5), byte(h, 7) / 255)
		printf "%s: unknown filter %s\n", src, f > "/dev/stderr"; exit 2
	}
	BEGIN {
		sec = ""
		while ((getline line < tokfile) > 0) {
			sub(/^[ \t]+/, "", line)
			if (line ~ /^#/ || line == "") continue
			if (line ~ /^\[[a-z0-9-]+\]/) { sec = substr(line, 2, index(line, "]") - 2); continue }
			eq = index(line, "=")
			if (!eq) continue
			k = substr(line, 1, eq - 1); v = substr(line, eq + 1)
			gsub(/[ \t]+$/, "", k); sub(/^[ \t]+/, "", v)
			if (v ~ /^"/) { v = substr(v, 2); v = substr(v, 1, index(v, "\"") - 1) }
			else { sub(/[ \t]*#.*$/, "", v); gsub(/[ \t]+$/, "", v) }
			tok[sec "." k] = v
		}
		close(tokfile)
	}
	{
		out = ""; rest = $0
		while (match(rest, /@\{[a-z0-9.|-]+\}/)) {
			ref = substr(rest, RSTART + 2, RLENGTH - 3)
			name = ref; f = ""
			if (index(ref, "|")) { name = substr(ref, 1, index(ref, "|") - 1); f = substr(ref, index(ref, "|") + 1) }
			if (!(name in tok)) { printf "%s:%d: unknown token %s\n", src, NR, name > "/dev/stderr"; exit 2 }
			out = out substr(rest, 1, RSTART - 1) filt(tok[name], f)
			rest = substr(rest, RSTART + RLENGTH)
		}
		print out rest
	}' "$1"
}

generated() {
	printf "%s\n" "ui/tokens.slint.in ui/tokens.slint" "ui/components.slint.in ui/components.slint"
}

case "${1:-}" in
--tree)
	src=$2 dst=$3
	mkdir -p "$dst"
	(cd "$src" && find . -type f) | while IFS= read -r f; do
		mkdir -p "$dst/$(dirname "$f")"
		case $f in
		*.in) render "$src/$f" >"$dst/${f%.in}"
		      chmod --reference="$src/$f" "$dst/${f%.in}" 2>/dev/null || true ;;
		*) cp -p "$src/$f" "$dst/$f" ;;
		esac
	done
	;;
--check)
	stale=0
	generated | while read -r in out; do
		if ! render "$root/$in" | cmp -s - "$root/$out"; then
			echo "stale: $out (run scripts/render-tokens.sh)" >&2
			exit 1
		fi
	done || stale=1
	exit $stale
	;;
"")
	generated | while read -r in out; do
		render "$root/$in" >"$root/$out"
		echo "rendered $out"
	done
	;;
*)
	render "$1"
	;;
esac
