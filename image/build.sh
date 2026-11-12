#!/bin/sh
# Runs as root inside the builder container (see scripts/build-iso.sh).
# Bootstraps Debian, applies the Sengi OS overlay, and writes a hybrid
# BIOS/UEFI live ISO to /out.
set -eu

: "${SUITE:?}" "${SNAPSHOT:?}" "${SOURCE_DATE_EPOCH:?}" "${VERSION:?}"
export SOURCE_DATE_EPOCH LC_ALL=C TZ=UTC

src=/src
work=${WORK:-/work}
out=${OUT:-/out}
name=sengi-os-$VERSION-amd64
mirror=http://snapshot.debian.org/archive/debian/$SNAPSHOT
security=http://snapshot.debian.org/archive/debian-security/$SNAPSHOT

rm -rf "$work/root" "$work/iso" "$work/overlay" "$work/target"
mkdir -p "$work/iso/live" "$work/iso/boot/grub" "$out"

"$src/scripts/render-tokens.sh" --tree "$src/image/rootfs" "$work/overlay"

# Our own programs, built against the same Debian the image is made of.
export CARGO_HOME="$work/cargo" CARGO_TARGET_DIR="$work/target"
export RUSTFLAGS="--remap-path-prefix=$src=. --remap-path-prefix=$work=/build"
programs="sengi-bar sengi-dock sengi-focus sengi-launcher sengi-notify sengi-quick"
set --
for program in $programs; do
	set -- "$@" -p "$program"
done
cargo build --release --locked --manifest-path "$src/Cargo.toml" "$@"
# The launcher starts on demand and has to stay small. This one build shares
# cargo features between the programs, so a shared crate that lost a feature
# gate would put a D-Bus stack or an image decoder into it unnoticed. The
# check finds a crate by the source paths of its panic locations, which
# survive in the binary: it would find nothing in a build that dropped
# those. A bare "png" would not do: the font code has a png.rs of its own.
for marker in zbus resvg '/png-[0-9]'; do
	if grep -a -q -e "$marker" "$work/target/release/sengi-launcher"; then
		echo "sengi-launcher has $marker linked in, which it must not" >&2
		exit 1
	fi
done
# Settings is an ordinary window, the one program that wants the toolkit's
# window backend and its font lookup. Built together with the others it
# would turn those features on for all of them, so it gets a build to
# itself and the shell's programs stay what they were without it.
cargo build --release --locked --manifest-path "$src/Cargo.toml" -p sengi-settings
programs="$programs sengi-settings"
for program in $programs; do
	install -D -m 0755 "$work/target/release/$program" "$work/overlay/usr/bin/$program"
done
install -D -m 0644 "$src/crates/sengi-settings/sengi-settings.desktop" \
	"$work/overlay/usr/share/applications/sengi-settings.desktop"

components=main
packages=$(sed -e 's/#.*//' -e '/^[[:space:]]*$/d' "$src/image/packages.list" | paste -sd, -)
if [ "${SENGI_NONFREE_FIRMWARE:-0}" = 1 ]; then
	components="main non-free-firmware"
	packages="$packages,firmware-iwlwifi,firmware-intel-graphics,intel-microcode"
fi

mmdebstrap --variant=minbase \
	--components="$components" \
	--include="$packages" \
	--aptopt='APT::Install-Recommends "false"' \
	--aptopt='Acquire::Check-Valid-Until "false"' \
	--dpkgopt='path-exclude=/usr/share/man/*' \
	--dpkgopt='path-exclude=/usr/share/doc/*' \
	--dpkgopt='path-include=/usr/share/doc/*/copyright' \
	--dpkgopt='path-exclude=/usr/share/locale/*' \
	--dpkgopt='path-exclude=/usr/share/info/*' \
	--customize-hook="VERSION=$VERSION OVERLAY=$work/overlay $src/image/customize.sh \"\$1\"" \
	"$SUITE" "$work/root" \
	"deb $mirror $SUITE $components" \
	"deb $mirror $SUITE-updates $components" \
	"deb $security $SUITE-security $components"

root=$work/root

# Things mmdebstrap manages itself during the bootstrap, so they are set here.
cat >"$root/etc/apt/sources.list" <<LIST
deb http://deb.debian.org/debian $SUITE $components
deb http://deb.debian.org/debian $SUITE-updates $components
deb http://security.debian.org/debian-security $SUITE-security $components
LIST
ln -sf ../run/systemd/resolve/stub-resolv.conf "$root/etc/resolv.conf"

# The kernel and initrd boot from the ISO, not from inside the squashfs.
cp "$root"/boot/vmlinuz-* "$work/iso/live/vmlinuz"
cp "$root"/boot/initrd.img-* "$work/iso/live/initrd.img"
rm -f "$root"/boot/vmlinuz-* "$root"/boot/initrd.img-* "$root"/vmlinuz* "$root"/initrd.img*

# Anything that differs from one build to the next has to go.
rm -rf "$root"/var/lib/apt/lists/* "$root"/var/cache/apt/* "$root"/var/log/* \
	"$root"/var/cache/fontconfig/* \
	"$root"/var/cache/debconf/*-old "$root"/var/lib/dpkg/*-old \
	"$root"/var/lib/systemd/random-seed "$root"/etc/.pwd.lock \
	"$root"/tmp/* "$root"/run/* "$root"/root/.[!.]*
: >"$root/etc/machine-id"

# Ship a font cache so the first program to draw text does not have to build
# one. The cache is only trusted if it is newer than the font directories, and
# the squashfs gives every file the same time, so set that time up front.
find "$root/usr/share/fonts" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
chroot "$root" fc-cache -s -f

env -u SOURCE_DATE_EPOCH mksquashfs "$root" "$work/iso/live/filesystem.squashfs" \
	-noappend -no-progress -comp zstd -Xcompression-level 15 -b 1M \
	-all-time "$SOURCE_DATE_EPOCH" -mkfs-time "$SOURCE_DATE_EPOCH" >/dev/null

sed "s/@VERSION@/$VERSION/g" "$src/image/grub.cfg" >"$work/iso/boot/grub/grub.cfg"
find "$work/iso" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +

# grub-mkrescue stamps the current time into the boot images and the FAT
# volume it builds for UEFI, and has no switch to turn that off. Freezing the
# clock for it is what makes two builds come out identical.
faketime -f "$(date -u -d "@$SOURCE_DATE_EPOCH" '+%Y-%m-%d %H:%M:%S')" \
	grub-mkrescue -o "$out/$name.iso" "$work/iso" \
	-volid SENGIOS \
	2>"$work/xorriso.log" || { cat "$work/xorriso.log" >&2; exit 1; }

(cd "$out" && sha256sum "$name.iso" >"$name.iso.sha256")
chmod a+r "$out/$name.iso" "$out/$name.iso.sha256"
du -h "$out/$name.iso"
