# Third-party notices

Sengi OS ships work by others under its own licences. This file lists what
is copied into this repository, with the licence and copyright line each
one asks to be kept. The licence text sits next to the files it covers.

## Phosphor Icons, Bold weight

- What: the icon files in `ui/icons/phosphor/bold/`, from
  https://github.com/phosphor-icons/core (`assets/bold`). The interface draws
  them as paths in token colours; `ui/icon-paths.slint` is generated from them
  by `scripts/render-icons.sh`.
- Licence: MIT. The full text is in `ui/icons/phosphor/LICENSE`.
- Copyright: Copyright (c) 2023 Phosphor Icons

## Slint

- What: the UI toolkit our programs link against, as a Cargo dependency. No
  Slint source is copied into this repository.
- Licence: used under the GNU GPL version 3 option of Slint's licences
  (https://slint.dev/terms-and-conditions). That grant is "only", not "or
  later", so a built Sengi OS program is distributed under GPL-3.0. Our own
  source files stay GPL-3.0-or-later.

## Flathub repository file

- What: `image/rootfs/etc/flatpak/remotes.d/flathub.flatpakrepo`, the file
  Flathub publishes at https://dl.flathub.org/repo/flathub.flatpakrepo,
  unchanged. It names the Flathub repository and holds its public signing
  key, so Flatpak can add Flathub as a source.
- Licence: none is stated by Flathub for the file. It is configuration, not
  a program, and carries no code of ours.

## Hardware firmware

- What: the image includes Debian's `firmware-iwlwifi`, `firmware-realtek`,
  `firmware-mediatek`, `firmware-atheros`, `firmware-brcm80211`,
  `firmware-intel-graphics`, `firmware-intel-sound` and `firmware-sof-signed`
  packages, and `intel-microcode`, from the `non-free-firmware` section. They
  are binary files that the kernel loads into Wi-Fi, Bluetooth, graphics and
  audio chips. Nothing of ours links them.
- Licence: each maker's own, which allows redistribution; most are not free
  software. The audio DSP firmware of `firmware-sof-signed` is built from the
  Sound Open Firmware project, whose source is mostly BSD-3-Clause, and is
  signed by Intel; Debian files it under `non-free-firmware` too. The texts are
  in the image under `/usr/share/doc/<package>/copyright`. Build the image
  without them with `SENGI_NONFREE_FIRMWARE=0 scripts/build-iso.sh`.
