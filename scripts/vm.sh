#!/bin/sh
# Builder VM for hosts where podman cannot run (for example inside a sandbox
# that sets no_new_privs). It boots a stock Debian cloud image under QEMU and
# gives it podman, so scripts/build-iso.sh runs in the VM exactly as it would
# on the host. Nothing here needs root on the host.
#
#   vm.sh up            download the image if needed, boot, wait for ssh
#   vm.sh run CMD...    run a command in the VM, in the synced repo
#   vm.sh sync          copy the working tree into the VM
#   vm.sh fetch         copy out/ from the VM back into the repo
#   vm.sh down          power the VM off
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
dir=$root/cache/vm
image_url=${SENGI_VM_IMAGE:-https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2}
port=${SENGI_VM_PORT:-2222}
cpus=${SENGI_VM_CPUS:-3}
mem=${SENGI_VM_MEM:-6144}
key=$dir/id_ed25519

ssh_opts="-F /dev/null -i $key -p $port -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=5"

vm_ssh() {
	# shellcheck disable=SC2086
	ssh $ssh_opts debian@127.0.0.1 "$@"
}

up() {
	mkdir -p "$dir"
	if [ -f "$dir/qemu.pid" ] && kill -0 "$(cat "$dir/qemu.pid")" 2>/dev/null; then
		echo "vm already running"
		return
	fi
	[ -f "$dir/base.qcow2" ] || curl -fL --retry 3 -o "$dir/base.qcow2" "$image_url"
	[ -f "$key" ] || ssh-keygen -q -t ed25519 -N '' -f "$key"
	if [ ! -f "$dir/disk.qcow2" ]; then
		qemu-img create -q -f qcow2 -b base.qcow2 -F qcow2 "$dir/disk.qcow2" 40G
	fi
	mkdir -p "$dir/seed"
	printf 'instance-id: sengi-builder\nlocal-hostname: sengi-builder\n' >"$dir/seed/meta-data"
	cat >"$dir/seed/user-data" <<SEED
#cloud-config
ssh_authorized_keys:
  - $(cat "$key.pub")
package_update: true
packages: [podman, uidmap, passt, fuse-overlayfs, rsync]
runcmd:
  - touch /var/lib/sengi-builder-ready
SEED
	genisoimage -quiet -output "$dir/seed.iso" -volid cidata -joliet -rock \
		"$dir/seed/user-data" "$dir/seed/meta-data"
	accel=tcg
	[ -w /dev/kvm ] && accel=kvm
	qemu-system-x86_64 -machine q35,accel=$accel -cpu max -smp "$cpus" -m "$mem" \
		-drive file="$dir/disk.qcow2",if=virtio,cache=unsafe \
		-drive file="$dir/seed.iso",if=virtio,format=raw,readonly=on \
		-netdev user,id=n0,hostfwd=tcp:127.0.0.1:"$port"-:22 -device virtio-net-pci,netdev=n0 \
		-display none -serial file:"$dir/console.txt" \
		-daemonize -pidfile "$dir/qemu.pid"
	echo "waiting for the vm"
	i=0
	until vm_ssh test -f /var/lib/sengi-builder-ready 2>/dev/null; do
		i=$((i + 1))
		[ $i -lt 120 ] || { echo "vm did not come up, see $dir/console.txt" >&2; exit 1; }
		sleep 5
	done
	echo "vm ready"
}

sync_tree() {
	# shellcheck disable=SC2086
	rsync -a --delete -e "ssh $ssh_opts" \
		--exclude /cache --exclude /out --exclude /build --exclude /target --exclude /test-output \
		"$root/" debian@127.0.0.1:sengi-os/
}

case "${1:-}" in
up) up ;;
sync) sync_tree ;;
run)
	shift
	sync_tree
	vm_ssh "cd sengi-os && $*"
	;;
fetch)
	mkdir -p "$root/out"
	# shellcheck disable=SC2086
	rsync -a -e "ssh $ssh_opts" debian@127.0.0.1:sengi-os/out/ "$root/out/"
	;;
down)
	vm_ssh sudo poweroff 2>/dev/null || true
	;;
*)
	echo "usage: vm.sh up|run CMD...|sync|fetch|down" >&2
	exit 2
	;;
esac
