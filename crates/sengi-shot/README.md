# sengi-shot

Takes a screenshot and saves it as a PNG in `~/Pictures`, named
`Screenshot-YYYY-MM-DD-HHMMSS.png`. The compositor runs it for the Print key
(the whole screen) and for Alt+Print or Shift+Print (the window in front).
A notification says where the file is.

    sengi-shot                                        every screen
    sengi-shot --output eDP-1                         one screen
    sengi-shot --output eDP-1 --region 10,20,640,480  a part of it

`--region` is in the logical coordinates of the output, which is how the
compositor knows a window's frame. With several screens each gets its own
file, with the output's name added.

It asks the compositor through wlr-screencopy, so it works with any compositor
that has it, and it does not see the lock screen: the compositor refuses to copy
the screen while the session is locked. It runs for a moment and is not
running otherwise. The notification goes through `sengi-notify --post`, so this
program links no D-Bus stack.
