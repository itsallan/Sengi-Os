# Sourced by the scripts that start a compositor: the desktop session and
# the login screen.
#
# On a display with no GPU acceleration (plain VMs, firmware framebuffers)
# Mesa would fall back to llvmpipe, which is slower than wlroots' own
# software renderer and drags LLVM into the compositor.
if [ -z "${WLR_RENDERER:-}" ]; then
	hw=0
	for drv in /sys/class/drm/card[0-9]*/device/driver; do
		case $(basename "$(readlink -f "$drv")") in
		bochs-drm | simple-framebuffer | simpledrm | virtio-pci | virtio_gpu | "") ;;
		*) hw=1 ;;
		esac
	done
	[ $hw = 1 ] || export WLR_RENDERER=pixman
	unset hw drv
fi
