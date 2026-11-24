#!/bin/sh
# Writes the licence notices of the Rust crates our programs are built from,
# for the image. Needs cargo and cargo-about, so run it where they are, such
# as the builder VM:
#
#   scripts/vm.sh run scripts/gen-notices.sh
#
# then copy image/rootfs/usr/share/doc/sengi-os/THIRD-PARTY-CRATES.txt back
# from the VM.
#
# Run it again whenever a Cargo.lock changes. The licences it accepts are
# listed in scripts/notices/about.toml.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
config=$root/scripts/notices/about.toml
template=$root/scripts/notices/about.hbs
out=$root/image/rootfs/usr/share/doc/sengi-os/THIRD-PARTY-CRATES.txt
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

command -v cargo-about >/dev/null || { echo "cargo-about is not installed" >&2; exit 1; }

(cd "$root" && cargo about generate --locked -c "$config" -o "$tmp/main.txt" "$template")
(cd "$root/crates/sengi-compositor" && cargo about generate --locked -c "$config" -o "$tmp/compositor.txt" "$template")

mkdir -p "$(dirname "$out")"
{
	cat "$tmp/main.txt"
	printf '\n\n========================================================================\n'
	printf 'The same for sengi-compositor, which is built on its own.\n'
	printf '========================================================================\n\n'
	cat "$tmp/compositor.txt"
} >"$out"
echo "wrote $out ($(wc -c <"$out") bytes)"
