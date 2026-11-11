#!/bin/sh
# mmdebstrap customize hook. $1 is the new root. Copies the rendered overlay
# in and does the setup that needs the packages to be installed first.
set -eu

root=$1
: "${VERSION:?}" "${OVERLAY:?}"

cp -a "$OVERLAY/." "$root/"
chown -R 0:0 "$root/etc" "$root/usr"
chmod 0440 "$root/etc/sudoers.d/live"

rm -f "$root/etc/os-release"
cat >"$root/etc/os-release" <<REL
NAME="Sengi OS"
PRETTY_NAME="Sengi OS $VERSION"
ID=sengios
ID_LIKE=debian
VERSION="$VERSION"
VERSION_ID="$VERSION"
HOME_URL="https://github.com/itsallan/Sengi-Os"
BUG_REPORT_URL="https://github.com/itsallan/Sengi-Os/issues"
REL
echo sengios >"$root/etc/hostname"
printf '127.0.0.1\tlocalhost\n127.0.1.1\tsengios\n::1\tlocalhost ip6-localhost ip6-loopback\n' >"$root/etc/hosts"

# Live user. The password is "live" so the lock screen can be unlocked; the
# hash is fixed to keep the image reproducible.
# netdev is the group iwd lets manage Wi-Fi.
chroot "$root" groupadd -f -r netdev
chroot "$root" useradd -m -u 1000 -s /bin/bash -c "Sengi OS" -G sudo,audio,video,input,render,netdev live
echo 'live:$6$sengioslive$upm4wU690nU7M6MY.tBcr8eE3usBYMTsW9k9mOAJ6ihPIEp1TGfFGJ0VTE30AUgz3f89ilMcM0KlC2ZSayiAz.' |
	chroot "$root" chpasswd -e
chroot "$root" passwd -l root >/dev/null

# Only what the desktop needs at boot. Wi-Fi and Bluetooth daemons are
# started by udev when the hardware shows up (see 70-sengi-on-demand.rules).
chroot "$root" systemctl enable greetd systemd-networkd systemd-resolved systemd-oomd \
	systemd-timesyncd sengi-boot-report
chroot "$root" systemctl disable iwd bluetooth networking 2>/dev/null || true
chroot "$root" systemctl mask systemd-networkd-wait-online.service \
	apt-daily.timer apt-daily-upgrade.timer dpkg-db-backup.timer e2scrub_all.timer \
	e2scrub_reap.service getty@tty1.service
# The image is read-only and complete, so there is nothing for the
# first-boot update services to do. ldconfig alone costs over a second.
chroot "$root" ldconfig
chroot "$root" systemctl mask ldconfig.service systemd-journal-catalog-update.service \
	systemd-hwdb-update.service systemd-update-done.service
# pipewire-pulse stays socket activated; the Bluetooth media proxy is not
# needed until a Bluetooth audio device is in use.
chroot "$root" systemctl --global disable pipewire-pulse.service
chroot "$root" systemctl --global mask mpris-proxy.service
chroot "$root" systemctl set-default graphical.target

# The initramfs was built when the kernel was installed, before our boot
# script and hook were in place.
chroot "$root" update-initramfs -u

# See the note in /etc/xdg/labwc/autostart.
mkdir -p "$root/usr/share/sengi-os/dbus"
chroot "$root" dpkg-divert --quiet --rename \
	--divert /usr/share/sengi-os/dbus/org.freedesktop.portal.Desktop.service \
	--add /usr/share/dbus-1/services/org.freedesktop.portal.Desktop.service

