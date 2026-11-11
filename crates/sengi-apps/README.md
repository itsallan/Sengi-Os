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
  256 characters.
- `exec`: splitting and expanding an Exec line by the Desktop Entry rules.
  Nothing goes through a shell. Programs are started through `sengi-run`
  when it is installed, inside `foot -e` for `Terminal=true`.
- `files`: reading a file with a size limit.

Desktop actions, D-Bus activation and startup notification are not
supported, and no files or URLs are passed to the application.

The crate has no dependencies in its default form, so a program that only
lists and starts applications stays small.
