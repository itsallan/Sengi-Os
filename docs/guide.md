# Using Sengi OS

## Installing

Start the computer from the USB stick. Nothing is installed yet: the desktop
you see runs from the stick, and changes are lost at shutdown. Sengi OS says so
and asks whether to **Install Sengi OS** or **Try it first** (Escape does the
same). To install later, open **Install Sengi OS** from the dock (or search for
"install"). The installer asks, in this order:

1. The keyboard layout and the time zone. The layout takes effect at once.
2. The disk.
3. Your name, a user name, a password and a name for the computer.
4. To look over all of it. Nothing on the computer has been changed until
   you press "Erase disk and install" (or "Install Sengi OS", beside
   another system, or "Shrink Windows and install") there and answer the
   question that follows.

Then it installs, which takes a few minutes, and offers to restart. Take the
stick out when the screen goes dark, or the computer may start from the stick
again. Log in with the password you chose.

**Erase the whole disk, or install beside.** On the disk page you say which.
"Erase the whole disk" is described next. "Install beside the existing
system" is offered when the computer started with UEFI and the disk has a
GPT, an EFI system partition and at least 10 GB of free (unallocated) space:
it adds one partition in that free space, adds a Sengi OS folder
(`EFI/sengios`) to the EFI system partition, and puts a Sengi OS entry first in
the firmware's boot list, in front of the other system's. Your other files
are not touched: nothing is shrunk, moved, formatted or overwritten, and the
other system's own files on the EFI system partition, and its entry in the
firmware's list, stay as they are. (The partition table itself is
rewritten to add the partition, and the firmware's list changes, which is why
BitLocker or a TPM may ask the other system for a recovery key once: have it
at hand.) It is not offered while Secure Boot is on, because the boot loader
is not signed: turn Secure Boot off in the firmware settings first. Sengi OS's boot menu then lists Windows or
whatever else GRUB's os-prober finds, and is shown for three seconds when
there is something to choose from; with nothing else on the disk it stays
hidden, as after a whole-disk install. When the option is not offered the
row says why; to make room, shrink the other system first, from inside it,
or, on a disk where Windows has all the room, let the installer do it
(below).
Back up what you want to keep anyway: this part is new, and it has been
tried on virtual disks only. If an installation fails after the partition
was made, the installer says what exists and how to remove the partition
(`sudo sfdisk --delete DISK NUMBER` from the live session, after looking at
`lsblk`); it does not delete partitions itself.

### Beside Windows, when Windows has the whole disk

**This changes your Windows partition. Back up your files first, somewhere
that is not this computer.** Making a file system smaller moves data. If the
power fails, the computer is switched off or the USB stick is pulled out
while that happens, Windows and everything on it can be lost. **It has not
yet been tried on a real Windows installation**, only on virtual disks with
volumes made by Linux's own NTFS tools. If you are not prepared to restore
that computer from your backup, make room from inside Windows instead (Disk
Management, right-click the C: drive, Shrink Volume) and then choose
"Install beside the existing system".

It is offered when the computer started with UEFI, the disk has a GPT and an
EFI system partition, there is no free space of 10 GB or more, and the disk's
largest Windows data partition holds NTFS. That partition is usually C:. If it
is not, for instance a larger data drive, the installer names it by its label
and size, and that one is made smaller: read the name before you go on.

**In Windows, before you start:**

1. Back up your files.
2. If a drive of this disk is encrypted (BitLocker, or "device encryption" in
   Settings), save the recovery key, turn it off and wait until decryption has
   finished.
3. Turn off Fast Startup: Control Panel, Power Options, "Choose what the power
   buttons do", "Change settings that are currently unavailable", and untick
   "Turn on fast startup". Do not hibernate.
4. If Windows asks for a disk check, or you suspect one is needed, run
   `chkdsk C: /f` as administrator and restart.
5. Shut down with Shut down, not Restart, Sleep or Hibernate.

**Then, on the live stick, with the charger plugged in:** on the disk page,
choose the disk and then the second choice, "Make room and install beside" (on
a disk that holds Windows or BitLocker nothing is chosen for you). The
installer looks at the disk (this writes nothing; after ten minutes it gives
up and says so), names the partition and shows how much of it is used. Choose
how much Sengi OS gets. Windows always keeps what it uses plus the larger of
15 GB or 20% of that, and Sengi OS gets at least 11 GB. Tick "I have a
backup". The review page lists the exact change, and the question after it
starts it.

**What happens.** The installer looks again, then `ntfsresize` makes the file
system fit its new size and the result is checked, then only the size of that
one partition is changed (its start, type and identifier stay, and no other
partition is touched), and Sengi OS is installed in the space that was given
up as described above. Nothing is forced, and the mark that tells Windows to
check its disk is left in place. **The next time Windows starts, it checks the
disk before anything else. That is expected: let it finish, and do not switch
the computer off.** Windows may restart once after it. While the partition is
being made smaller the installation cannot be cancelled; sleep, the lid, the
power button and shutting down are held off.

**It refuses, and says why,** when the volume is encrypted with BitLocker or
cannot be read; when Windows is hibernated, Fast Startup is on or it was not
shut down properly (every NTFS partition of the disk is looked at for this);
when another partition of the disk cannot be looked at, or is encrypted with
BitLocker while the one to be made smaller holds no Windows; when a disk check
is waiting; when the file system has errors or records bad sectors; when the
partition is in use; when a laptop is not on its charger; when there is not
enough free space; and on disks with 4096-byte sectors. Disks that are part of
a firmware RAID set (such as Intel RST in RAID mode) are not installed on at
all. After a refusal nothing on the disk has changed.

**If it stops part way,** the failure page says which state the disk is in.
Where it names the installation's record (`/var/log/sengi-install.log`), copy
that file to a USB stick or another computer **before** you restart: it is in
memory, and it holds the partition table as it was.

- Before the resize began: nothing changed.
- During the resize, or its check: the file system may be damaged. Start
  Windows, let it check the disk, and restore your backup if it does not start
  or files are missing.
- After the check and before the partition was changed: the two agree, and
  Windows should start (and check the disk). To give the file system its room
  back, run `diskpart` as administrator, choose the volume (`list volume`,
  `select volume`) and enter `extend filesystem`. To go on instead, start
  Windows once, shut it down, run the installer again and give Sengi OS the
  same size as before. A larger size also works; a smaller one is not offered
  until the file system has been extended.
- Later: the partition is smaller and intact, and the space it gave up is free,
  so the installer can be run again. To give it the room back, delete Sengi
  OS's partition if there is one, then use "Extend Volume" in Windows' Disk
  Management.

**Limits.** Only NTFS is made smaller: Linux file systems are not. Only the
largest Windows data partition of a disk is considered. It can only be made as
small as it is full, with the margin above. Windows on another disk is not
looked at: if it is on another disk, shut it down fully all the same.

**What it erases (whole-disk install).** The whole disk you choose: every partition and
everything on them, with no way back. The installer shows what is on each
disk before you choose, and again before it starts. It does not offer the
stick it is running from, a disk that is in use, or one smaller than 8 GB.
Back up anything you want to keep first, and if the computer has more than
one disk, read the list with care.

**What you get.** A GPT disk with an EFI system partition and one ext4
partition, no swap partition (zram does that), GRUB for both UEFI and BIOS,
one account that uses `sudo` with its password, and a locked root account.
Beside another system you get one ext4 partition in the free space, GRUB for
UEFI in `EFI/sengios` of the EFI system partition that was there, and the same
account.

**What is not offered yet.** Encrypting the disk; moving partitions, or
shrinking anything but a Windows (NTFS) partition (make room from inside the
other system first); installing beside a system on a BIOS/MBR computer or a
disk without an EFI system partition; choosing or laying out partitions by
hand. Secure Boot has to be off.

**Firmware.** Wi-Fi and Bluetooth chips usually need firmware that is not free
software, and so does the audio processor in many Intel laptops. The image
includes Debian's set for the common ones (Intel, Realtek, MediaTek, Atheros,
Broadcom); see `THIRD-PARTY-NOTICES.md`.

**Updates do not install themselves.** An installed Sengi OS does not look
for updates in the background: the timers Debian uses for that are switched
off. Settings, Updates checks when you ask, and updates the system packages
and your Flatpak apps with your password.

**Printing.** Settings, Printers lists your printers, sets the default, prints a
test page, shows and cancels print jobs, and adds a printer by searching the
network or by its address (it asks for your password to add, remove or search).
Only printers that need no driver (IPP Everywhere, which most recent network
printers speak) are offered; a printer that needs its maker's
driver is not supported yet. CUPS is 87 MB with what it pulls in, so the image
does not carry it: Settings, Printers offers to install it (about 25 MB to
download, with your password), and once installed it starts only when something
asks, never at boot. Flatpak apps print through the portal, and other apps
through CUPS, with nothing more to set up. Details and measurements are in
[crates/sengi-print](../crates/sengi-print/README.md).

**Installing apps.** The Store, on the dock, shows recommended apps by
category, searches Flathub and installs apps for your user, with no password.
An install under way has a Cancel button. An app you installed this way has a
Remove button: close the app first; the Store asks, keeps the app's settings
and data in `~/.var/app`, and clears the shared runtimes that no other app of
yours uses.
Programs installed with apt or for the whole system are not removed from the
Store. From a terminal, `sudo apt install <package>` for Debian's
packages, or `sudo flatpak install flathub <app>` for Flatpak apps. Flathub is
already set up as the Flatpak source (`/etc/flatpak/remotes.d/`); the first
`sudo flatpak` command creates the Flatpak store. When Sengi OS is run from the USB stick without installing, every
change is kept in RAM, so a large Flatpak runtime may not fit and the install
stops with a "min-free-space-size" error. Install Sengi OS to the disk first.

To try the installer without a spare machine, give QEMU a blank disk beside
the ISO:

    qemu-img create -f qcow2 /tmp/sengi-disk.qcow2 16G
    qemu-system-x86_64 -enable-kvm -cpu host -smp 2 -m 4G \
      -device virtio-vga -display gtk \
      -drive file=out/sengi-os-1.0.0-amd64.iso,format=raw,if=virtio,readonly=on \
      -drive file=/tmp/sengi-disk.qcow2,format=qcow2,if=virtio

## Keyboard

| Keys | Action |
|---|---|
| Super+Space | Search and launch |
| Super+Return | Terminal |
| Super+L | Lock |
| Super+Q | Close window |
| Print | Screenshot of the screen, saved to `~/Pictures` |
| Alt+Print | Screenshot of the window in front |
| Super+/ | Show every keyboard shortcut |
| Super+V | Clipboard history (turn it on in Settings, Keyboard) |

## Sound

Settings, Sound picks the output and input device and sets their volume; the
volume keys and quick settings change the output in use. Speakers, headphones,
HDMI, USB and Bluetooth devices are meant to appear there on their own. "No
sound device" means that the system found no output at all.

If there is no sound, save a system report ([Reporting a
problem](#reporting-a-problem)). The kernel's messages are not in it, so also
run these in a terminal and keep what they print. The last one asks for your
password (on the live system it is `live`).

```sh
cat /proc/asound/cards
aplay -l
wpctl status
sudo dmesg | grep -i -E 'snd|sof|avs|hdaudio|hda_intel|audio'
```

To play a test sound on the speakers: `speaker-test -c 2 -t wav -l 1`.

On Intel laptops from Skylake on that have a digital microphone, the sound goes
through an audio processor. If no card shows up there, or the card is silent,
try the plain HD Audio driver, which plays through speakers and headphones but
cannot use a digital microphone. To try it once, press Esc while the machine
starts, press `e` on the first menu entry, add
`snd_intel_dspcfg.dsp_driver=1` to the end of the line that starts with
`linux`, and press Ctrl+X. To keep it on an installed system, put the same
words between the quotes of `GRUB_CMDLINE_LINUX=""` in `/etc/default/grub`
and run `sudo update-grub`.

## Reporting a problem

Open Settings, About and press Save system report. After some seconds a text
file named `sengi-os-report-` and the date is in your Documents folder; Show in
Files opens it there. An earlier report is never overwritten. Attach the file to
your problem report and say what you did and what went wrong. Nothing is sent
anywhere by Sengi OS. On the live system the file is in memory and gone at the
next restart, so copy it to a USB stick first.

The file holds the system and kernel versions, the processor, memory, disks,
PCI and USB devices with their drivers, the state of sound, Wi-Fi, Bluetooth,
graphics and the battery, and services that failed. User, computer, network and
Bluetooth device names, personal file paths, network addresses, serial numbers
and unique identifiers are replaced with markers such as `<user>` or `<mac>`.
A program does this and it can miss things, so read the file before sharing it.

Ordinary accounts may not read the kernel's messages or the system log, so these
are missing, and the file says so at the top. If someone asks for them, run this
in a terminal. Its output is not cleaned of names or addresses: read it, and
remove what you do not want to share.

```sh
sudo journalctl -b -p warning > ~/Documents/messages.txt
```

## Accessibility

Accessibility (Settings, Accessibility): text size (Small, Default, Large,
Extra large: 90, 100, 120 and 140 percent of every type size, and of the
heights that hold a line of text, in every program and in the title bars),
reduced motion (every animation takes no time) and a larger pointer (36 or
48 px, for the compositor and for programs started from it). Sticky keys,
slow keys and an on-screen keyboard are not built: they need a layer of the
compositor between the keyboard and every program that has to be tested with
real people before it is trusted, and an on-screen keyboard also needs a
text-input protocol the compositor does not have yet. Icons keep their size
at every text size. The settings apply to sengi-compositor; the labwc
fallback does not follow them.

