---
name: linux-host-vm-run
description: Try a Couch Co-App Linux host on a real Linux desktop (a VirtualBox VM with GNOME and KDE on Wayland, or a Linux PC or Steam Deck in Desktop Mode) — what WSL can't show: the screen-sharing portal's dialog and remembered choice, the host's keys through the global-shortcuts portal, desktop notifications, guests' uinput controllers and Set up, the stream's sound (the chime left out, one program picked), a session ended or the host killed leaving nothing held. First finds out whether there is such a desktop at all and asks before setting one up. Use when a change to the Linux host (couch-platform-linux's portals, sound, notices, pads; pipeline::linux; the UI on Linux) needs a real desktop, when the author wants to run the results page's "In the VM" items, and never as a gate for committing.
---

# A Linux host on a real desktop

WSL builds and tests the Linux host (the `linux-checks` skill), but has no desktop session: no screen-sharing portal, no global shortcuts, no notification server of its own, no real sound device. This is the run on a real one. **Optional:** nothing in the build, the tests or CI needs it, and a missing VM never blocks a commit or a milestone (`milestone-wrap-up` lists it as optional). What isn't tried stays in the results page's "Still to come".

## Before: is there a desktop to run on?

1. Look first, assume nothing: `& "C:\Program Files\Oracle\VirtualBox\VBoxManage.exe" list vms` (VirtualBox installed, and which VMs), or ask whether the author has a Linux PC or a Steam Deck to hand. A Linux PC or the Deck's Desktop Mode works with this checklist as it is (skip "Once").
2. **None:** say what one costs, then ask; never install anything unasked. VirtualBox (an installer and an admin prompt), an Ubuntu ISO of about 6 GB, about 25 GB of disk, an hour (most of it unattended), and the author at the screen for the installer's questions, the first sign-in and every dialog in the checklist. The alternative: run only what WSL can (`linux-checks`) and leave the rest in "Still to come".
3. Check the disk first (`Get-PSDrive C`): `target\debug` has filled C: twice.

## Once: make the VM (VirtualBox)

- **The system: Ubuntu 26.04 LTS,** desktop ISO, from ubuntu.com, its SHA-256 checked against the page's. Its GNOME (48 or later) has the global-shortcuts portal; Ubuntu 24.04's GNOME 46 doesn't, so 24.04 can't test the host's keys on GNOME. Its packages match `scripts/linux-deps.sh`.
- **The VM:** 4 CPUs, 8 GB of memory, a 40 GB disk, VMSVGA graphics with 3D acceleration and 128 MB of video memory (Wayland needs it), the audio device on (PipeWire needs a sink), the network **bridged** to the PC's adapter (so a Windows guest on this PC reaches it directly; NAT works too, through a relay).
- **Install** with the defaults, then the Guest Additions (Devices → Insert Guest Additions CD, `sudo apt install build-essential`, run it), and KDE beside GNOME: `sudo apt install kde-plasma-desktop`, choosing SDDM or GDM when asked (either works); the login screen then picks "Ubuntu" (GNOME) or "Plasma (Wayland)".
- **Tools for the checks:** `sudo apt install evtest pipewire-bin pulseaudio-utils supertuxkart` (SuperTuxKart: a native game with sound that reads controllers through SDL).
- **Keep it reusable:** take a snapshot after this ("clean, tools in"), and go back to it before a run that installs a new build. The VM's disk lives in VirtualBox's folder, never in the repository.
- The first time this is done for real, put what was run into `scripts/vm/` (creating the VM with `VBoxManage`, checked as it ran) and point here at it. Until then these are the steps by hand.

## Each run

**Get the build in:** an AppImage from the branch, built as the `release-installer` skill says (`bash scripts/build-appimage.sh --test-relay` from `~/couch-release`, the branch checked out there), copied into the VM (a shared folder, or `scp`); `chmod +x` it, start it. Or a clone in the VM built from source (`sudo bash scripts/linux-deps.sh`, rustup, `. scripts/dev.sh`, as `docs/development.md` says), for runs that change code between tries. The guest is this PC's app (`npm run tauri dev`, or the installed one) in the same room.

**Then, once on GNOME and once on KDE** (sign out, pick the other session), with the author at the VM's screen; ask them to try each, and say what to look for:

| Area | Try | Expect |
|---|---|---|
| Set up | Settings → Drivers → Virtual controllers → Set up | one pkexec prompt; afterwards a session plugs pads (`ls -l /dev/uinput`: the group rule in place) |
| Sharing | Start a session, Start streaming | the desktop's dialog at Start streaming, never when a guest arrives; a window and a screen both; Stop streaming with the dialog open closes it |
| The choice remembered | stream again, and again after restarting the app | no dialog; until it's revoked in the desktop's settings |
| A window closed, a share stopped | close the shared window; stop from the desktop's indicator | the stream ends, the session goes on |
| The host's keys | the panic key, Accept and Decline, over two Go lives in a row | the desktop's binding dialog the first time; the console shows the desktop's names for them; Accept answers only a request pending 1 s |
| Guests' controllers | the guest takes a seat and plays SuperTuxKart in the VM | the pad moves the kart; `evtest` shows it as an Xbox 360 controller; rumble reaches the guest |
| Sound | the guest listens while SuperTuxKart plays | the game heard; **the request chime never** (the author asks for a seat from the guest side, the host hears the chime, the guest must not); a stream window of the VM's own, watching another host, never in the stream |
| One program | the stream picker's sound: SuperTuxKart, then a browser playing music | only the program picked; the choice remembered for the next stream |
| Replays | if a loopback or EasyEffects is installed | the game heard once, not doubled |
| Notifications | a request, a viewer joining and leaving, with the app's window not in front | a request stays until answered and goes when it is; notes go by themselves; none while the app is in front; none left after quitting the app; no notification sound in the stream |
| Remote play's home PC (part 5) | Settings → Remote play in the VM: a PIN, the Windows app added as a device, remote play on; from Windows, unlock it | the screen-sharing note, no notification-area switch; `systemd-inhibit --list` shows Couch Co-App (`sleep`, then `sleep:idle` once unlocked), or the log says logind refused and the desktop holds it; the VM neither suspends nor blanks its screen past the desktop's own timeouts (shorten them in its settings to try) |
| Its lock screen | lock the VM (Super+L) while Windows is unlocked as its device | the device says the PC is locked within about 2 s, and unlocked again after signing in |
| Streams from afar | from Windows, start a session and a stream, first with no choice remembered (revoke it in the desktop's settings), then after sharing a screen at the VM once; then share a window, close it, and stream from Windows again | refused with the words to share at the PC once; then streams without a dialog; the dialog that comes for the gone window closes after 30 s, and the device says nothing was chosen; on a desktop that never bound the host's keys, a session started from Windows: the shortcuts dialog closes after 30 s, and the console says the keys aren't available |
| Start at sign-in | turn it on (the AppImage only), sign out and in; rename the AppImage as a newer version would be, start it once, sign out and in | `~/.config/autostart/couch-co-app.desktop` names the AppImage; the app starts with its window; after the rename, the entry names the new file |
| Teardown | End session; then kill the app mid-game (`pkill -9 -f -i couch`, its stream windows with it) | the pads gone (`evtest` lists none of ours), nothing held in the game; after the kill, a request's notification may stay (only a clean end closes them) |

**Collect** as you go: the app's `desktop.log` in the profile folder (`~/.local/share/CouchCoApp/profiles/default`), `pw-top -b -n 2` and `pw-link -l` while streaming (our node, `couch-coapp-sound-…`, linked to the programs taken), and the guest's stream window stats line.

## After

- Each item tried goes into the milestone's results page (`docs/m16c-results.md`) under its part: what was tried, on which desktop, what happened; the ones left stay in "Still to come".
- A failure found here is a bug like any other: fix it on a branch, with a test in WSL where one can reach it.
- Tell the author which snapshot the VM is at, if a build was installed into it.
