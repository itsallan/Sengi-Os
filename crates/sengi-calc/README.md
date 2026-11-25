# sengi-calc

The Sengi OS calculator: a small window with a display and keys. It is not a
service: it starts when asked for (Name=Calculator in the launcher) and is
gone when its window is closed.

    sengi-calc

## How it works

You type an expression, key by key, and `=` or Return works it out. The answer
then stands in place of the expression, so the next operator carries on from
it and a digit starts a new sum. The answer so far shows small under the
display while you type. The last five sums are listed above the display; a
click brings an answer back.

- Arithmetic is decimal, done by hand in `src/eval.rs`: a number is a whole
  mantissa and a power of ten, so 0.1 + 0.2 is exactly 0.3. No floating
  point, no `eval`, no dependency beyond the toolkit.
- Precedence is the usual one: `*` and `/` before `+` and `-`, left to right,
  brackets, a sign in front of a number or a bracket.
- `x%` is x / 100. Right after a plus or a minus it is a share of the left
  side, as on a shop calculator: 200 + 10% is 220, 200 - 10% is 180, and
  200 * 10% is 20.
- A quotient is kept to 30 digits and shown to 12 (`1/3` is 0.333333333333,
  `1/3*3` is 1). A number of up to 15 digits is shown as it is. Results
  below 0.000001 or of more than 15 digits before the point are written like
  `1e-7`. Beyond 10^300 it says "Number too large".
- Division by zero says "Cannot divide by zero" in place of the preview and
  keeps the expression so it can be mended. So does anything that is not an
  expression ("Check the expression"). Brackets left open are closed.
- The decimal separator is always a dot, whatever the locale. A comma on the
  keyboard types a dot.

## Keys

Digits, `.`, `+ - * /` (also `x`), `( ) %`, Return or `=` for the answer,
Backspace, Escape or Delete to clear, Ctrl+C to copy the answer (or what is
worked out so far, or what is typed), Ctrl+W or Ctrl+Q to close.

Ctrl+C and the Copy key select a hidden, read-only text field that holds the
text and use the toolkit's own copy, so what is copied goes where the
toolkit's clipboard goes.

## Files

- `src/eval.rs`: the numbers and the parser, with the table of expressions.
- `src/calc.rs`: what each key does to the typed expression, and the history.
- `src/app.rs`: the glue between those and the window.
- `ui/calc.slint`: the window, from the shared components and tokens
  (`calc-window-*` and `calc-display-height` in `tokens/sengi.toml`).
- `sengi-calc.desktop`; the icon is `sengi-calc.svg.in` in the image overlay.

## Tests

    cargo test -p sengi-calc

`tests/window.rs` drives the real window with keys without a display and
writes PNGs when `SENGI_RENDER_DIR` is set.
