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
