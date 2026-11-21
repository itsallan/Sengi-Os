# sengi-text

Making text from outside safe to show: network and device names,
notification text, tray titles and menu labels, names in desktop files.

Used by sengi-apps (and through it sengi-launcher and sengi-dock),
sengi-quick, sengi-notify and sengi-bar. It has no dependencies.

- `invisible(c)` and `hidden(c)`: the one list of characters that are not
  shown on their own. `hidden` is controls plus `invisible`: bidirectional
  marks, embeddings, overrides and isolates, zero-width characters and
  joiners, blank fillers, variation selectors, tag characters.
- `keep(previous, c, next)` and `shown(text)`: the same list with its three
  exceptions. The zero-width non-joiner and joiner stay between two letters
  or combining marks (Persian and the Indic scripts need them) and the
  joiner between two emoji; the emoji variation selector stays after a
  visible character. At the edges, in runs and next to spaces they go.
- `line(raw, max_chars)`: one line. White space is folded, hidden
  characters go, a cut ends in an ellipsis.
- `tidy(text, max_chars, multiline)`: for message text. Spaces are kept as
  written, line breaks too if `multiline`, with at most one blank line in
  a row.
- `isolate(name)`: wraps a cleaned name in directional isolates for use
  inside a sentence.
- `initial(name)`: the letter for a fallback icon tile.

Add a character to the list here and every program stops showing it.
