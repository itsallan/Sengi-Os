# sengi-calendar

The popup that opens when you click the clock in the bar: a month of days with
today marked, arrows for the month before and after, and a Today button. It
drops down under the right end of the bar like the quick settings panel.

    sengi-calendar

It is a program of its own, not part of sengi-bar, so the bar gained one
line to start it and keeps no calendar code or memory. It is not a service: it
starts on the click, draws once, wakes for input only, and exits when
dismissed: Escape, a click outside it, or the keyboard going elsewhere (a
click on the clock while it is open lands on it and closes it).

## Keys

Left and Right, or Page Up and Page Down, change the month. Home or T
returns to today. Escape closes.

## Dates

The date maths is in `src/cal.rs`, by hand: leap years (every 4th year,
except centuries not divisible by 400), month lengths, the weekday of any day
(days since 1970-01-01, a Thursday) and the grid of a month as six rows of
seven, with the end of the month before and the start of the next in grey.
Six rows always, so the popup keeps its height. Today comes from sengi-clock,
so it follows the time zone chosen in Settings.

## Setting

`appearance.week-start` in `~/.config/sengi-os/settings.toml`: `"monday"`
(the default), `"sunday"` or `"saturday"`. Settings has no control for it yet.

## Tests

    cargo test -p sengi-calendar

The month tables for 2024-02 (a leap February), 2100-02 (not a leap year, so
exactly four rows) and 2026-11 (starts on a Sunday) are in `src/cal.rs`.
`tests/window.rs` draws the real popup without a display and writes PNGs when
`SENGI_RENDER_DIR` is set.
