# Sourced by session-start, and by the boot test's report: sees to it that
# the sound devices came up whole.
#
# PipeWire 1.4 keeps what it has of a device's answer when the program that
# asked leaves before the answer is complete. A device asked by such a
# program just as it appears can be left with no profiles for as long as
# WirePlumber runs: it is listed, it has no output, and the machine is
# silent. The bar takes care not to ask at that moment, but any program
# may. Restarting WirePlumber announces the devices afresh.

# The numbers of the audio devices that have no profiles, or no current
# one, one to a line. A whole device always has both, "off" if nothing
# else. Listing the devices asks nothing of them.
sengi_sound_broken() {
	pw-cli ls Device 2>/dev/null |
		awk '$1 == "id" { id = $2; sub(/,.*/, "", id) }
			$1 == "media.class" && $3 == "\"Audio/Device\"" { print id }' |
		while read -r id; do
			for param in EnumProfile Profile; do
				# A failure says nothing about the device.
				answer=$(pw-cli enum-params "$id" "$param" </dev/null 2>/dev/null) || continue
				if [ -z "$answer" ]; then
					echo "$id"
					break
				fi
			done
		done
}

# Looks a few times while the session's sound comes up, and a while after
# for a device that is slow to appear, then ends. A device is taken for
# broken when two looks a second apart both say so, as one that is just
# going away answers nothing either. SENGI_SOUND_WAITS, the seconds before
# each look, is for the tests of this file.
sengi_sound_watch() {
	for wait in ${SENGI_SOUND_WAITS:-3 3 4 10 20}; do
		sleep "$wait"
		[ -n "$(sengi_sound_broken)" ] || continue
		sleep 1
		broken=$(sengi_sound_broken | tr '\n' ' ')
		[ -n "$broken" ] || continue
		echo "sound device ${broken% } came up without its profiles, restarting WirePlumber" |
			systemd-cat -t sengi-session -p warning
		systemctl --user try-restart wireplumber.service
	done
}
