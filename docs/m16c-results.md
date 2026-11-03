# M16c results: the Linux host

## Why

[ADR 0012](adr/0012-linux-host.md)'s first part, M16c-1: a Linux PC (KDE or GNOME on Wayland) hosts sessions with controller seats only, every safety rule unchanged on any desktop. Keyboard and mouse, desktop control and the "game in front" check are M16c-2.

M16c-1 comes in five PRs (the third was split in three, the author's choice of 2026-11-03):
- **part 1 (version 0.25.0, wire revision 13):** what a system can host, and Linux's encoders;
- **part 2 (0.25.1):** guests' controllers through `uinput`, and the host's keys through the global-shortcuts portal;
- **part 3 (0.25.2):** capture through the screen-sharing portal and PipeWire, the PC's sound, and hosting turned on;
- **part 4 (0.25.3):** the app's own sounds left out of the stream, or one program's sound alone, and requests as desktop notifications;
- **part 5 (0.26.0, wire revision 14):** remote play's home PC on Linux (kept awake, the lock screen, start at sign-in, streams from afar only with a remembered choice).

**The author's choices** (ADR 0012, 2026-11-02; and 2026-11-03):
- controllers first, keyboard later;
- the host's own controller outside seats at first;
- KDE and GNOME;
- `/dev/uinput` set up with consent;
- the remote console on a Linux home PC now, desktop control later;
- sound: everything but the app, or one program picked;
- **the operator's device told what its home PC can host** (a wire change), rather than offered what then gets refused.

## Part 1: capabilities and Linux's encoders

**What a system can host** (`HostCapabilities { sessions, keyboard_seat, desktop_control }`)
- **`Backend::capabilities` replaces `can_host`.** Windows has all three. Linux's backend has none yet: part 3 turns on sessions, controller seats only.
- **Each is refused before anything is set up or announced** (`check_capability`):
  - without `sessions`: what M16b refused;
  - without `keyboard_seat`: a session asking for one, from the host's window or from afar;
  - without `desktop_control`: `RemoteOp::Desktop { on: true }` (turning it off always works);
  - the host's own controllers on seats where the system doesn't read them (`local_pads` is `None`, as on Linux).
- **The UI follows `EngineInfo.capabilities`:** Start a session as before (`sessions`); the start dialog drops the keyboard-and-mouse choice without `keyboard_seat` and says why.
- **The operator's device is told** (`RemoteState.capabilities`, wire revision 13, so 0.25.0 doesn't talk to 0.24.x): its start dialog for that PC drops the keyboard seat, and its console drops the desktop panel. The home PC refuses either way; the device only hides.
- **A `remote-console-linux` preview scenario** shows a Linux home PC run from afar.

**Linux's encoders** (`couch_codec_ffmpeg`)
- **Each system's list:** Windows unchanged; Linux `h264_vaapi` (AMD, Intel, the Deck), `h264_nvenc`, then `libopenh264`. NVIDIA's VAAPI driver only decodes, so VAAPI fails to open there and NVENC follows. No HEVC from a Linux host yet.
- **VAAPI takes only its surfaces** (`vaapi.rs`): each frame in memory is converted to NV12 (swscale, BT.709 limited range, as for every encoder) and uploaded into a surface from the encoder's own pool, on the default VAAPI device. A requested keyframe (`pict_type` I) starts an IDR there.
- **VAAPI's option sets:** CBR, one frame in flight, `low_power` first (Intel's fixed-function encoder; AMD's Mesa refuses it), then the plainest, which stands in for "the settings before M9a" in that test.

**A Linux backend** (`pipeline::linux`, replacing M16b's `ViewerOnly` on Linux; other systems keep it): the stream window, the clock, the encoders, the test pattern and the tone. `couch-cli host --source testpattern` runs on Linux.

**A race Linux showed** (once in nine runs of the session scenarios in WSL): a watch asked for whose connection closed before the host answered was refused at once ("lost the connection to the host"), even when another connection to the host was about to replace it (two members dialing each other at once keep one connection; M16a covered the case where the kept one is already there). Now such a watch waits like any reconnect (`WatchState::Reconnecting` keeps the command's reply): answered when the host announces its session on the new connection, refused when the reconnect wait runs out.

## The review

A review agent found no way to start a keyboard seat or desktop control on a host without them, the wire change complete, and VAAPI's references right. Its small points, all taken:
- **A bitrate change rebuilt VAAPI's device** (each reopen opened a new one): a reopen now shares the old encoder's device and surfaces (`VaapiFrames::share`), and a missing device stops at once rather than being tried with each option set.
- **The surface pool was fixed at 8,** and would fail every upload once exhausted: it now grows.
- **Comments said a Linux host has controller seats** before it does; and the start dialog stored a keyboard choice it had forced off, over the device's own: it stores the choice made.
- **Unverified until AMD or Intel hardware runs it:** VAAPI's plainest option set still asks for the high profile and CBR, which a driver could refuse (then that GPU falls back to software).

## Checked

- **Windows:** clippy; the protocol crate (73); the engine's tests, the rooms scenarios (27) and the session scenarios (79), the new `a_controllers_only_host_refuses_keyboard_and_desktop` included; the codec crate, every encoder opened twice without a crash; tsc.
- **Linux (WSL):** fmt, clippy, the codec crate. `couch-cli probe`: `h264_nvenc` opens (WSL reaches the NVIDIA GPU) and `libopenh264`; VAAPI has no device in WSL (no DRM render node, even with Mesa's VAAPI drivers installed), so the VAAPI path waits for AMD or Intel hardware (the Steam Deck).
- **A Linux host streaming to Windows:** `couch-cli host --source testpattern` in WSL, watched by `couch-cli view` on Windows through iroh's relay, then direct: 60.2 fps, 566 frames in 10 s, no decode errors or lost packets, sound with no underruns; the host encoded with `h264_nvenc` from frames in memory, about 5 ms a frame. (Capture to present can't be read across two machines: their clocks differ.)

## Part 2: guests' controllers and the host's keys (0.25.1)

**Virtual controllers through `uinput`** (`couch_platform_linux::pads`)
- **Each pad is made like the kernel's own Xbox 360 controller** (`xpad`): its ids (`045e:028e`, version `0x114`), name, buttons (its X is `BTN_NORTH`, its Y `BTN_WEST`), sticks with Y pointing down, triggers, and the D-pad as a hat. Guide is never sent.
- **Named ours by its `phys`,** `couch-coapp/<pid>/pad<N>` (the process's Nth pad): the stream window on the same PC reads that from sysfs and skips exactly those (`pads::is_ours`, its Linux `host_check`).
- **Rumble:** a game uploads its force-feedback effect and waits for the answer, so each pad has a thread that answers at once, follows which effect plays (and for how long), and reports the motors.
- **A crash leaves nothing held:** a `uinput` device goes with the process that made it.
- **Access:** making a device needs `/dev/uinput` writable. Settings → Drivers lists "Virtual controllers" on Linux, with Set up (`pkexec` asks for the administrator's password, then `uinput` is loaded where it's a module and one fixed udev rule is installed and applied) and the same command to run by hand (a test keeps the two the same). If this session still can't use it, Set up says to sign out and back in. Undo it by deleting `/etc/udev/rules.d/60-couch-coapp-uinput.rules`. Go live's "virtual controllers missing" warning reads it too (`Backend::virtual_pads`).
- **`couch-cli pad-test` runs on Linux,** a `uinput` pad to watch with `evtest`.

**The host's keys through the global-shortcuts portal** (`couch_platform_linux::global_keys`, `ashpd`)
- The panic key, Accept and Decline are offered to the desktop with the user's keys as Ctrl+Alt+Shift+key, the desktop's own key names. The desktop may let the user change them; **the console shows the keys the desktop names** (`HostKeyEvent::Bound`, again whenever they change).
- **Binding returns at once;** the desktop answers later. If it has no portal, doesn't open a session within 5 s, refuses the keys, or goes away, the console drops them (`HostKeyEvent::Lost`), as on Windows when the hook fails: it says so, and Take back does the panic key's work. A panic key the desktop didn't bind takes the answer keys with it.
- **One portal runtime for the process** (`couch_platform_linux::portal`): `ashpd` keeps one D-Bus connection per process, served by the runtime that made it.
- **Safe without telling injected keys apart** only because a Linux host offers no keyboard seat: no guest has keys.

**The review** found the host's keys would have stopped working from a session's second Go live on, and two smaller gaps there, all fixed:
- **The second session's keys:** each binding ran on a runtime of its own thread, and `ashpd`'s one connection per process, made on the first, died with it. Now every portal request runs on one runtime for the process (above); a test binds twice where there's no portal and sees both fail at once.
- **A failed binding wasn't shown:** the console showed the panic key whatever the desktop answered. Now the answer reaches the console (above); a session scenario has the desktop rename the keys, leave one unbound and go away.
- **Binding blocked the room actor** for up to 6 s (3 s, twice with the panic key alone) where the portal didn't answer; and a binding that timed out kept trying in the background. Now it returns at once, and a dropped binding stops at once.

Its small points, taken:
- **Rumble:** a game's repeat count and delay now set how long an effect plays; a failed read no longer ends the rumble thread (a game's next upload would have waited on it); a pad's thread is woken to stop at once rather than within 100 ms (`eventfd`).
- **The `phys` number** counts the process's pads, not seats: `pad<N>`.
- **Set up** loads `uinput` first, says when a sign-out is needed, and names pkexec's other refusal (no agent to ask with).

Left: the portal's session isn't checked on each activation (`ashpd` keeps a session's path private); only one task reads activations at a time, so they are ours.

**Found while building:** evdev 0.13.2's `with_phys` always failed (`EINVAL`): its `UI_SET_PHYS` carried a size of 1 where the kernel expects a pointer's. Vendored with the one-line fix (`third_party/evdev/PATCHED.md`), as vigem-rust is.

**Checked**
- **Windows:** clippy, the engine's tests (the session scenarios, 80, with the new `keys_the_desktop_binds_or_drops_reach_the_console`), the viewer's, the CLI's and the input crate's, tsc, vitest, `cargo deny` (the new crates: evdev, ashpd).
- **Linux (WSL):** fmt, clippy, the workspace's tests, the session scenarios eight times in a row; `couch-platform-linux`'s tests (binding twice without a portal fails at once both times); as root, the real pad: made, found ours by `phys`, B read back, a game's rumble uploaded, played (`0x80`/`0x40`) and stopped; `pad-test` seen by `evtest` exactly as `xpad`'s controller (`045e:028e` v`0x114`, its buttons).
- **Not in WSL:** the global-shortcuts portal (no desktop session there), for the VM.

## Part 3: capture, sound, and hosting on (0.25.2)

**What the desktop shares** (`couch_platform_linux::screencast`)
- **One source:** a Linux host's picker offers "A window or a screen"; the desktop's own dialog (the screen-sharing portal) asks which when the stream starts. To the pipeline it is a screen (`pipeline::linux::SHARED`): no window to keep in front, the PC's sound.
- **The dialog comes at Start streaming,** watched or not: a Linux stream captures from its start, and goes on while nobody watches (`Backend::captures_without_viewers`), never asking again when a guest arrives.
- **Starting returns at once,** the dialog running inside the capture: Stop streaming never waits on it, and dropping the capture closes the portal's session, which ends a dialog still open. A dismissed dialog fails the stream with "nothing was chosen to share".
- **The choice is remembered:** the portal's restore token (one start each, every start giving the next) is kept in the profile's `portal.json`, never backed up nor logged, so the next stream, this run or the next, starts without the dialog until the user revokes it in the desktop's settings.
- **Frames over PipeWire in memory:** BGRx, BGRA, RGBx or RGBA, up to the stream's rate, each copied out packed as BGRA (red and blue swapped where needed), stamped on `CLOCK_MONOTONIC`. The newest frame waits for the pipeline; one not yet taken is replaced.
- **The shared window closing, or the user stopping the share** from the desktop's own indicator, ends the stream as its source lost; the session goes on.
- `couch-cli host --source monitor` shares through the dialog too.

**What the PC plays** (`couch_platform_linux::sound`): the default output's monitor through PipeWire, which converts it to 48 kHz stereo float, in 10 ms quanta. It included this app's own sounds, so a Linux host played none: no request chime, and no overlay (no window can stay over a game on GNOME without taking the focus); Settings hid Overlay and sounds there. Part 4 leaves the app's sounds out and brings notifications.

**Hosting on:** the Linux backend's `capabilities` now say sessions (no keyboard seat, no desktop control). The UI shows Start a session, Drivers and the host's shortcuts; the start dialog offers controller seats alone, as part 1 made it. Being a home PC run from afar waits for part 5 (`Backend::home_pc`): Settings → Remote play offers a Linux PC only "This device".

**The AppImage never carries `libpipewire`:** it must match the PipeWire the desktop runs. The bundler is told to leave it out (`LINUXDEPLOY_EXCLUDED_LIBRARIES`), and `xtask appimage` fails if it is inside anyway.

**The review** found no memory-safety problem or panic in PipeWire's callbacks, the safety rules intact, and the framer content with PipeWire's stamps. What it found, all taken:
- **Remote play's home PC came on with sessions:** a device could start a stream on an unattended Linux PC, whose dialog nobody would answer, and the PC would sleep and say it was unlocked when locked. Being a home PC is now its own question (`Backend::home_pc`, `EngineInfo.home_pc`), Windows only until part 5; a scenario has a host that isn't one refuse remote play's home side and still host.
- **The dialog came with the first viewer,** not at Start streaming: over the game when a friend arrived, and again whenever everyone left and someone came back. A Linux stream now captures from its start, watched or not (`Backend::captures_without_viewers`); a scenario checks one capture all along, through a viewer leaving and coming back.
- **The sound's callback ran on PipeWire's realtime thread,** sharing its state with the main loop's: it runs on the main loop now.
- **A chime made for a session run from afar** could still play into a later Linux stream: a Linux host's Go live drops it.
- **A session opened while Stop came** was left open: the open finishes, then the session is closed.
- **The picker's one source** loads in one placeholder row, so nothing below it moves.
- **The AppImage now needs the system's `libpipewire`** to start at all: said in the self-hosting guide and the release skill. The bundler's exclusion setting is recent (linuxdeploy, August 2025), so the check after bundling is what holds.

Left: a restore token is used up by a start that fails before the portal answers, so the dialog comes back once.

**Checked**
- **Linux (WSL), PipeWire's half, without a portal** (PipeWire, WirePlumber and GStreamer's PipeWire plugin installed in WSL for the checks): a red 640×480 RGBx test source came out as BGRA frames, packed, and quitting ended the thread at once; a sine played into a null output was captured from its monitor in the stream's format.
- **A Linux host's sound end to end:** `couch-cli host --source testpattern --audio system` in WSL, the sine playing, watched from Windows' `couch-cli view` through iroh's relay: 60 fps, the audio buffer at 17 ms (its target 15), nothing concealed, no underruns.
- **Linux (WSL):** fmt, clippy, the workspace's tests, the session scenarios (82, with `a_backend_capturing_unwatched_captures_once_from_the_streams_start` and `a_host_that_isnt_a_home_pc_refuses_remote_play_and_still_hosts`) and the rooms scenarios three times each.
- **Windows:** clippy, the engine's tests (the same 82 and 27), xtask's (`pipewire_is_never_bundled`), `cargo deny` (the new crates: pipewire, libspa), tsc, vitest, and `npm run shots` with the new `linux-host` scenario and its picker: no problems.
- **Not in WSL:** the screen-sharing portal (no desktop session there), for the VM.
- **Found on the way:** C: filled up during the checks (`target\debug` at 80 GB); `cargo clean --profile dev` freed it, and WSL's debug build, damaged meanwhile, was rebuilt clean.

## Part 4: our own sounds left out, one program's, and notifications (0.25.3)

**Everything but us** (`couch_platform_linux::sound`, `Sounds::AllButOurs`): no longer the default output's monitor. Our capture stream links to nothing by itself (`node.autoconnect` off); it watches PipeWire's registry and links each program's output stream (`Stream/Output/Audio`) into its own two inputs, which PipeWire mixes and converts to 48 kHz stereo float as before. Stereo goes as it is, sides and rears with their fronts, mono and centre to both, the low-frequency channel left out. A stream that starts later is linked as it comes; our links go with our capture (`object.linger` off). A stream is ours, and left out, when a process id PipeWire has for it (the stream's or its client's `application.process.id`, or the client's `pipewire.sec.pid`, its connection's) is this process or one of its descendants by `/proc`: the webview's processes, which play the chime, and a stream window on the same PC. Read from each stream's and client's own info, bound for it, never the registry's snapshot alone: for a PulseAudio client (WebKit plays through one) the registry shows only pipewire-pulse's process, its own id only in the info. A stream is taken once its info has come, and given up again if a later fact makes it ours. **Replays are left out:** a loopback's or a filter chain's output (`node.link-group`, `stream.is-loopback`; the Steam Deck's speaker filter is one), and the streams of a client that has a sink of its own (effects like EasyEffects), which play again what others play. Streams to any output are taken, not only the default's. While nothing it takes plays, silence comes all along (`node.always-process`), as Windows' loopback gives it.

**One program's sound** (`Sounds::Program`): the same links, to the streams of the program named alone, by the name PipeWire has for it (the application's name, else its binary's; Proton's games give their exe's name). `Command::ListSoundSources` lists what plays now, the app left out (`sound::programs`, one pass over the registry, off the actor's thread); `Command::StartStream`'s new `program` picks one, refused where the sound follows the window (`Backend::picks_sound`, Windows) and for an empty or overlong name. The stream picker on Linux offers "Everything but Couch Co-App" or a program, with a refresh button; the choice is remembered (the webview's storage), and a program picked before stays on offer while it isn't playing. A device running its PC from afar picks none (`RemoteOp::StartStream` is unchanged: no wire change): the PC sends everything but the app.

**Notifications** (`couch_platform_linux::notices`, `Command::ShowNotices`): on a Linux host the overlay's list (`useOverlay`, under the same Show setting and only while the app isn't in front) goes to the desktop's notification server (`org.freedesktop.Notifications`, through `ashpd`'s zbus, on the portals' runtime), with the overlay's words (`noticesOf`). Each notice has the toast's id: a new one is shown, a changed one replaced in place, a gone one closed. Requests stay until answered (critical urgency), notes go by themselves (normal, transient). They ask the server for no sound (`suppress-sound`): the server's sound would be in the stream. The body is escaped where the server reads markup (`body-markup`); the summary is plain text by the spec. At most 16 notices, each text cut to 500 bytes, rather than a list refused (which would leave an answered request up). A crash leaves a request's notification up (only a clean end closes them), and a shared screen shows the notifications, unlike Windows' overlay. Shown from one task, the newest list winning over any not yet shown; closed at the engine's end, so no request outlives the app. No answer buttons: answering stays the host's keys or the console.

**One capture at a time, and 1.5 s between them** (`sound::COOLDOWN`, through a gate every start passes): PipeWire 1.0.5 (Ubuntu 24.04's, in WSL) froze a program whose stream we linked again within about a second of our last links to it going, its own sound to the output gone silent too, and the other programs' with it (the null output's monitor recorded nothing; `pw-top` counted hundreds of xruns on the players). A stream started again at once (Stream this instead) does that. Found by the test failing half the time, at its third capture; ending a capture alone left the programs playing, a second's gap made it pass every time, and `pw-link` with `pw-record` didn't reproduce it. Ending a capture also takes its node down and waits for PipeWire to say so (at most 0.5 s) before the cooldown starts. The pipeline starts the next stream's capture before the last one's thread has ended, so a start waits for that end first (at most 5 s), then the cooldown. Whether newer PipeWire needs it is for the VM.

**The chime is back on a Linux host,** and Settings shows Overlay and sounds there, worded for notifications. The `chime` prop and `dropChime` (part 3's guard against a chime made for a session run from afar) are gone: the stream leaves the app's sounds out.

**Checked**
- **Linux (WSL), PipeWire:** `ours_is_left_out_and_one_program_is_taken`, against a null output, its sines from `pw-play` (GStreamer's `pipewiresink` stalled under the same churn): a sine of our own (a child process) wasn't heard, another program's (started by a shell that left) was, and listed while ours wasn't; picking that program by name heard it, in a capture started as soon as the last ended, and a program not playing heard nothing; silence came all along in every capture; the output's monitor still played the programs afterwards; no link of ours left (`pw-link -l`). Six runs in a row passed with the cooldown; without it, about half failed. `what_plays_is_captured` with a sine playing.
- **A Linux host's sound end to end, with the new capture:** `couch-cli host --source testpattern --audio system` in WSL, a sine playing, watched from Windows' `couch-cli view --measure-secs 20` (direct, 0.3 ms round trip): 59.6 fps, 1,921 sound packets played, the buffer at 18 ms (target 15), 2 concealed, none late, no underruns, no decode errors.
- **Linux (WSL), PulseAudio** (`pipewire-pulse` and `pulseaudio-utils` installed in WSL for the check, `PULSE_SERVER` pointed at it past WSLg's own): `a_pulse_client_of_ours_is_left_out`, our own `paplay` not heard nor listed, another's heard; twice. A `pw-loopback` running beside them wasn't listed.
- **The gate:** `the_gate_waits_for_the_last_capture_then_the_cooldown`; and the main test starts its program capture before dropping the last one, as the pipeline does: three runs in a row passed.
- **Linux (WSL), notifications** (`dunst` installed in WSL for the check): `notices_are_shown_replaced_and_closed`, a request and a note shown, the request replaced in place under the same id, the note closed, then everything closed.
- **Linux (WSL):** fmt, clippy, the workspace's tests, the session scenarios (83, with `a_host_picking_its_sound_streams_the_program_picked`). Under the full parallel run two rooms scenarios on invite expiry (`revoked_and_expired_invites_stop_working`, `an_opened_invite_is_previewed_before_joining`) failed once, then passed three runs in a row alone: WSL's wall clock jumping (the linux-checks pitfall), nothing here touches rooms.
- **Windows:** fmt, clippy, the workspace's tests twice, the session scenarios twice more (83), tsc, vitest (with `soundChoices` and `noticesOf`), `cargo deny`, and `npm run shots` with the Linux picker's sound choice and Settings on Linux: no problems, nothing jumps. One run of the sessions scenarios failed once in its setup (`OwnerOffline` while three engines joined their room, `no_hevc_with_frames_in_memory`), under the whole workspace's load; three runs since passed. Worth watching: a race in joining, not in this change's code. After the second review.s fixes, the full run once failed `a_wrong_password_is_refused_then_rate_limited` (the backoff had run out before the second try), then the rooms scenarios passed three runs in a row alone. With the two on Linux, the rooms scenarios that time real seconds fail now and then under the whole workspace.s load: worth making sturdier, apart from this change.

**The review** (a fresh agent over the change) found, all checked against the code:
- **The cooldown didn't cover a stream started again,** the case it was for (high): the pipeline starts the next stream's capture before the last one's thread has ended, so the new capture saw no end yet and linked at once. Now a gate counts captures in and out: a start waits for the last to end (at most 5 s), then the cooldown; a pure test of the gate, and the WSL test now starts the next capture before dropping the last.
- **The chime could reach guests** (medium): WebKit plays through PulseAudio, and for a PulseAudio client the registry shows only pipewire-pulse's process, with no `application.process.id`; any id missing counted as not ours. Confirmed in WSL with `paplay` (`pw-cli ls` against `pw-cli info`). Each client and stream is now bound and decided on its own info, and given up again if a later fact makes it ours; `a_pulse_client_of_ours_is_left_out` checks it.
- **Loopbacks and filters doubled the sound** (medium): their outputs are streams too, and play again what the programs feeding them play. Left out now (above).
- **Notices refused whole** (low) left an answered request up: cut instead. **Markup** (low): bodies escaped where the server reads it. **Notifications show in a shared screen, and outlive a crash** (low): said in Settings and above.
- **Left as they are, said above or in the VM list:** a process id a program declares counts as proof (a sandboxed one, in its own namespace, could match one of ours and be left out: unlikely, and failing toward silence for that program alone); the downmix summing at full scale; programs on two outputs joining one driver.
- **The linux-checks skill** named the wrong player for its test: fixed.
- **A second review** (a fresh agent, since the first found a high one) found the fixes sound (no deadlock, lost wakeup, re-entrancy or drop-order problem), and these, all taken: a start whose stream stopped while it waited at the gate still captured, and held up the next start and the session's end, by seconds after quick restarts (`Backend::audio` now takes the stream's stop: a start gives up, counting nothing in; `a_stopped_start_gives_up_at_the_gate`); notice titles carry peers' names and some servers (dunst) read markup there too (their markup characters become look-alikes); two notices on one id would leave one that can't be closed (each id once, whole); a panic in the capture's thread would have left the gate counting it (a guard counts it out); our links were destroyed twice (dropped once now). It asked whether a program reopening its stream while we capture hits the freeze too: `a_program_opening_its_stream_again_is_heard_again` (a game changing scenes) passed three times, the program still sounding at the output: its new stream has new ports.

## Part 5: remote play's home PC on Linux (0.26.0, wire revision 14)

**A Linux PC is a home PC** (`Backend::home_pc` true on Linux): Settings → Remote play offers "This PC" there, and a device of the user's own runs its sessions from afar as on Windows, controller seats only (desktop control waits for M16c-2). M25c's unattended PC, the Linux way (ADR 0012):

- **Awake** (`couch_platform_linux::awake`): while remote play is on, two requests, each enough where it is honoured: **logind's inhibitor** (`sleep`, and `idle` while a device is unlocked), held as the descriptor logind gives back, and **the desktop's inhibit portal** (`org.freedesktop.portal.Inhibit`: suspend, and idle while a device is unlocked), which GNOME's and KDE's own power settings follow (they suspend and blank the screen themselves; logind doesn't touch the screen). Taken and let go on the portals' runtime, never blocking the actor; a failure is only logged. **Found in WSL:** logind's polkit default for a process in no logind session is no (`allow_any`), so logind refused us there (and `systemd-inhibit` itself), while root's request was listed and let go; an app a desktop starts may run outside a session too (under the user's service manager), which is why the portal is beside it. Which one each desktop honours is for the VM.
- **Locked** (`couch_platform_linux::session_lock`): logind's `LockedHint` for the user's graphical session (the user's `Display`, the user found by id with `GetUser`; else this process's own session). `user/self` and `session/auto` name the caller's, which logind finds only for a process in one of its sessions: unknown in WSL. Read on the portals' runtime: each look returns the last answer at once and asks for a fresh one, so the actor's 2 s check never waits on the bus and is one look behind at most.
- **Start at sign-in** (`couch_platform_linux::autostart`): an XDG autostart entry, `$XDG_CONFIG_HOME/autostart/couch-co-app.desktop`, naming the AppImage (`$APPIMAGE`, counted only when this exe runs inside its mount) with `--background`, for an AppImage's default profile in a release build (`EngineOptions::start_at_sign_in`). The path is quoted and escaped as the Desktop Entry Specification says (a path with a control character is refused). Ours by our key in it: another program's file of that name is never read as set, removed or rewritten; ours is replaced by any AppImage turning the switch on, and removed by any turning it off (an older AppImage beside a newer must not go on starting). **An AppImage's name carries its version** (the author's choice): at start, an entry of ours naming an AppImage that's gone is pointed at the one running (`Backend::keep_start_at_sign_in`); one still there is left alone (it may be the one the user runs). No notification-area icon, so a start at sign-in opens the window, and closing it quits.

**Streams from afar** (the author's choice, 2026-11-03): nobody is at the PC to answer the screen-sharing dialog. A command from a device is marked as such (`Actor::command_from`). Its stream:
- **is refused without a remembered choice** (`Backend::shares_unattended`: the portal's restore token), before anything starts, the device told to share a screen at the PC once (`RemoteResult::Failed`, no wire change);
- **with one, gives the dialog 30 s** (`CaptureRequest::unattended`, `UNANSWERED_AFTER`): a dialog that comes up anyway (the remembered window gone) is closed, and the stream ends as **`EndReason::NothingShared`** (wire revision 14, IPC v15), which the device's console, the viewers and the stream window's bar say. A dialog dismissed at the PC ends the same way now, rather than as an error.

And also:
- **A session from afar binds the host's keys unattended** (found by the review): the global-shortcuts portal may show its own dialog the first time; `Backend::host_keys` takes `unattended`, and that dialog too is closed after 30 s, the console then saying the keys aren't available (its own buttons, Take back every seat among them, still work).
- **The remembered choice is spent in the file too** when a start takes it (`portal.json` emptied), as the portal spends it: a start that fails before the next token comes leaves nothing remembered, rather than a token that would let a start from afar bring the dialog up again after each restart.

**The UI:** Settings → Remote play on Linux has no notification-area switch, "Start Couch Co-App when I sign in" (offered only by the AppImage), and a note on the sharing dialog; a device's words for a locked PC no longer name Windows. A `linux-home-pc` preview scenario and its shot.

**Checked**
- **A session scenario,** `a_stream_from_afar_needs_a_choice_the_pc_remembers` (84 in all): a device's stream refused with nothing remembered and no capture tried, the PC's own start allowed and not unattended, then from afar with a choice remembered (the capture marked unattended), then a dialog nobody answers ending the stream as `NothingShared`, on the device and at the PC; and the session from afar binding the host's keys unattended.
- **Linux (WSL), against the real logind:** `session_lock::tests::the_lock_screen_reads` as the user (the lookup through `GetUser`; `user/self` and `session/auto` were unknown objects there, which is how it was found); `awake::tests::logind_lists_us_while_held` as root (`systemd-inhibit --list` shows the inhibitor while held, and not after the guard drops; as the user polkit refuses it, `systemd-inhibit` itself too). The autostart entry's tests: quoting round trips (quotes, `$`, backquotes, `%`, backslashes, spaces), paths it can't hold refused, only our key counts, on and off, an edited entry of ours, another program's file left alone, a newer AppImage taking over one whose file is gone and not one still there.
- **Linux (WSL):** fmt, clippy, the workspace's tests (675 passed; two rooms scenarios on invite expiry, `revoked_and_expired_invites_stop_working` and `an_opened_invite_is_previewed_before_joining`, failed once under the full parallel run, as in part 4: WSL's wall clock), then the rooms and sessions scenarios three runs in a row (27 and 84 each), before the review's fixes and again after.
- **Windows:** fmt, clippy, the workspace's tests, the rooms and sessions scenarios three runs in a row before and after the review's fixes, tsc, vitest, and `npm run shots` for Remote play's settings (the new `settings-remote-linux` among them): no problems, nothing jumps.
- **Not here:** the inhibit portal, the shortcuts and sharing dialogs, a real lock screen and an autostart at sign-in: no desktop session in WSL. They are in the VM list below.

**The review** (a fresh agent over the change) found no high-severity problem; checked against the code:
- **A session from afar could bring up the global-shortcuts dialog** for nobody (medium), against the rule this change adds: the bind now gives up after 30 s when unattended (above).
- **A request that never answered kept the other** (medium): `hold` waited for both logind and the portal before watching its guard, and zbus calls have no time limit of their own, so a stuck portal would have kept logind's inhibitor, and the PC awake, for good. Each request now has 10 s.
- **A choice made as the 30 s ran out could lose its token** (low): the select now prefers the choice. **A spent token came back from `portal.json` after a restart** (low): spent in the file too now. **An older AppImage could go on starting at sign-in, and an edited entry of ours was nobody's** (low): ours by the key alone, removed by any AppImage turning the switch off. **"Can't be kept awake" at every unlock** (low): each failure said once. **The shell's own `own_appimage`** lacked the absolute-path check: added.
- **Left as they are:** the host console's words for nothing shared say what a stream from afar needs even when the user at the PC dismissed the dialog (they say it as a condition); at an unlock after a long time with no device, the first lock state sent may be one look old, corrected 2 s later; a `.desktop.new` file left if a rename fails.

## Still to come

- **In the VM, for part 5:** remote play turned on at a Linux PC on GNOME and KDE, which of logind and the inhibit portal each honours, the PC neither suspending nor blanking with a device unlocked; its lock screen told to the device; a stream from afar refused with nothing remembered, then shared without a dialog, and a dialog for a gone window closed after 30 s; the shortcuts dialog on a session from afar closed after 30 s; start at sign-in from the AppImage, and after a newer one replaces it (the `linux-host-vm-run` skill's rows).
- **The items "In the VM" below** have their run in the `linux-host-vm-run` skill (a VirtualBox VM, or a Linux PC or the Deck).
- **In the VM, for part 4:** the chime and a stream window on the same PC not heard by a Windows guest (WebKit's processes are the app's children; a sandboxed web process would report another process id), a game picked by name (a native one, and one under Proton), notifications on GNOME and KDE (a request staying until answered, replaced when its keys come, closed when the app quits, none while the app is in front), and whether either desktop plays a sound for them anyway; a loopback or EasyEffects not doubling a game's sound, and programs on two outputs (speakers and a headset) both taken without xruns on either (linking them into one node joins their drivers); a 5.1 game's mix (sides, rears and centre are summed into stereo at full scale, which can clip: the monitor's channel mixer scaled them).
- **In the VM, for part 3:** the portal's dialog on GNOME and KDE, the choice remembered across streams and runs, a window shared and closed (the stream ends, the session goes on), a share stopped from the desktop's indicator, Stop streaming with the dialog open, a Windows guest watching and playing with a controller, the PC's sound.
- **In the VM:** the portal's keys on KDE and GNOME (GNOME may want the app registered with the portal, as a sandboxed app is), over two Go lives in a row, with the console showing the desktop's names for them (whether a desktop gives an empty name for a key it bound, which counts as unbound, and how KDE takes Shift with a digit or symbol); a guest's pad driving a game and its rumble; Set up.
- **VAAPI on real hardware:** the Steam Deck (AMD).
