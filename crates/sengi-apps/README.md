# sengi-apps

Installed applications for the Sengi OS shell: one place that reads desktop
entries, turns an Exec line into a command and starts it.

Used by sengi-launcher (search and start), sengi-dock (pins, running apps
and their icons) and sengi-bar (tray icons).

- `env`: the environment read once (`Env`), the application directories and
  program lookup in `PATH`.
- `desktop`: scanning the application directories and parsing desktop files.
  Earlier directories override later ones by desktop file ID. Files over
  1 MiB are ignored, a scan looks at no more than 10,000 directory entries,
  shown text has control and bidirectional characters removed and is cut at
  256 characters. `scan` returns what a menu shows; `scan_all` returns every
  installed application, for matching windows.
- `exec`: splitting and expanding an Exec line by the Desktop Entry rules.
  Nothing goes through a shell. Programs are started through `sengi-run`
  when it is installed, inside `foot -e` for `Terminal=true`, and let go of
  with a double fork and a session of their own, so the caller is never
  their parent.
- `index`: every installed application by desktop file ID (`AppIndex`).
- `matching`: the application a window's app_id belongs to.
- `files`: reading a file with a size limit.

Desktop actions, D-Bus activation and startup notification are not
supported, and no files or URLs are passed to the application.

## Icons

The `icons` feature adds `icons`: lookup in the `hicolor` theme and
`/usr/share/pixmaps`, and decoding of PNG files to the size they are drawn
at. The `svg` feature adds scalable icons through resvg.

Icon files are never decoded in the program that draws them. `Decoder`
starts that same program again as a child with an address-space and CPU
limit and a timeout, and the child finds the file, decodes it and writes the
pixels to its standard output. A program that uses it must hand its
arguments to `icons::run_decoder` first thing in `main`:

    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = sengi_apps::icons::run_decoder(&args) {
        return code;
    }

Files over 4 MiB (PNG) or 1 MiB (SVG), images over 2048 pixels a side and
requests over 256 pixels a side are refused. An SVG cannot pull in other
files. Paths that resolve into `/proc`, `/sys` or `/dev` are refused.

Without features the crate depends on nothing but libc, so a program that
only lists and starts applications links no image decoder.
