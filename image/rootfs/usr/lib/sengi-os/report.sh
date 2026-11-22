# Sourced by the scripts that report to the QEMU tests over the serial
# port: sengi-boot-report and sengi-install-report.
port=${port:?the script that sources this says where the serial port is}

# Asks logind, as the user of the session on the seat and from inside
# that session, whether it may restart the machine and shut it down. This
# only asks. A process counts as part of a session by the control group
# it is in, so the question is put from a shell moved into the session's.
may_power_off() {
	uid=$(id -u "$1")
	gid=$(id -g "$1")
	session=$(loginctl list-sessions --no-legend | awk -v user="$1" '$3 == user && $4 == "seat0" {print $1; exit}')
	scope=$(loginctl show-session "$session" -p Scope --value 2>/dev/null)
	for method in CanReboot CanPowerOff; do
		# shellcheck disable=SC2016
		answer=$(sh -c 'echo $$ >"$1" && exec setpriv --reuid "$2" --regid "$3" --init-groups \
			busctl call org.freedesktop.login1 /org/freedesktop/login1 org.freedesktop.login1.Manager "$4"' \
			sh "/sys/fs/cgroup/user.slice/user-$uid.slice/$scope/cgroup.procs" "$uid" "$gid" "$method" 2>&1)
		echo "SENGI-INFO $method asked by $1 in session $session: $answer" >"$port"
		if [ "$answer" != 's "yes"' ]; then
			echo "SENGI-FAIL logind does not let $1 use $method: $answer" >"$port"
		fi
	done
}

# Proportional memory of the processes given, in kB.
pss_kb() {
	total=0
	for pid in "$@"; do
		kb=$(awk '/^Pss:/ {print $2}' "/proc/$pid/smaps_rollup" 2>/dev/null)
		total=$((total + ${kb:-0}))
	done
	echo "$total"
}

# Whether the desktop of the person with user number 1000 is up: its
# compositor's socket, the bar and the dock. labwc calls its socket
# wayland-0, sengi-compositor counts from wayland-1.
desktop_is_up() {
	for socket in /run/user/1000/wayland-[0-9]; do
		if [ -S "$socket" ] && pidof sengi-bar >/dev/null && pidof sengi-dock >/dev/null; then
			return 0
		fi
	done
	return 1
}

# The compositor the desktop session is running on. Ours only ever runs
# for a session; labwc is also what the login screen is shown in.
session_compositor() {
	if pidof sengi-compositor >/dev/null; then
		echo sengi-compositor
	else
		echo labwc
	fi
}

# Runs a command as the person logged in on the live image, inside their
# desktop session as far as the environment goes: the session's socket,
# and the compositor's process number under the name each compositor
# gives it, which is how Settings knows whom to tell of a change.
as_live() {
	desktop_is_up || return 1
	compositor_pid=$(pgrep -u live -x "$(session_compositor | cut -c1-15)" | head -n 1)
	pid_name=LABWC_PID
	[ "$(session_compositor)" = labwc ] || pid_name=SENGI_COMPOSITOR_PID
	runuser -u live -- env HOME=/home/live XDG_RUNTIME_DIR=/run/user/1000 \
		WAYLAND_DISPLAY="${socket##*/}" "$pid_name=$compositor_pid" "$@"
}
