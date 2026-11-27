# sengi-osd

The level indicator: a small rounded panel near the bottom centre of the
screen with an icon and a bar, shown for about a second after a volume,
mute, brightness or keyboard light key.

    sengi-osd volume                     show the volume of the default output
    sengi-osd show brightness 55         show a level: volume, brightness or keyboard
    sengi-osd show volume 40 muted       a muted sound shows an empty bar

The compositor runs it after the key's change (sengi-brightness for the
brightness keys, `wpctl` for the volume keys), so what is shown is what the
key did. On a machine with no sound device a volume key shows the crossed
speaker and an empty bar.

It is not running at boot. The first run opens the panel; a run while it is
open hands its level over a datagram socket in `$XDG_RUNTIME_DIR`
(`sengi-osd.sock`, under a lock file) and exits, so a held key moves one bar
and never stacks panels. The panel ends 1.2 seconds after the last level
(`timing.osd-timeout`).

The surface is on the overlay layer and takes neither keyboard nor pointer.
Colours, size and spacing come from the tokens (`[osd]` in
tokens/sengi.toml); the icons are Phosphor Bold, drawn as paths, so the
program links no image decoder and no D-Bus stack. image/build.sh checks
that on every build. `cargo test -p sengi-osd` draws the panel with the
software renderer; with `SENGI_RENDER_DIR` set it writes the pictures there.
