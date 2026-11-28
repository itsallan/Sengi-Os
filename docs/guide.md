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
   another system) there and answer the question that follows.

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
row says why; to make room, shrink the other system first, from inside it.
Back up what you want to keep anyway: this part is new, and it has been
tried on virtual disks only. If an installation fails after the partition
was made, the installer says what exists and how to remove the partition
(`sudo sfdisk --delete DISK NUMBER` from the live session, after looking at
`lsblk`); it does not delete partitions itself.

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

**What is not offered yet.** Encrypting the disk; shrinking or moving
another system to make room (do that from inside it first); installing beside
a system on a BIOS/MBR computer or a disk without an EFI system partition;
choosing or laying out partitions by hand.
Secure Boot has to be off.

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

