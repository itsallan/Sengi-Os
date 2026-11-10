#!/bin/sh
# Build the Sengi OS ISO inside a container. Output lands in out/.
# Needs podman (rootless is fine). Set SENGI_ENGINE=docker to use docker.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
. "$root/image/build.conf"
version=$(cat "$root/VERSION")
engine=${SENGI_ENGINE:-podman}
rust=$(sed -n 's/^channel = "\(.*\)"/\1/p' "$root/rust-toolchain.toml")
tag=sengi-os-builder:$SNAPSHOT-rust$rust

"$engine" build -q -t "$tag" --build-arg SNAPSHOT="$SNAPSHOT" --build-arg RUST_VERSION="$rust" \
	-f "$root/image/Containerfile" "$root/image" >/dev/null

mkdir -p "$root/out"
"$engine" run --rm --privileged --security-opt label=disable \
	-v "$root":/src:ro -v "$root/out":/out -v /work \
	-e SUITE="$SUITE" -e SNAPSHOT="$SNAPSHOT" -e SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
	-e VERSION="$version" -e SENGI_NONFREE_FIRMWARE="${SENGI_NONFREE_FIRMWARE:-0}" \
	"$tag" /src/image/build.sh

cat "$root/out/sengi-os-$version-amd64.iso.sha256"
