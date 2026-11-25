# sengi-snip

The two small screens `sengi-shot` asks for. They are a separate program so
that a plain screenshot (the Print key) links no toolkit. You do not normally
run it yourself.

    sengi-snip --select           dim the screen and drag a rectangle
    sengi-snip --countdown 5      count down from 5 in a corner

## --select

A layer-shell overlay surface covering the output the compositor puts it on
(the one with the pointer), with the keyboard. The screen is dimmed with the
scrim colour of the tokens. Drag a rectangle: everything outside it stays
dimmed, the rectangle is outlined in the accent colour and a label says its
size (`640 x 480`). Letting go or Enter takes it, Escape or a right click
cancels, and so does the keyboard going elsewhere. A drag shorter than 4
pixels on a side is taken for a click and ignored.

It prints one line, `NAME X,Y,WIDTH,HEIGHT`, in the logical pixels of the
output named `NAME` (`-` if the compositor gives no name), and exits 0. On
cancel it prints nothing and exits 1. `sengi-shot --select` reads that line.

The pointer is the ordinary arrow: sengi-layer always sets the default cursor,
so there is no crosshair yet.

## --countdown N

A small card under the bar with the seconds left, 1 to 60. It takes no
keyboard and covers only itself, so what is being photographed stays usable.
Exits 0 at zero and 1 if clicked.

## Code

- `src/select.rs`: the maths: two corners to a clamped, rounded rectangle,
  the size label, the line printed, seconds left.
- `src/cli.rs`: the command line.
- `ui/snip.slint`: both screens, from the shared components and tokens.

    cargo test -p sengi-snip

`tests/window.rs` drives the selection screen with the pointer and keys
without a display and writes PNGs when `SENGI_RENDER_DIR` is set.
