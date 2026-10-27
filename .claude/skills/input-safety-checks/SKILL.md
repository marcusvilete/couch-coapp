---
name: input-safety-checks
description: Verify that Couch Co-App's remote input stays safe after a change, with `couch-cli release-check`, `input-sink`, `lifecycle-check` and the router tests: the host must release a viewer's keys, buttons and pad whenever a session ends badly, and inject only what the seat router allows. Use after changing couch_input (router, forward, filter), couch_net::input or datagrams, couch-viewer's input, pipe or shutdown code, couch_engine::pipeline (host, watch), the engine's rooms::sessions, couch-cli's host/view/host-pad, couch_gamepad, couch-platform-win's sendinput, held_input, hidhide, markers, keymap, vigem, foreground, game or host_keys code, the desktop app's host overlay (overlay/useOverlay.ts, its window in platform/tauri.ts), couch_engine::pipeline::source, couch_input::shortcuts, or anything on the viewer/host teardown path; and before reporting or committing such a change.
---

# Input safety checks

**What the checks protect.**
- Remote input is remote control of someone's PC.
- Windows keeps an injected key down until something releases it, even after the injecting process exits. So any path that ends a session without releasing leaves a key or button held on the host.

**The rules live in `couch_input::router::SeatRouter`** (docs/protocol.md, "Input"). Its tests are the first guard. Never weaken or delete one to make a change pass. If a rule seems wrong, say so and ask the user.

**Several seats** (P1). Each seat has its own holder, epoch and pad; a pad effect goes only to its seat's pad (`router::apply`, pads by seat, `None` for a seat without one). Two rules beyond the router's per-seat ones:
- **Keyboard and mouse on one seat at a time.** `SeatRouter::grant` refuses a grant with keyboard or mouse while another seat's holder has them (`only_one_holder_at_a_time_has_keyboard_or_mouse`).
- **Seats per viewer by its controllers** (M18). A viewer holds at most one pad seat per controller it said it has (`ViewerControllers`; 0 until told), plus the keyboard-and-mouse seat. `HostSession::keeps` decides add or move, counting grants on their way (kept in the order sent, each answered only by a report for its own member: keyed by seat alone, a report for one viewer once cleared another's grant, the review's finding); a move revokes the viewer from every other pad seat before the grant, on the same ordered channel, and keeps its keyboard-and-mouse seat. Never reorder those effects. The session property test checks that the router never sees a viewer on more pad seats than the most controllers it said (one if none), nor on two seats without a pad, nor the host on two, even with reports arriving late. Its ops are weighted so viewers do reach two seats and two pad seats; if you change the ops, check it still does (a temporary `prop_assert!` that fails on two seats must fail).
- **The host's choice of a guest's controller** (P12). A grant may name one of the guest's controllers (`SeatAssigned::controller`), and `SeatController` moves a held seat to another; the forwarder pins it while it is connected (a seat it drove swaps) and reports what it decided (`ViewerControllers`), which the host only shows. Neither touches the router: no seat changes hands by them. Tests: `the_hosts_choice_of_controller_drives_the_seat`, `the_host_moves_a_seat_to_another_controller`, `a_choice_that_cant_be_followed_falls_back`; scenario `the_host_picks_which_of_a_guests_controllers_plays`.
- **The guest's side keeps a hold per seat** (`couch_input::forward::Forwarder`): each seat its own epoch, pad sequence and heartbeat; a seat no controller drives sends neutral, so it isn't suspended. Keys and mouse go only to the seat that allows them. The forwarder alone decides which controller drives which seat (pinned by slot with several pad seats) and tells the window. A controller that goes, with several pad seats, makes its seat neutral at once and gives it back; it must never move another controller to it, and a restarted window must give nothing back. The forwarder's tests: `two_seats_are_driven_by_two_controllers`, `a_controller_that_goes_leaves_its_seat_and_the_others_stay`, `one_seat_stays_when_its_controller_goes`, `a_controller_connecting_moves_nobody`, `a_replaced_viewer_gives_no_seat_back`, `keys_go_to_the_keyboard_seat_beside_a_pad_seat`, `one_member_drives_two_pads_end_to_end`.
- **Every release covers every seat:** the panic key, a viewer leaving or closing its window (`SeatRouter::disconnected`), a stream stop (the hold) and the session's end.

The session scenarios `two_guests_play_two_pads_at_once`, `moving_to_another_seat_lets_go_of_the_old_one_first`, `a_keyboard_seat_types_and_never_drives_a_pad`, and for M18 `a_guest_with_two_controllers_plays_two_seats_and_the_keyboard`, `a_seat_passed_is_added_or_moves_by_the_guests_controllers` and `a_guests_seats_survive_a_stream_stop_held_off_and_end_with_the_session` cover it end to end with recording pads.

**The host on seats** (P2; M23: one seat per controller of theirs). The host's own controllers are one more holder in the same router, never a shortcut around it:
- **Through the router.** The input task wraps each controller's states (`Inputs::local_input`, with the controller's index) as pad datagrams for that controller's own seat, with its current epoch, under the host's member id, and only while the router has the host on it. Granted the pad permission only. A controller whose seat the host doesn't hold sends nothing.
- **Which controller drives which seat is the session's word** (`HostSession`'s homes, `Effect::HostSeats` → `InputCmd::HostSeats`), sent before the claims and grants it concerns on the same ordered channel. Never derive it from the router's holders or from order. Moving a controller revokes its old seat whatever the mirror says; onto another of the host's seats, the two swap (holders unchanged).
- **Claims, not grants, bring a controller's seat back** (`HostSession::reclaim`, `InputCmd::Claim`): the router takes one only if the seat is free, it is one of the host's controllers' seats and control isn't paused, and only from the host. A plain grant could take a seat a viewer was just passed. The session property test runs with no host, one controller and two (half the cases without, so viewers still reach two pad seats), its router model taking claims only for free seats, and checks the router never sees the host on more seats than controllers (nor on a keyboard-only seat), that claims go only to a controller's own seat never left empty, that the session's homes match a model of the host's moves, and that each controller ends back on its seat unless a viewer has it, it was left empty, or control is paused. It must keep reaching the host on two seats (a temporary `prop_assert!(!host_held_two)` must fail).
- **A host controller that disconnects keeps its seat** (M23, the user's choice): the reader reports it neutral every 100 ms, so the router never suspends it.
- **Rumble** goes to the controller driving the seat now; after a swap, the one that drove it stops.
- **The panic key doesn't pause the host:** it frees only viewers' seats (`InputCmd::RevokeViewers`, decided in the input task from the router's own holders, never from the session's mirror, which may lag), and the pipeline refuses paused grants to viewers only. Taking every seat back uses the same command.
- **After stopping** (`ReleaseAll`), no grant or claim goes through.

**Sessions before streams** (M19). A session can stream nothing for a while, and its stream stops and starts again; viewers and their seats stay through it. One more router rule:
- **While nothing is streamed, viewers' input is held off** (`SeatRouter::hold(Some(host))`, set by the input task from the session's start and by `stop_stream`): holding releases at once every key, button and pad a viewer held; their input is then dropped but still counts as heard (no suspension, no seat lost after `SUSPEND_LIMIT`), and mouse totals stay current (no pointer jump when it lets go). The host's own controller, the exception, plays on. `start_stream` lets it go (`hold(None)`). Router tests: `holding_releases_and_drops_a_guests_input_but_keeps_the_seat`, `a_held_seat_is_never_suspended_while_its_holder_is_heard`, `motion_while_held_moves_nothing_later`, `the_hosts_own_controller_plays_through_a_hold`.
- **Stopping a stream releases first:** the hold (acknowledged), then the epochs end with `STREAM_END`, then encoders, capture and sound. Scenario: `between_streams_a_guests_input_is_released_and_held_off` (a held key and pad released at Stop streaming, nothing in until the next stream, then the guest's input again), and `the_hosts_controller_plays_between_streams`.
- **Keyboard and mouse still need the streamed window in front:** with no stream there is no window, so nothing of the keyboard's reaches anything, held or not.
- **The hidden controller** (`couch_platform_win::hidhide`) is shown again after the pads are unplugged (the input task drops its guard last), and `restore_stale` in `Backend::release_left_input` undoes what a killed host left (its marker in `%LOCALAPPDATA%\CouchCoApp\hidden-controllers\`).

Scenarios: `the_hosts_controller_plays_a_seat_and_gets_it_back`, `the_host_plays_another_seat_and_feels_its_rumble`, `a_controller_that_cant_be_hidden_plays_outside_the_seats`, and for M23 `the_hosts_two_controllers_play_two_seats` (each on its pad, one passed and taken back, Play here, a swap, rumble to the controller driving the seat, the panic key keeping both, both shown again), with fake controllers (a DualSense and a GameSir, each a thread reporting every 100 ms, like the real reader) and a fake hiding guard counting what is hidden.

**Pads Windows is still setting up** (P10). A pad not taking input within 3 s is waited for in the background (`pipeline::host::set_up_pads`), up to 5 minutes or Stop waiting; its seat can be held meanwhile and drops gamepad input (the router's `None` pad), as for a seat whose pad failed. Its marker carries its serial from the plug on. Ending gives the waiting pads up and joins the waiting task after the host's controllers stop being read and before the hiding guard drops. Scenarios: `a_pad_windows_sets_up_late_takes_its_seat_when_ready`, `stopping_the_wait_unplugs_the_pads_still_being_set_up`, `ending_the_session_gives_up_a_pad_still_being_set_up`, `a_pad_not_set_up_in_time_is_given_up`.

**Editable shortcuts** (P3). The user moves shortcuts to other keys (Settings; `shortcuts.json`), never their modifiers. `couch_input::shortcuts::Shortcuts` is the only place that checks a key, for the app, the engine and the viewer alike:
- **Only keys in `KEYS`**, each with a plain, non-extended scancode the host-keys hook can match (`every_allowed_shortcut_key_has_a_plain_scancode` in `host_keys.rs`); never Esc, Tab, Delete, F4, Enter, Space, the modifiers, the Windows keys, Pause, Print Screen or the numpad, nor the gray navigation keys (arrows, Insert, Home, End, Page Up and Down: depending on NumLock, keyboards wrap them in fake Shift events the hook takes for the real Shift, so a chord there might never fire). The row that works without the chord takes only F-keys. Keep these rules when adding keys; `risky_keys_are_refused` is the guard.
- **No two rows on one key, the panic key included**, and the panic key can't be removed (`two_rows_never_share_a_key`). A file that breaks a rule loses only the offending rows (`Shortcuts::with_keys`).
- **The host's keys are read at a session's start** (`Backend::host_keys(keys, …)`) and the viewer's keys reach each window right after `HELLO` (IPC `SHORTCUTS`). `moved_shortcuts_reach_the_hook_and_the_stream_window` covers both with fakes. Shortcuts are still never forwarded: the viewer's check is unchanged, it only reads the table in use.

**Answering requests with keys** (M8). Accept and Decline (Ctrl+Alt+Shift+Y and N by default) share the panic key's hook, `couch_platform_win::host_keys`: one hook, a chord state per key, the same physical-only modifier tracking. So a guest holding the keyboard seat can't accept their own request by typing the chord (`injected_answer_keys_never_count`). The keys answer only the oldest request, and only once it has been pending for `ANSWER_AFTER` (1 s), so a press never answers a request the host hasn't seen (`the_hosts_keys_answer_the_oldest_request`). The host overlay that shows them must never take the focus from the game, or guests' keyboard and mouse stop: it is created unfocused and unfocusable and closed when empty, never hidden and shown again.

## Run

```powershell
. .\scripts\dev.ps1
Get-Process couch-* -ErrorAction SilentlyContinue | Stop-Process -Force
cargo test -p couch-input -p couch-net        # router, forwarder, filter; input over loopback
cargo test -p couch-room session              # the host's session machine and its property test
cargo test -p couch-engine --test sessions    # handoffs, panic key, viewers leaving, all with a recording pad
cargo build --release -p couch-cli -p couch-viewer
.\target\release\couch-cli.exe release-check  # about 1 minute
.\target\release\couch-cli.exe input-sink     # a few seconds
.\target\release\couch-cli.exe lifecycle-check  # about 1 minute
```

- **Tell the user first.** `release-check` and `lifecycle-check` start several hosts, each plugging a virtual pad (a Steam notification each), and open windows that take the focus.

- **Close leftovers first.** A running `couch-cli.exe` blocks the release build ("Access is denied"), and a stray viewer confuses the checks.
- **Leave the machine alone while they run.** Both commands open windows. `input-sink` types into its own window, and stops if that window loses the foreground. `release-check` starts and ends viewer windows. Don't click around.
- **Run these from a console.** `release-check` sends Ctrl+Break to a host it started in its own process group, so it needs a console it shares with that host. The PowerShell tool qualifies.
- **Exit code** 0 means everything passed.

## What `release-check` does

Each scenario starts a host (test pattern) and a viewer, waits for the seat to go active, does one thing, and times the host's `EVENT` line against that moment. Both sides read the same QPC clock.

| Scenario | Expected release | Limit | Typical |
|---|---|---|---|
| Viewer's main process (`view`) killed | `suspended`: nothing tells the host, so its 300 ms dead-man timer fires | 350 ms | 250 to 300 ms |
| Viewer window process killed | `gone`: `view` closes the connection | 350 ms | 70 to 120 ms |
| Viewer window closed (WM_CLOSE, as its X button sends) | `gone`, and the viewer process exits | 350 ms | 130 to 200 ms |
| Viewer window process frozen | `suspended`: `view` stops the heartbeat after 250 ms without the viewer's `Alive`, then the dead-man fires. After thawing, the seat must go active again | 600 ms | 350 to 400 ms |
| Host stopped (Ctrl+Break) | `stopped`, and the host exits | 500 ms | under 5 ms |

Ctrl+C and closing the host's console run the same code path as Ctrl+Break. Ctrl+C was verified by hand in M2.

**If a scenario fails**
- **No release at all:** some path ends without calling the router's release, or without reaching it.
  - Check the exits of the input task, `input_loop` in `couch_engine::pipeline::host`: it must release on every one, including when the `HostStream` is dropped.
  - Check the viewer's shutdown: it must send `Outgoing::Close`, not wait for its channels to close.
  - Check `Forwarder::tick`'s liveness gate.
- **Late:** look at the timer constants first:
  - `router::DEAD_MAN`;
  - `forward::{PAD_REFRESH, VIEWER_QUIET}`;
  - the viewer's `ALIVE_EVERY`;
  - `couch_engine::pipeline::host`'s `INPUT_TICK`.

  Their sum sets each limit above. Change a limit only with the user's agreement.
- **Error before acting:** usually leftovers, or the seat never went active (a slow ViGEm plug can take seconds).

## What `input-sink` and `lifecycle-check` do

- **`input-sink`** plays a guest holding the seat: every key, button, move and wheel turn goes through a `SeatRouter` into the `SendInput` injector, into `couch-viewer --input-sink` as the game, and must arrive in order. Then the blocked combinations (Windows, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc, Alt+F4) must not, beyond their modifiers. A Windows key that got through would open Start and take the focus, which stops the run.
- **`lifecycle-check`**, times on QPC:

| Scenario | Expected | Limit |
|---|---|---|
| `fake-game` swaps its window | host `followed` | 1 s |
| `fake-game` exits | host `game-exited` | 3 s |
| Its window closes, the process runs on | host `source-lost`, after the grace | 5 to 6 s |
| The stream window (`view --restart-viewer`) killed | `view` `viewer-restarted`; the seat active again if it was suspended | 2 s |
| Host killed | `view` reports `connection-lost` (no `Alive` for 5 s) | 5.5 s |
| A helper holding F24 through `SendInputInjector` is killed | F24 stays down; the next `host` start releases it | 1 s after start |

  - The last one checks that F24 really was left down first ("nothing to show" otherwise), and lets go of F24 whatever happens.
  - A stale-key failure means the held-input marker (`couch_platform_win::held_input`) wasn't written, read or acted on: check `%LOCALAPPDATA%\CouchCoApp\held-input\`.

## What isn't covered automatically; ask the user

- **`pad-test`:** run it and watch `joy.cpl` (Game Controllers → the Xbox 360 controller → Properties). Needs ViGEmBus. Each run plugs a pad, and Steam announces it: say so before running it repeatedly.
  - `--count 2` plugs two: each must show in its own slot (pad 2 a button ahead of pad 1), and the printed count of game controllers must rise by two (`couch_platform_win::controllers`, which the session dialog's warning about a controller plugged into the host reads).
  - Several real pads end to end: `couch-cli host --pads 2` and two `view`s; `xinput-watch.ps1` must show each viewer's input only in its own slot.
- **A guest's pad reaches the host:** with a session live and a guest holding the controller, `.\scripts\xinput-watch.ps1` on the host must show every button and stick under the right name in the virtual pad's slot. That is the check for our side; a game ignoring the pad on one PC is usually the game binding to the guest's physical controller (the `app-manual-run` skill).
- **A real game:**
  - put it in windowed mode and turn off Steam Input for it;
  - run the host on the game window and the viewer;
  - with the game focused, the pad should drive it, and rumble lines should appear in both the host and `view` output;
  - on one PC the game may pick the physical controller instead; only two PCs settle it.
- **Pad identity.** The app's virtual pad must report a real Xbox 360 controller's ids (`real_pad_ids: true` in `rooms::sessions`): with made-up ids, games and Steam treat it as a generic joystick and only the D-pad works ("XInput Controller #1" in Steam's notification). So ids can't tell our pads from a real Xbox 360 controller or a remapper's pad; their origin does (P9, `couch_platform_win::pad_origin`): the device under ViGEmBus whose serial a host's marker names (`virtual_pads`). A viewer on the same PC skips exactly the hosts' pads, and any Xbox 360 pad whose origin can't be told; the host's reader lists physical controllers, and ones of unknown origin without our ids (`pad_origin::may_be_the_hosts`); hiding by ids takes the same, and never a node under a virtual bus. Since M22 the reader also lists another program's virtual pad proven so (a remapper's), hidden by its device alone (`hidhide::hide_virtual`, which checks the origin again), never ours: a host reading its own seat pad back would loop. So a host knows its own pads in memory too, beside their files (`virtual_pads`), and markers it can't read mean any ViGEm pad may be a host's. Keep "unknown counts as ours by ids", and the marker written before the plug with its serial added after: a lost marker or a misread origin would let a viewer here read the host's pad and send it back. After touching `vigem.rs`, `virtual_pads.rs`, `pad_origin.rs`, `xinput_slots.rs`, `device_tree.rs` or the viewer's `open_pad`:
  - run `couch-cli pad-origins --ours 1 --unmarked 1 --hold 15`, and meanwhile from other shells `couch-viewer --probe` (the host's pad skipped, the unmarked one and physical controllers read) and `couch-cli host-pad` (the physical controllers, and the unmarked pad as another program's, never ours); each plugs pads, so say so first;
  - check the markers in `%LOCALAPPDATA%\CouchCoApp\virtual-pads\` are gone once the pads are unplugged.
- **Keyboard and mouse end to end:** needs a second PC or Windows Sandbox. On one machine the viewer holds the focus, so the router correctly keeps keyboard and mouse away from the game.
- **The panic key** (Ctrl+Alt+Shift+Backspace by default; Settings can move it) ignores injected keys by design, so only a person can press it. After touching `couch_platform_win::host_keys`, the shortcuts table or the hook's wiring in `rooms::sessions`, ask the user to stream from the app with a guest playing and press it on the physical keyboard, once with the game in front and once with the host's own app window in front. The guest's pad must go neutral at once, the host panel must show "control paused", and the game must not see the key. After touching the shortcut editor, also ask them to move it (say to P), start a session again, and check the new key works and the old one does nothing.
  - The `Chord` unit tests cover the logic, including a guest's injected modifier key-ups (which clear Windows' shared down bit) not blocking it, and a Backspace held before the chord staying the game's. `the_panic_key_takes_control_back_until_allowed` covers the wiring with a fake hook, and every end path checks the hook goes.
  - Never log or block inside the hook callback: every key on the PC waits for it.
- **The user's DualSense going silent in `joy.cpl`** while SDL or Steam holds it in enhanced mode is normal.
- **The host's own controller and HidHide** (P2; needs HidHide installed; since P9 an Xbox 360 controller works too, and since M22 a remapper's pad, below; ours never). After touching `couch_gamepad`, `hidhide.rs`, `controllers.rs` or the host pipeline's local input:
  - `couch-cli host-pad` lists the controller, never our pads; run it while `pad-test --count 2` runs to check our pads stay unlisted.
  - `couch-cli host-pad --feed 1 --secs 300` with `joy.cpl` and `xinput-watch.ps1` open: after the user reconnects the controller once, `joy.cpl` (closed and reopened) lists only "Xbox 360 Controller", which moves with it. At the end the controller is back and the marker is gone.
  - Kill a feed with `Stop-Process -Force`: the marker stays and `joy.cpl` lists no controller; the next `couch-cli host-pad` prints "showed 1 controller(s) a crashed host had left hidden", and the controller is back.
  - If the controller stays hidden for good: HidHide's configuration client (Start menu) shows the blacklist; remove its entries there, or start `couch-cli host-pad` once.
- **A remapper's pad as the host's controller** (M22; HidHide, and a remapper or a stand-in). It is hidden by its device and kept hidden while it is gone, because the remapper plugs it again under the same device id and a game polling for pads opens a pad hidden only after it appears (docs/m22-results.md); the entry comes off once the device could be something else's (`pipeline::virtual_hiding::keeps_hidden`, and `a_remappers_pad_plays_a_seat_hidden_by_its_device`). After touching `virtual_hiding.rs`, `hide_virtual` or the reader's kinds:
  - `couch-cli remapper-probe --unmarked 1 --hide --sticky --secs 25` (a stand-in, one Steam notification), and from another PowerShell `xinput-watch.ps1`: the stand-in's slot reads "not connected" there while the probe still lists it; the blacklist is empty at the end. With the user's DS4Windows: toggle its "Hide DS4 controller" and reconnect the DualSense during the run; `xinput-watch.ps1`, started before, must never show the pad. Stop the probe in a plain console (Ctrl+C there ends it cleanly; piped, PowerShell may kill it, and `couch-cli host-pad` then shows the pad again).
  - `couch-cli remapper-hop` (a stand-in; remappers closed): the hop from the pad to the host's reader, about 1 ms (M22).
  - On one PC a guest's stream window may read the physical controller behind the remapper too: one press then drives the host's seat and the guest's. Not a fault; two PCs never do.

## After the checks

- Report the `release-check` table and any change from the typical values.
- If behaviour changed on purpose, update `docs/protocol.md` ("Input") and the milestone's results file.
