# sengi-greeter

The Sengi OS login screen. It is a client of greetd: it shows who can log
in, takes the password, has greetd check it, and asks greetd to start the
session.

    sengi-greeter                      started by greetd, inside a compositor
    sengi-greeter --session /path      start this instead of sengi-session

It is running only while nobody is logged in. greetd starts it at boot,
unless a user is logged in automatically as on the live image, and again
each time a session ends.

Exit status: 0 greetd has granted a session and starts it now, 1 greetd or
the compositor could not be reached or went away, 64 a bad command line.

## What is on the screen

It is laid out as the lock screen is, from the same components and with
the same words, so that the two read as one system: the time, large, and
the date under it; below them the name of the user and a password field;
under the field one line for what there is to say.

- The field shows a dot for each character and never the characters.
  Return logs in. Escape empties the field. Backspace takes one character
  back.
- While greetd is checking, the field reads "Checking…" and takes no
  input. Keys pressed meanwhile are dropped.
- A wrong password empties the field, marks it, and says "Wrong
  password", or what a PAM module said about the refusal if one did.
- "Caps Lock is on" is shown under the field while it is, and with more
  than one keyboard layout the active one is named in the field.
- After 30 seconds with no key and no pointer movement
  (`timing.lock-prompt-hide`) the name and the field go and the clock is
  left. What was typed is wiped then, and a login that was waiting for an
  answer is given up. Any key or move of the pointer brings the field
  back.
- **Restart** and **Shut down** are in the bottom right corner and stay
  there when the field goes. Tab moves to them and Return presses the one
  that has the focus. Typing goes to the field wherever the focus is.

Every output shows the same screen.

### Who can log in

The accounts in the passwd database with a user number from 1000 to 59999
and a shell that is listed in `/etc/shells` and is not `nologin` or
`false`. The database is read through the C library, so accounts that do
not come from `/etc/passwd` are found as well.

- **One account**: its full name is shown, and the password is all that
  is asked for.
- **Several**: the name has a small button at each end, `‹` and `›`, that
  step to the account before and the one after, round and round. The
  arrow keys do the same. What was typed for one account is not kept for
  the next, and stepping does not buy fresh tries: wrong passwords are
  counted across all of them. Two accounts with one full name are shown
  with their login names after it.
- **None**: the field asks for "User name" first and shows what is typed.
  Return goes on to the password, with the name above the field; Escape
  on an empty password field goes back to the name.

Accounts outside that range, root among them, are not offered and cannot
be typed in when any account is offered. They can log in on a text
console.

### Wrong passwords

As on the lock screen and from the same code (`sengi_lock::backoff`):
from the third wrong password in a row the field stays shut for 2, 4, 8,
16 and then 30 seconds, never longer, and never for good. A login that
could not be checked at all costs no wait.

## Talking to greetd

The protocol is greetd-ipc(7): on the socket named by `GREETD_SOCK`, a
32-bit length in the machine's byte order and then that much JSON.

A login is one thread's work (`src/login.rs`), so a PAM module that takes
its time holds up nothing on the screen:

1. Return sends `create_session` with the login name.
2. greetd answers with what PAM asks:
   - a question to be kept secret: the first one is answered with what
     was typed, which on an ordinary account is the password. If nothing
     was typed, or it is a second such question, the field opens again
     with PAM's wording in place of "Password";
   - a question that may be seen: the field opens and shows what is typed;
   - a notice or a problem: kept, answered with nothing, and shown under
     the field with the next question or with the refusal.
3. `success` is followed by `start_session` with `/usr/bin/sengi-session`
   and no extra environment, the same as the live image's automatic login.
   When greetd has taken that, the program exits with status 0. greetd
   starts the session once the greeter's process has gone.
4. `error` with `auth_error` is a wrong password. The session is cancelled
   with `cancel_session`, and the next Return starts over at 1. Any other
   error is shown as greetd words it and is not counted as a wrong
   password.

Return on an empty field is sent too: an account with no password is let
in by PAM without a question.

If the connection breaks or greetd sends something that is not in the
manual, that login ends with "The password could not be checked" and the
socket is opened again for the next. If it cannot be opened, greetd is
gone: the program says so on standard error and exits with status 1, and
its compositor with it.

Requests are written out by hand and replies are read by a parser that
knows only what a reply can hold (`src/ipc.rs`). No JSON library is
linked, and sengi-greeter adds no crate to the workspace's dependencies.
It does not link libpam either: greetd does the PAM work. The image build
fails if either `zbus` or `libpam` turns up in the binary.

## The password

Handled as on the lock screen, and by the same code.

- It is collected from key presses by the program itself
  (`sengi_lock::entry::PasswordEntry`), in a buffer that is allocated
  once, kept out of swap, and overwritten when a character is taken back,
  when the field is emptied, when the field hides and when it is dropped.
  sengi-layer hands each typed character over before Slint sees it and
  overwrites the string it arrived in. The password is never in a Slint
  string.
- On Return the buffer is moved, not copied, to the thread that speaks to
  greetd. The one request that carries it is built in a buffer whose full
  size is set aside first, so it never moves, and that buffer is
  overwritten as soon as it has been written to the socket. The buffer
  the password was typed into is overwritten then too.
- It is never logged, never on a command line, never in the environment
  and never in a file. Core dumps are switched off first thing in `main`.
- A user name, and the answer to a question that PAM asks out loud, are
  not secrets. They are shown as typed and held in ordinary strings.

What is not wiped, because it is not ours: greetd's copy and PAM's, the
socket's buffers in the kernel, and the key codes in the Wayland
connection.

## The compositor it runs in

greetd runs `/usr/libexec/sengi-greeter-session` as its own user,
`_greetd`. That starts

    labwc -C /usr/share/sengi-os/greeter -S /usr/bin/sengi-greeter

- `-C` gives labwc a configuration directory of its own. It then reads
  `rc.xml`, `menu.xml`, `environment`, `autostart`, `shutdown` and
  `themerc-override` from there and from nowhere else: nothing from
  `/etc/xdg/labwc`, which is the desktop's, and nothing from a home
  directory. Checked both ways: strace shows no other configuration
  being opened, and bindings planted in both places did not fire.
- The directory's `rc.xml` has one key binding and one mouse binding, and
  both do nothing. They are there because labwc loads its built-in
  bindings when a configuration has none, and those start a terminal and
  a launcher. There is no `autostart`, and the menu is empty.
- `-S` ends labwc when sengi-greeter exits, whatever the reason. That is
  what lets greetd start the user's session after a login, and what makes
  greetd start a new login screen if this one dies.
- sengi-greeter is a layer surface on the overlay layer of every output,
  with the keyboard to itself.

labwc switches to another virtual terminal on Ctrl+Alt+F1 to F12 by
itself; that is not a binding and is not taken out. The other terminals
ask for a login of their own.

greetd's user gets no sound server: the image tells PipeWire and
WirePlumber not to start for `_greetd`.

## Restart and Shut down

The buttons run `systemctl --no-ask-password reboot` and `poweroff`, which
ask logind. logind asks polkit, whose stock policy allows both to the
active session on the seat, and the login screen's session is that. The
image has `polkitd` for this; it is started by the first such question
and not at boot. sengi-greeter has no privilege of its own and there is no
setuid helper.

If logind refuses, or `systemctl` cannot be run, the line under the field
says "The computer could not be restarted" or "… shut down".

Nothing asks "are you sure". Nobody is logged in on this seat, so there is
nothing open to lose.

## What this does not do

- It does not offer a choice of session: there is one.
- It follows the system's keyboard layout, not the one a user chose in
  Settings, which is kept per user. A password with characters that are
  somewhere else on the system's layout has to be typed for that layout.
- It does not switch the screen off when left alone.

## Cost

Measured in the builder VM under headless labwc 0.8.3, release build, one
1280 x 720 output:

- Left alone, with the field put away, it wakes once a minute to draw the
  new time: 9 context switches in three minutes for all its threads, the
  same as the lock screen. Its labwc: 6.
- Memory: sengi-greeter 10.3 MB proportional, its labwc 28 MB. Both are
  gone once someone has logged in. The boot test records the same sum on
  the image.

## Testing

    cargo test -p sengi-greeter

covers the protocol both ways, logins against a scripted greetd on a
socket pair (every kind of message, refusals, a greetd that hangs up or
talks nonsense), the list of accounts, the screen's states, and the power
commands. The image's boot test (`scripts/boot-test.sh`) logs out of the
live session, requires the login screen, types a wrong password and then
the right one, and fails unless the desktop is back for `live` with
nothing of the login screen left running.

To try it by hand it needs a greetd: see greetd(5), with
`command = "/usr/libexec/sengi-greeter-session"` under `[default_session]`.

## Building

Needs `libxkbcommon-dev` and `libfontconfig-dev` at build time,
`libxkbcommon0` and `libfontconfig1` at run time.
