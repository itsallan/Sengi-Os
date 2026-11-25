# sengi-view

The Sengi OS image viewer (the app is called Image Viewer): one PNG or JPEG in
a window, fitted to it, with the other pictures of the folder a key away.

    sengi-view FILE          open FILE (a path or a file:// address)
    sengi-view               an empty window that says to open one from Files

It is not a service. It starts when asked for, exits when its window is closed
and leaves nothing running. The window's application id is `sengi-view`, which
is also the name of the `.desktop` file here. That file has `NoDisplay=true`:
a viewer with nothing to show is no use in the launcher, so the picture is
opened from Files or the desktop (through `sengi-open`) and the entry only
serves the dock, the window match and the file associations (`image/png`,
`image/jpeg`).

## Keys and mouse

- `+` or `=` zoom in, `-` zoom out (steps of 25%, from 2% to 3200%), `0` the
  size of the picture (100%), `9` or `F` fit to the window. A picture is
  fitted when it opens, and never enlarged beyond its own size by that. The
  buttons at the top do the same.
- Drag with the pointer to move a picture that is bigger than the window.
- Left and Right show the previous and next picture in the same folder,
  round the ends, in name order without regard to case and with numbers by
  value (`img2` before `img10`). Only `.png`, `.jpg` and `.jpeg` names count.
- Ctrl+W or Ctrl+Q closes the window.

The line at the bottom has the size in pixels and the size of the file, the
place in the folder ("3 of 12") and the zoom.

## Decoding

Done in this program, with the `png` crate and `zune-jpeg` (pure Rust, both
MIT or Apache-2.0), on a thread, so the window stays alive while a big picture
loads; a stale result (the next picture was asked for meanwhile) is dropped.

- The kind is told by the first bytes, not the name.
- The size is read first. A picture over 100 megapixels is refused with a
  message before any pixels are decoded.
- Memory is bounded: the copy that is drawn has its longer side at most 3072
  pixels, and a PNG is shrunk row by row as it is read, so it is never held
  whole (an interlaced PNG is, and so is the decoded output of a JPEG, as
  3 bytes a pixel, for as long as it takes to shrink it). At 100% a picture
  larger than 3072 pixels is drawn from the shrunk copy.
- A damaged file, or a kind of JPEG the decoder cannot read, is named in the
  window with the reason; nothing fails silently.

Not done: EXIF orientation, colour profiles, animation, 16-bit depth beyond
dropping it to 8, CMYK JPEGs.

## Files

- `src/decode.rs`: the two decoders and the limits. `src/scale.rs`: shrinking a
  row at a time. `src/folder.rs`: the pictures beside this one, in order.
  `src/app.rs`: the window, the thread, stepping.
- `ui/view.slint`: the window.
- `tests/window.rs`: the real window driven without a display; with
  `SENGI_RENDER_DIR` set it writes `view-*.png`. `tests/fixtures/` holds two
  small JPEG files (a colour test card, and a grey gradient) made with
  ImageMagick.
