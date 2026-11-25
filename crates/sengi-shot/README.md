# sengi-shot

Takes a screenshot and saves it as a PNG in `~/Pictures`, named
`Screenshot-YYYY-MM-DD-HHMMSS.png`. The compositor runs it for the Print key
(the whole screen) and for Alt+Print or Shift+Print (the window in front).
A notification says where the file is.

    sengi-shot                                        every screen
    sengi-shot --output eDP-1                         one screen
    sengi-shot --output eDP-1 --region 10,20,640,480  a part of it
    sengi-shot --select                               drag a rectangle to keep
    sengi-shot --delay 5                              count down 5 seconds first
    sengi-shot --clipboard                            also copy the picture

`--region` is in the logical coordinates of the output, which is how the
compositor knows a window's frame. With several screens each gets its own
file, with the output's name added.

It asks the compositor through wlr-screencopy, so it works with any compositor
that has it, and it does not see the lock screen: the compositor refuses to copy
the screen while the session is locked. It runs for a moment and is not
running otherwise. The notification goes through `sengi-notify --post`, so this
program links no D-Bus stack.

## Select, delay and clipboard

- `--select` runs `sengi-snip --select`: the screen dims and you drag a
  rectangle on the output the pointer is on. Enter or letting go takes it,
  Escape cancels (nothing is saved and nothing is said). The overlay is given
  400 ms to leave before the picture is taken, as the quick settings panel
  is. It cannot be combined with `--region` or `--output`.
- `--delay N` (1 to 60) runs `sengi-snip --countdown N`, a small card with the
  seconds left, which is gone before the shot. A click on it cancels. Without
  sengi-snip it waits quietly. With `--select` the choosing comes first, then
  the countdown.
- The time limit of 15 seconds applies to the copy from the compositor, not
  to choosing or counting.
- `--clipboard` copies the first picture with `wl-copy --type image/png`. The
  compositor (smithay's data device only) offers no wlr-data-control or
  ext-data-control, which is what a program needs to set the clipboard
  without a window and a keyboard focus of its own, and wl-clipboard is not in
  the image. So with wl-copy installed it copies, and without it the
  notification says "Not copied to the clipboard: wl-copy is not installed"
  and the file is saved all the same.
- The notification gives the path. `sengi-notify --post` has no actions, so
  there is no Open button.

Quick settings starts these three as Screen, Area and In 5 s.
