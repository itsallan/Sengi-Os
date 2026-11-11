# sengi-apps

Installed applications for the Sengi OS shell: one place that reads desktop
entries, turns an Exec line into a command and starts it.

Used by sengi-launcher (search and start) and sengi-dock (pins, running
apps and their icons).

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

The crate depends on nothing but libc in its default form, so a program that
only lists and starts applications stays small.
