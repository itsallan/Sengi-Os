# Sourced by the scripts that start a compositor: the desktop session and
# the login screen. Which compositor, and how ours is started.
#
# labwc is the compositor unless ours is asked for, which is done in one
# of three ways:
#   - the word sengi.compositor=own on the kernel command line: the live
#     image's "sengi-compositor preview" boot entry, or the word added by
#     hand to any entry (press E on it in the boot menu). For that start.
#   - a file /etc/sengi-os/compositor that says "own": on an installed
#     system, for every start until the file is removed.
#   - a file /run/sengi-os.compositor that says the same: until the next
#     start. The boot test makes it.
# The login screen and the session follow the same choice.

# Prints "own" or "labwc".
wanted_compositor() {
	wanted=labwc
	read -r cmdline <"${SENGI_CMDLINE:-/proc/cmdline}"
	case " $cmdline " in
	*" sengi.compositor=own "*) wanted=own ;;
	esac
	for choice in /run/sengi-os.compositor /etc/sengi-os/compositor; do
		word=
		if [ -r "$choice" ]; then
			read -r word <"$choice" || true
		fi
		[ "$word" != own ] || wanted=own
	done
	echo "$wanted"
}

# Exports the NAME=value lines of an environment file as labwc reads one:
# the keyboard layout, the cursor size and the time zone are kept in such
# files, by Settings and by the installer. labwc reads its own; for our
# compositor it is done here, so that it and everything it starts have
# them. A file that is not there is no error.
export_environment() {
	[ -r "$1" ] || return 0
	while IFS= read -r line || [ -n "$line" ]; do
		case $line in
		'#'* | '') continue ;;
		*=*) ;;
		*) continue ;;
		esac
		name=${line%%=*}
		case $name in
		'' | [0-9]* | *[!A-Za-z0-9_]*) continue ;;
		esac
		export "$name=${line#*=}"
	done <"$1"
}

# Runs sengi-compositor on the screen with the arguments given and ends
# the script with it. It is run as a child so that one that cannot start
# does not leave a black screen: if it gives up with an error within ten
# seconds this returns, once, for labwc to be started in its place.
own_compositor() {
	read -r up _ </proc/uptime
	started=${up%.*}
	systemd-cat -t sengi-compositor sengi-compositor --backend drm "$@" &
	pid=$!
	# greetd ends the session by signalling this shell. The signal cuts the
	# wait short, so wait once more for the compositor to shut down.
	stopping=0
	trap 'stopping=1; kill -TERM "$pid" 2>/dev/null' TERM HUP INT
	status=0
	wait "$pid" || status=$?
	if [ "$stopping" = 1 ]; then
		status=0
		wait "$pid" || status=$?
	fi
	trap - TERM HUP INT
	read -r up _ </proc/uptime
	lasted=$((${up%.*} - started))
	if [ "$status" -eq 0 ] || [ "$lasted" -ge 10 ]; then
		exit "$status"
	fi
	echo "sengi-compositor gave up after $lasted s with status $status, starting labwc instead" |
		systemd-cat -t sengi-session -p warning
}
