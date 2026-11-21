#!/bin/sh
# mmdebstrap customize hook. $1 is the new root. Copies the rendered overlay
# in and does the setup that needs the packages to be installed first.
set -eu

root=$1
: "${VERSION:?}" "${OVERLAY:?}"

cp -a "$OVERLAY/." "$root/"
# What the overlay brought belongs to root, and only that is touched. A
# chown over all of /etc and /usr takes the setuid and setgid bits off
# every program that has them, sudo and su among them, and gives
# /etc/shadow to a group that unix_chkpwd is no longer in: no password
# could be checked, so a locked screen could not be unlocked.
(cd "$OVERLAY" && find . ! -name .) | while IFS= read -r entry; do
	chown -h 0:0 "$root/$entry"
done
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
# netdev is the group iwd lets manage Wi-Fi, and bluetooth the one Debian's
# BlueZ policy lets talk to the Bluetooth service.
chroot "$root" groupadd -f -r netdev
chroot "$root" groupadd -f -r bluetooth
chroot "$root" useradd -m -u 1000 -s /bin/bash -c "Sengi OS" -G sudo,audio,video,input,render,netdev,bluetooth live
echo 'live:$6$sengioslive$upm4wU690nU7M6MY.tBcr8eE3usBYMTsW9k9mOAJ6ihPIEp1TGfFGJ0VTE30AUgz3f89ilMcM0KlC2ZSayiAz.' |
	chroot "$root" chpasswd -e
chroot "$root" passwd -l root >/dev/null

# Only what the desktop needs at boot. Wi-Fi and Bluetooth daemons are
# started by udev when the hardware shows up (see 70-sengi-on-demand.rules).
chroot "$root" systemctl enable greetd systemd-networkd systemd-resolved systemd-oomd \
	systemd-timesyncd sengi-boot-report sengi-install-report
chroot "$root" systemctl disable iwd bluetooth networking 2>/dev/null || true
# What is masked here is masked on an installed system too: the installer
# copies this image. So each of these is a decision for both.
#
# - The apt timers: nothing fetches package lists or installs updates in
#   the background. On the live image there would be no point. On an
#   installed system it means updates come only when the person asks for
#   them (apt update, apt upgrade), until Sengi OS has a way of its own
#   to offer them; the README says so under what is not offered yet.
# - dpkg-db-backup and the e2scrub pair: housekeeping the desktop does
#   not need at boot, and e2scrub wants LVM, which is not used.
# - getty@tty1: the desktop has no use for a text login on the first
#   console. The installer puts it back for the boot menu's text mode
#   and for nothing else.
# - grub-common.service notes at every start that the start worked, for a
#   menu that falls back after a failed one. Ours does not.
# - systemd-networkd-wait-online: nothing at boot waits for the network.
chroot "$root" systemctl mask systemd-networkd-wait-online.service \
	apt-daily.timer apt-daily-upgrade.timer dpkg-db-backup.timer e2scrub_all.timer \
	e2scrub_reap.service getty@tty1.service grub-common.service
# The services that bring /etc and /var up to date with a /usr that was
# replaced whole have nothing to do here, live or installed: packages run
# ldconfig and their own triggers when apt installs them. ldconfig alone
# costs over a second at boot.
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

