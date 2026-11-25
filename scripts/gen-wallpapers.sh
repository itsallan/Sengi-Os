#!/bin/sh
# Draw the wallpapers Sengi OS ships into the image overlay. They come from
# the design tokens, so run this again after a colour changes; the test of
# crates/sengi-wallpapers fails when the committed files are stale.
# Needs cargo, so run it in the builder VM.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
exec cargo run -q --release -p sengi-wallpapers -- image/rootfs/usr/share/backgrounds/sengi
