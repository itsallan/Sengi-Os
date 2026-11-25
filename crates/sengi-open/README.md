# sengi-open

The Sengi OS opener: given a file, a folder or a `file://` address, it starts
the program that opens it and exits.

    sengi-open FILE_OR_URI...

It decides from the name and the first 8 KiB of the file, in this order:

| What | Opens in |
| --- | --- |
| a folder | `sengi-files` |
| a PNG or JPEG by its first bytes, or named `.png`, `.jpg`, `.jpeg` | `sengi-view` |
| a name from the list of text kinds (`.txt`, `.md`, `.log`, `.rs`, `.toml`, `.json`, ...) | `sengi-edit` |
| a name from the list of kinds that are never text (`.pdf`, `.zip`, `.mp3`, `.webp`, ...) | nothing |
| anything else whose first bytes are UTF-8 with no NUL (an empty file counts) | `sengi-edit` |
| anything else | nothing |

When nothing can open it, or the program for it is not installed, the file does
not exist, or the address is not a file (`https:`, `mailto:`, ...), it posts a
notification with `sengi-notify --post` ("Nothing can open this file", with the
name under it) and exits with status 1. The same text goes to standard error.
Nothing fails silently. Programs are started in their own scope with
`sengi-run` when that is there, like every app started from the desktop.

Files and the desktop use it when `xdg-open` is not installed, and the
association files do the same for programs that go through the desktop:
`image/rootfs/usr/share/applications/mimeapps.list` makes `sengi-edit.desktop`
the default for text types, `sengi-view.desktop` for `image/png` and
`image/jpeg` and `sengi-files.desktop` for folders; the `MimeType=` lines of
those `.desktop` files say the same. Flatpak apps and the Store reach them
through the portal.

## No `xdg-open` shim

The image does not put a `/usr/bin/xdg-open` of its own in place. That path
belongs to the `xdg-utils` package, which apt can install (Flatpak apps and
others ask for it), and a file of ours there would collide with it. Our
programs call `sengi-open` by name instead.

## Files

- `src/lib.rs`: the decision table, as a function with its tests.
- `src/main.rs`: reading the disk, starting the program, the notice.
