# sengi-brightness

Run by the compositor for the brightness keys of a laptop:

    sengi-brightness up | down | kbd-up | kbd-down | kbd-toggle

It reads the panel from `/sys/class/backlight` (or the keyboard light from
`/sys/class/leds`, the one whose name has `kbd_backlight` in it, which on a
ZenBook is `asus::kbd_backlight`), works out the next step, has logind set
it, and starts `sengi-osd` to show the result. It exits at once; nothing
stays running.

- The screen moves in 5 percent steps, by at least one device step on a
  panel with few of them, and never below 5 percent: a black screen is a
  trap. This is the same floor the quick settings slider keeps.
- The keyboard light moves one level per press and may go all the way
  off. The toggle key switches it off, or back on to half of its range.
- logind's `SetBrightness` is the one route open to an ordinary user, so
  no root and no polkit rule are needed. That is why this is a program of
  its own: the compositor must not link a D-Bus stack.
