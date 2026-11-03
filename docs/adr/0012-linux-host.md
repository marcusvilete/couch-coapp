# ADR 0012: The Linux host

**Status:** accepted (2026-11-02), for M16c. The author's choices:
- **controllers first, keyboard later:** M16c-1 hosts with controller seats only; M16c-2 adds the keyboard-and-mouse seat, checked on KDE and GNOME and refused elsewhere;
- **the host's own controller outside seats** at first: the game sees it as it is;
- **KDE and GNOME,** on Wayland; other desktops untested and unpromised;
- **virtual controllers set up with consent:** a Set up button that installs one fixed udev rule, or the command to run by hand;
- **remote play's home PC on Linux:** the remote console in M16c-1, desktop control with the keyboard in M16c-2;
- **sound:** everything but the app itself by default, or one program picked from what's playing.

And for testing: a general Linux desktop is the target, not the Steam Deck (its Game Mode is out of scope); built and checked in WSL and CI, run in a VM, then on the Deck (AMD) and a dual boot of the author's PC (NVIDIA) for encoding and speed.

M16c-1 built so far ([results](../m16c-results.md)): part 1, 0.25.0 (capabilities, Linux's encoders); part 2, 0.25.1 (`uinput` controllers, the host's keys); part 3, 0.25.2 (the portal's capture, PipeWire, hosting on); part 4, 0.25.3 (our own sounds out, one program's, notifications). Settled while building it:
- **`can_host` split three ways and a fourth:** `HostCapabilities { sessions, keyboard_seat, desktop_control }`, each refused before anything is set up, told to a remote-play device too (wire revision 13, the author's choice); being a home PC run from afar is its own question (`Backend::home_pc`), Linux's from part 5;
- **a Linux stream captures from its start,** watched or not (`Backend::captures_without_viewers`), so the portal's dialog comes at Start streaming, never over the game when a guest arrives;
- **every portal request runs on one runtime for the process:** `ashpd`'s one D-Bus connection dies with the runtime that made it;
- **the AppImage never carries `libpipewire`,** which must match the desktop's;
- **our sound is every program's stream but ours, linked into our own node,** ours decided from each client's and stream's own info (a PulseAudio client's process isn't in the registry's snapshot), loopbacks and filters left out; one capture at a time, 1.5 s apart, since PipeWire 1.0.5 froze a program relinked faster;
- **notifications without answer buttons:** answering stays the host's keys and the console; they ask the server for no sound, which would be in the stream.

## Context

ADR 0011 made a Linux PC a viewer (M16a, M16b) and left the host for its own design, because a host needs what Wayland changes most:
- **capture** goes through a system dialog where the user picks the window;
- **input** is injected through the kernel or the desktop;
- **no app can ask which window is in front.**

What Wayland offers on KDE and GNOME (researched 2026-11-02):

| Need | Windows | Linux (Wayland) |
|---|---|---|
| Capture a window or a screen | Windows.Graphics.Capture, silently | The screen-sharing portal (`org.freedesktop.portal.ScreenCast`): the user picks in the desktop's dialog; frames come through PipeWire. With `persist_mode` 2, a restore token lets later sessions skip the dialog (tokens are single-use: each start returns the next) |
| Guests' controllers | ViGEm | `uinput`: virtual input devices made by the kernel. Writing `/dev/uinput` needs a udev rule most distributions don't ship (Steam's device rules reportedly often grant it; the app checks rather than assumes) |
| Guests' keyboard and mouse | SendInput | `uinput` too, or the remote-desktop portal with libei (a permission dialog each session; remote-desktop sessions can't persist) |
| Which window is in front | One call | No standard way. KDE: a KWin script reportedly can read the active window. GNOME: reportedly only through a Shell extension the user installs. Elsewhere: nothing. (Both from developers' reports, to verify in M16c-2's plan) |
| The panic key and answer keys | A low-level hook that ignores injected keys | The global-shortcuts portal (KDE; GNOME since 48). The desktop delivers the shortcut; it can't tell a key a guest injects through `uinput` from a real one |
| Hiding the host's own controller | HidHide | No equivalent: grabbing its evdev device hides it from programs reading evdev, not from those reading its raw HID device |
| H.264 encoding | NVENC, AMF, Quick Sync, Media Foundation | The pinned FFmpeg already has `h264_vaapi` (AMD, Intel), `h264_nvenc`, `h264_vulkan` and `libopenh264` (software) |
| Game sound | The game's process tree, through WASAPI | PipeWire: one program's stream, matched by its name, or every stream but ours. The portal doesn't say which program owns the chosen window |
| The host overlay | A window that never takes the focus, above the game | Wayland lets an app place no such window on GNOME (KDE has its layer-shell) |

The safety rules (CLAUDE.md, "Safety invariants") stand as they are; this ADR decides how a Linux host keeps them.

## Decision

### In parts

- **M16c-1, a controllers-only host:** a Linux PC hosts sessions with pad seats (and seatless sessions, M24): portal capture, guests' controllers through `uinput`, encoding with VAAPI, NVENC or in software, sound from PipeWire, the panic key through the global-shortcuts portal, requests in notifications. Remote play's home-PC side, without desktop control.
- **M16c-2, keyboard and mouse:** the keyboard-and-mouse seat, and desktop control, with the "game in front" rule kept on KDE (a KWin script) and GNOME (a Shell extension), refused where it can't be checked. Its own plan first: the answer keys and the panic key must then ignore guests' keys (below).
- **Later, measured as M9 was:** frames kept on the GPU (PipeWire's DMA-BUF into VAAPI or NVENC), HEVC, the host's own controller through a seat.

### Why controllers first

A controllers-only host keeps every safety rule on any desktop, unchanged:
- **guests get gamepads only** (`Permissions::GUEST_DEFAULT`), which reach the game whatever is in front, on Windows too;
- **no guest key exists,** so no guest can press the panic key or Accept through the global-shortcuts portal: a virtual pad carries gamepad buttons only, which no desktop takes for a shortcut, and Guide stays blocked;
- **a crash leaves nothing held:** a `uinput` device goes with the process that made it, and its pad with it. Windows needs `held_input` for keys; a pad on Linux needs nothing.

### The engine's capabilities

`Backend::can_host` (M16b) splits: hosting sessions, the keyboard-and-mouse seat, and desktop control become separate capabilities, each refused before anything is set up or announced, as `check_can_host` refuses now. On a Linux host in M16c-1, sessions are allowed; a keyboard seat, the keyboard in the input setup, and `RemoteOp::Desktop` are refused, and the UI hides them. `EngineInfo` carries the capabilities to the UI.

### Capture: the screen-sharing portal

- **Choosing what to stream opens the desktop's own dialog** (a window or a screen), in place of our list of windows. The restore token is kept per profile, so the same choice comes back without the dialog while the desktop can restore it.
- **Frames through PipeWire,** in memory first (shared memory, copied once), paced by our pacer (`couch_media::pacing`), which already times frames by arrival.
- **The window closing ends the stream,** as on Windows; the session goes on (M19). The portal names no process, so "the game exited" and "the window closed" are one event on Linux, and a game that swaps its window ends the stream rather than being followed: the host picks it again (the restore token often brings it back in one click).

### Guests' controllers: `uinput`

- **A virtual Xbox 360 controller per pad seat,** with the ids games expect, as on Windows, and its rumble (`uinput`'s force feedback) back to the guest.
- **Ours are told apart by what we name them,** a unique string per host and seat: a Linux stream window on the same PC skips exactly those (P9's rule, simpler on Linux, where we name our own devices).
- **Access to `/dev/uinput`:** the app checks it. If missing, Settings explains what it grants (any program of this user may create input devices, as Steam's rules allow) and offers Set up: `pkexec` asks for the administrator's password and installs one fixed rule, `/etc/udev/rules.d/60-couch-coapp-uinput.rules` (`KERNEL=="uinput", SUBSYSTEM=="misc", OPTIONS+="static_node=uinput", TAG+="uaccess"`), then reloads udev. The manual command is shown beside it. Nothing of the user's goes into that file.
- **Every rule in `SeatRouter` holds unchanged:** guests' pads reach `uinput` only through it.

### The panic key, and answering requests

- **Through the global-shortcuts portal:** the panic key, Accept and Decline are offered to the desktop with the user's keys (`shortcuts.json`) as the preferred ones. The desktop may let the user change them; the host console shows the keys in force.
- **Without the portal,** the session still starts, and the host console says the keys aren't available: its own "Take back every seat" does what the panic key does.
- **Accept still waits 1 s** after a request arrives (`ANSWER_AFTER`), as on Windows.
- **In M16c-2,** guests' keys exist. A key injected through `uinput` reaches the desktop as a real one, so the portal's shortcuts could be pressed by a guest: the answer keys are then off while a keyboard seat is held, and the panic key's behaviour is designed in that part (a guest pressing it only takes control from guests, the host's own aim; nothing is ever granted by it).

### Requests and notices: notifications

No window can stay above the game unfocused on GNOME, so a Linux host shows requests and notices as desktop notifications (with the overlay's sound), never taking the focus. The host console in the app stays the place to answer with the mouse; answering from the notification itself is the host's own click, as in the console. KDE's layer-shell overlay is a possible later step.

### Encoding

- **Tried in order:** `h264_vaapi` (AMD, Intel), `h264_nvenc` (NVIDIA), then `libopenh264` in software, through `couch_codec_ffmpeg::options::OptionSet` (the rule from M9a: never `AVCodecContext::open` with options). Each encoder's option sets end with the plainest that opens.
- **H.264 only** at first, as Linux viewers decode. HEVC later, from hosts that have it, to viewers that ask for it (M12c's rule).
- **Frames in memory first,** read back once per frame sent, never per viewer (M12a's rule); keeping them on the GPU is measured against it, with a switch, as M9 did.

### Sound: PipeWire

- **By default, everything but us:** every program's output stream, without the app's own (its notices' sounds). The host can pick one program instead, from a list of what's playing, by its name; the choice is remembered.
- **The stream level** (P7) applies as on Windows.

### Remote play's home PC on Linux

- **The remote console works** (M25a): unlocking with the PIN, starting sessions, passing seats, from the operator's device. Desktop control (M25b) waits for M16c-2.
- **The unattended PC** (M25c), the Linux way: kept awake with a logind inhibitor (sleep), and the screen too while a device is unlocked; start at sign-in as an XDG autostart entry, for an AppImage's default profile, naming its own `$APPIMAGE`; "Windows is locked" becomes logind's `LockedHint`. No notification-area icon (GNOME shows none without an extension): closing the window quits.

### Where the code goes

- **`couch-platform-linux`** gains it all: the portals (through `ashpd` or `zbus`), PipeWire (`pipewire`), `uinput` and evdev, logind, the udev rule. Every crate MIT or Apache-2.0, checked by `cargo deny`.
- **System libraries stay the system's:** PipeWire's client library must match the running daemon, so the AppImage never carries it (as it already doesn't carry the GPU's drivers); the notices' tracing (`notices/system_libraries.rs`) would catch it if the bundler did.
- **The engine's Linux backend** (`ViewerOnly` today) becomes a full backend whose capabilities say what it can do.

## Consequences

- **A Linux PC hosts couch co-op** with friends' controllers on KDE and GNOME, every safety rule intact, from M16c-1.
- **Keyboard games stay Windows-hosted until M16c-2,** and then only where the "game in front" rule can be checked.
- **The host picks what to stream in the desktop's dialog,** once while the restore token holds. A game that replaces its window needs picking again.
- **A first host on a PC asks once for administrator rights** (or a command), for virtual controllers, as Windows asks for ViGEmBus.
- **The host's own controller isn't on a seat** at first: the game sees it beside the guests' virtual ones, and its player order is the game's.
- **Guests on any system** watch and play a Linux host the same way: no wire change is expected from M16c-1.

## Testing

| Where | What it checks |
|---|---|
| WSL and CI | building, clippy and the tests; the session scenarios with the fake backend, a capabilities scenario (a host that can't offer a keyboard seat refuses it) |
| A VM (KDE and GNOME on Wayland) | the portal dialog and restore token, `uinput` pads seen by a test game and by `evtest`, the Set up button, the global-shortcuts portal, notifications, PipeWire sound, a session end and a host killed leaving nothing held; software encoding |
| The Steam Deck (Desktop Mode, AMD) | VAAPI encoding, a real game, latency |
| A dual boot of the author's PC (NVIDIA) | NVENC on Linux, a demanding game, latency and load |
| Windows | everything unchanged: the scenarios, `release-check`, `lifecycle-check`, `input-sink` |

A Linux `release-check` follows the Windows one: a host killed, a viewer gone, a session ended, each leaving the virtual pads neutral or gone, timed.

## Open questions, for the parts' plans

- **M16c-2:** how the KWin script and the GNOME extension are installed and trusted (the extension is the user's to enable); what a guest's keys may do to the global shortcuts; whether the remote-desktop portal (libei, asked each session) beats `uinput` for keys, given the "in front" check is ours either way.
- **The host's own controller through a seat:** grabbing its evdev device hides it from games reading evdev (SDL's usual path), not from raw HID readers; measured on real games before promising it.
- **A Shell extension or KWin script for the "in front" check** must not become a way to drive the desktop: what it exposes is read-only, the active window's identity.
