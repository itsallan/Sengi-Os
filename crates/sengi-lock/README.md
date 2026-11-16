# sengi-lock

The Sengi OS lock screen. It locks the session through the compositor's
ext-session-lock-v1 protocol and unlocks it when PAM says that the
password typed is that of the user the session belongs to.

    sengi-lock        lock, and return when the screen has been unlocked
    sengi-lock -f     lock, and return as soon as the screen is locked

It is not running until the screen locks. The idle timer (`swayidle`, see
`image/rootfs/etc/xdg/labwc/autostart.in`), Super+L, the session menu and
the Lock button in quick settings all start `sengi-lock -f`.

Exit status: 0 unlocked (with `-f`: locked), 2 the compositor refused the
lock, which is what happens when the session is already locked, 3 there is
no compositor or it cannot lock, 1 anything else, 64 a bad command line.

## What is on the screen

The time, large, and the date under it. Below them the name of the user
and a password field. Nothing else: no buttons, no notifications, no media
controls, nothing to click.

- The field shows a dot for each character and never the characters.
- Return checks the password. Escape empties the field. Backspace takes
  one character back.
- While PAM is checking, the field reads "Checking…" and takes no input.
  Keys pressed meanwhile are dropped, a second Return included.
- A wrong password empties the field, marks it, and says "Wrong password".
- "Caps Lock is on" is shown under the field while it is.
- With more than one keyboard layout set in Settings, the active one is
  named at the right end of the field.
- After 30 seconds with no key and no pointer movement
  (`timing.lock-prompt-hide` in the tokens) the name and the field go and
  the clock is left alone. What was typed and not sent is wiped then. Any
  key or any move of the pointer brings the field back, and a key that
  types a character is also the first character of the password.
- Switching the screen off stays `swayidle`'s job.

The clock follows `clock.format` and `clock.time-zone` from Settings, like
the bar's.

Every output shows the same screen, kept in step. Whichever output the
compositor gives the keyboard to, the password goes to the one prompt.

## Wrong passwords

PAM waits about two seconds before it says no. On top of that, from the
third wrong password in a row the prompt stays shut for 2, 4, 8, 16 and
then 30 seconds each time, and says for how long. It never goes past 30
seconds and never becomes a lock-out: the right password works as soon as
the wait is over, however many wrong ones came before. A check that could
not be made at all costs no wait.

## How it locks

The order matters, and it is the protocol's:

1. Connect, ask for the lock, and at once create a lock surface for every
   output. Lock surfaces are shown by the compositor only on a locked
   session, so if the lock is refused nothing of this program is ever on
   screen.
2. Draw each surface as soon as the compositor has given it a size.
3. When the compositor has said `locked`, every output has had its first
   frame, and a round trip has shown that the compositor took those frames
   in, the session is locked and covered. Only then does `sengi-lock -f`
   return. labwc says `locked` straight away and blanks the outputs a
   tenth of a second later, so `locked` on its own is not yet "nothing can
   be seen".
4. `unlock_and_destroy` is sent in exactly one place: after PAM has granted
   a password that was submitted from the prompt. The program then waits
   for the compositor to have seen it before it exits.

If the compositor answers `finished` instead of `locked`, there is another
locker or the compositor will not lock: exit 2, nothing shown.

There is no "unlock on error". A panic aborts (the release profile), a
lost connection ends the program, a signal kills it, and in each case the
compositor keeps the session locked, because that is what the protocol
promises when a locker goes away.

### Why `-f` is what swayidle needs

`swayidle -w` waits for each command it runs before it carries on, and
before sleep it holds off the suspend until the command has returned. With
`-f` the process that was started forks and returns only when the worker
has reported step 3 through a pipe. So the machine never goes to sleep
with the desktop still on screen, to show it for a moment on waking. If
the lock fails, the same pipe closes without the report and `-f` returns
the failure.

### A second sengi-lock

Is turned away by the compositor, which allows one lock at a time: it gets
`finished`, exits with status 2 and prints nothing. No lock file and no
socket of our own: the compositor is the only one that knows for certain
whether the session is locked, including by some other locker.

## When the locker dies

Under labwc 0.8.3 (`src/session-lock.c`), a locker that goes away without
unlocking leaves the session locked: every output is black, input goes
nowhere, and labwc waits for a new client to ask for the lock. It lets
that client "replace the abandoned lock", and the new client can unlock.
Nothing else can. A dead locker with no successor is a black screen until
the session is ended from outside.

So sengi-lock is two processes:

- The **supervisor** is the process that was started (with `-f`, its
  child). It never connects to the compositor and holds no password. It
  forks the worker and sleeps in `waitpid`.
- The **worker** does everything else.

When the worker exits, the supervisor looks at how:

| The worker | The supervisor |
| --- | --- |
| exited 0: unlocked | exits 0 |
| exited 2: the compositor refused | exits 2. Someone else holds the lock |
| exited 3: no compositor | exits 3. Nothing is left to lock |
| was killed, aborted, or failed, after the session was locked | starts a new worker, always |
| the same, before any worker reported the session locked | tries three times, then gives up |

A new worker takes over the abandoned lock and asks for the password
again. Between the two the screen is black: 0.25 s after the first death,
doubling up to 5 s if workers keep dying quickly. What had been typed is
gone with the old worker.

SIGTERM, SIGINT and SIGHUP are not caught by either process. They kill the
one they are sent to. The worker comes back; a supervisor that is killed
leaves a worker that still works but will not be replaced.

If both are killed (`pkill sengi-lock` does that) the screen stays black.
The ways back are a new `sengi-lock`: the idle timer starts one after the
lock time has passed without input (five minutes unless changed, and not
at all if locking when idle is switched off in Settings), and it can be
started from a console or over ssh with `WAYLAND_DISPLAY` and
`XDG_RUNTIME_DIR` set. Or the session can be ended with
`loginctl terminate-session`.

## The password

- It is collected from key presses by the program itself
  (`entry::PasswordEntry`). There is no text input widget behind the
  field, because a toolkit's text field keeps copies that cannot be wiped.
  sengi-layer hands each typed character to the lock screen before Slint
  sees it (`Handle::on_text`), the lock screen appends it to one buffer,
  and sengi-layer overwrites the string the character arrived in.
- The buffer (`sengi_system::secret::Secret`) is allocated once at its
  full size, 511 bytes, so it never moves; it is `mlock`ed so it is not
  written to swap; and it is overwritten when a character is taken back,
  when the field is emptied, when the prompt hides, and when it is
  dropped.
- At 511 bytes no more is taken: the key press is dropped whole, the field
  says "That is as long as a password can be", and Backspace and Return
  work as before. PAM's own limit is 512 bytes with the terminator.
- On Return the buffer is moved, not copied, to the PAM thread, which
  wipes it when PAM has answered.
- It is never logged, never on a command line, never in the environment
  and never in a file. The program takes no password, user name or
  command as an argument.
- Core dumps are switched off first thing in `main`
  (`PR_SET_DUMPABLE` 0), which also closes `/proc/PID/mem` and `ptrace`
  to other processes of the same user. The supervisor and the worker
  inherit it.

What is not wiped, because it is not ours:

- PAM gets a copy of the password, as its interface requires. It is PAM's
  to free, and `pam_unix` hands it on to its helper `unix_chkpwd` through
  a pipe.
- The key codes, not characters, pass through the Wayland connection's
  read buffer and xkbcommon's state.

## PAM

The service is `sengi-lock`, configured in `/etc/pam.d/sengi-lock`:
`common-auth` and `common-account`, as for any other login on the system.
sengi-lock refuses to lock if that file is missing, since a screen locked
without it could never be unlocked.

- The user is the one the process runs as, looked up by number with
  `getpwuid_r`. No name is ever typed or taken from the environment.
- Each Return is one PAM transaction on a thread of its own:
  `pam_start`, `pam_authenticate`, `pam_acct_mgmt`,
  `pam_setcred(PAM_REFRESH_CRED)` on success, `pam_end` always. The UI
  thread never calls PAM, so a module that blocks for a minute leaves the
  clock running and the screen locked, with "Checking…" in the field.
  There is no way to cancel a check: PAM has none.
- The conversation answers a prompt that does not echo with the password,
  a prompt that echoes with an empty string, and keeps the last message
  or error text a module sends, to show if the check fails for a reason
  other than a wrong password.
- A password that is right but has expired (`PAM_NEW_AUTHTOK_REQD`)
  unlocks. It cannot be changed from here, and the session it would shut
  the owner out of is already open. An account that is expired or barred
  does not unlock.
- libpam is declared by hand in `src/pam.rs`: six functions and two
  structs, no binding crate. sengi-lock adds no crate to the workspace's
  dependencies.

## What this protects, in plain words

Someone who walks up to the machine while it is locked cannot see or
touch the session without the password. They see the clock, the date and
the user's name. They can guess passwords, slowly.

It does not protect against:

- **root**, or anyone who can become root. Root can read any process's
  memory, replace this program, or unlock by other means. Nothing a user
  program does changes that.
- **Another program of the same user that is already running.** It could
  have been logging keys before the screen locked, can kill the locker
  (the screen stays locked, but see above), and can connect to the
  compositor. `ptrace` and `/proc/PID/mem` are closed to it by the
  dumpable flag, but a program running as the user is inside the thing
  being protected, not outside it.
- **The compositor.** It sees every key. The lock is as good as the
  compositor's implementation of the protocol.
- **Someone with the hardware**: booting another system, taking the disk,
  cold-boot attacks on memory. That is what disk encryption is for.
- **Switching to another virtual terminal**, where a login prompt or an
  open console may be. The image masks the spare getty on tty1.
- **A password that is easy to guess.** Thirty seconds between tries
  slows a person down. It is not rate limiting against a machine.

## Cost

Measured in the builder VM under headless labwc 0.8.3, release build, two
1280 x 720 outputs:

- Not running until the screen locks, and gone when it unlocks: nothing
  at idle.
- From exec to locked and covered: 34 to 58 ms over five runs.
- While locked and left alone it wakes once a minute to draw the new
  time: 9 context switches in three minutes for the worker and all its
  threads, none for the supervisor. It also wakes when the wall clock is
  set, which is how the clock is right the moment the machine wakes from
  suspend.
- Memory while locked: the worker 23 MB resident, 13.6 MB proportional,
  most of it the frame buffers of the two outputs; the supervisor 1.3 MB
  resident, 0.4 MB proportional.

## Reused by the login screen

- `ui/components.slint`: `SengiPasswordField` and `SengiClockBlock`, with
  their sizes in `tokens/sengi.toml`.
- `sengi-clock`: the time and date strings that follow Settings, and
  `every_minute`.
- This crate as a library with `default-features = false`, which leaves
  PAM out: `entry` (the password as typed), `backoff`, `user`.
- `sengi-layer`: `Handle::on_text` and `Handle::on_keyboard_changed`.
- `sengi_system::secret::Secret`.

## Testing

    cargo test -p sengi-lock

covers the states of the screen, the entry and its limit, the waits, the
PAM conversation, and the supervisor's decisions. The image's boot test
(`scripts/boot-test.sh`) locks the live session, types a wrong password
and then the right one, and fails unless the screen stays locked after the
first and the desktop is back after the second.

To try it by hand in a session: `sengi-lock`. The PAM service file has to
be installed.

## Building

Needs `libpam0g-dev` and `libxkbcommon-dev` at build time, `libpam0g`,
`libpam-modules` and `libxkbcommon0` at run time.
