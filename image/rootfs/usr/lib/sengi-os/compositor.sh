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
# Each of them can ask for labwc instead, with "labwc" in place of "own":
# the boot entry "Sengi OS, labwc compositor" does, and so can a file that
# is left in /etc/sengi-os to override a preview word.
#
# Precedence, first that applies:
#   1. a start of ours that failed earlier in this boot: labwc. It is
#      recorded in /run/sengi-os/compositor-failed, see own_compositor.
#   2. any selector that says labwc: labwc.
#   3. any selector that says own: ours.
#   4. labwc.
# The login screen and the session follow the same choice.

# Prints "own" or "labwc". SENGI_CMDLINE, SENGI_COMPOSITOR_FILES and
# SENGI_FAILED_FILE are for the tests of this file.
wanted_compositor() {
	own=0
	labwc=0
	failed=${SENGI_FAILED_FILE:-/run/sengi-os/compositor-failed}
	read -r cmdline <"${SENGI_CMDLINE:-/proc/cmdline}" || true
	case " $cmdline " in
	*" sengi.compositor=own "*) own=1 ;;
	esac
	case " $cmdline " in
	*" sengi.compositor=labwc "*) labwc=1 ;;
	esac
	for choice in ${SENGI_COMPOSITOR_FILES:-/run/sengi-os.compositor /etc/sengi-os/compositor}; do
		word=
		if [ -r "$choice" ]; then
			read -r word <"$choice" || true
		fi
		case $word in
		own) own=1 ;;
		labwc) labwc=1 ;;
		esac
	done
	if [ -e "$failed" ] || [ "$labwc" = 1 ] || [ "$own" = 0 ]; then
		echo labwc
	else
		echo own
	fi
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

# Notes that sengi-compositor died without being asked to, so that the
# rest of this boot uses labwc. The directory is made by tmpfiles.d, open to
# every user: the login screen and the session run as different ones.
note_compositor_failed() {
	printf '' >"${SENGI_FAILED_FILE:-/run/sengi-os/compositor-failed}" 2>/dev/null || true
}

# Runs sengi-compositor on the screen with the arguments given and ends
# the script with it. It is run as a child so that one that cannot start
# does not leave a black screen. It is given 15 seconds to say it is ready,
# by the file sengi-compositor.ready in XDG_RUNTIME_DIR (a screen is lit and
# the session program runs); one that is silent for that long is hung and
# is killed. A compositor that is killed, or that ends with an error within
# ten seconds of the start, which includes the session program failing, makes
# this return, once, for labwc to be started in its place. One that ends
# with an error later is not replaced from here, as the session is over by
# then and the user would see it change under them; the next start in this
# boot uses labwc because of the note. SENGI_READY_TICKS, SENGI_READY_POLL
# (seconds) and SENGI_FAST_SECONDS are for the tests of this file.
own_compositor() {
	ready=
	if [ -n "${XDG_RUNTIME_DIR:-}" ]; then
		ready=$XDG_RUNTIME_DIR/sengi-compositor.ready
		rm -f "$ready"
	fi
	read -r up _ </proc/uptime
	started=${up%.*}
	systemd-cat -t sengi-compositor sengi-compositor --backend drm "$@" &
	pid=$!
	# greetd ends the session by signalling this shell. The signal cuts the
	# wait short, so wait once more for the compositor to shut down.
	stopping=0
	hung=0
	trap 'stopping=1; kill -TERM "$pid" 2>/dev/null' TERM HUP INT
	if [ -n "$ready" ]; then
		ticks=0
		while [ ! -e "$ready" ] && [ "$stopping" = 0 ] && kill -0 "$pid" 2>/dev/null; do
			if [ "$ticks" -ge "${SENGI_READY_TICKS:-150}" ]; then
				hung=1
				break
			fi
			sleep "${SENGI_READY_POLL:-0.1}"
			ticks=$((ticks + 1))
		done
	fi
	if [ "$hung" = 1 ]; then
		echo "sengi-compositor did not say it was ready, ending it" |
			systemd-cat -t sengi-session -p warning
		kill -TERM "$pid" 2>/dev/null
		sleep 1
		kill -KILL "$pid" 2>/dev/null || true
	fi
	status=0
	wait "$pid" || status=$?
	if [ "$stopping" = 1 ]; then
		status=0
		wait "$pid" || status=$?
	fi
	trap - TERM HUP INT
	rm -f "$ready"
	[ "$hung" = 0 ] || [ "$status" -ne 0 ] || status=1
	read -r up _ </proc/uptime
	lasted=$((${up%.*} - started))
	# Told to stop is not a failure to start: labwc must not take over.
	if [ "$stopping" = 1 ] || { [ "$status" -eq 0 ] && [ "$hung" = 0 ]; }; then
		exit "$status"
	fi
	note_compositor_failed
	if [ "$hung" = 0 ] && [ "$lasted" -ge "${SENGI_FAST_SECONDS:-10}" ]; then
		exit "$status"
	fi
	echo "sengi-compositor gave up after $lasted s with status $status, starting labwc instead" |
		systemd-cat -t sengi-session -p warning
}
