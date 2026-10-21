# M22 results: a remapper's virtual pad as the host's controller

## Why

A host who plays through a remapper (DS4Windows, reWASD: gyro as a stick, buttons moved) couldn't choose its virtual pad as one of "Your controllers" (P9 lists physical controllers only), only the physical controller behind it, without the remaps. You asked for the remapper's pad to play a seat.

Your choices (2026-10-20):
- **Listed beside the physical controllers**, as another program's virtual pad: ticking it hides it from games and plays it through its seat.
- **The physical controller behind it:** the dialog warns that games may see it too, and points to the remapper's own hiding; no new choice.

## The probe

`couch-cli remapper-probe` watches every gamepad SDL finds; with `--hide` it hides each other program's virtual pad through HidHide (`hidhide::hide_virtual`: the pad's device and the filtered devices under it, only if its origin is another program's by the live markers), still reading it, and prints its button changes; `--sticky` keeps a hidden pad's device on the blacklist when it goes; `--rumble` shakes it every 5 s; `--unmarked N` plugs stand-ins. `.\scripts\xinput-watch.ps1` in another PowerShell (not on HidHide's whitelist) shows what games see.

**A stand-in on this PC (2026-10-20):** an unmarked ViGEm pad on slot 1, hidden 119 ms after SDL saw it; a fresh PowerShell saw slot 1 empty; shown again at the end, the blacklist empty.

**Your runs with DS4Windows and the DualSense (2026-10-20):**

| Question | Answer |
|---|---|
| Can HidHide hide DS4Windows's pad from games? | Yes: XInput's slot reads empty outside our process, and joy.cpl lists only the DualSense. Hidden 38 to 51 ms after SDL saw it |
| Do we still read it? | Yes: 1,298 button changes in one run, 1,197 and 636 in another |
| Rumble? | Reaches the DualSense through DS4Windows |
| Does DS4Windows rewrite HidHide's lists? | No: our entries stayed through its "Hide DS4 controller" toggle, which re-plugs its pad (and seems not to use HidHide's list itself) |
| When DS4Windows plugs its pad again (that toggle, the DualSense reconnecting) | **Same device id** (`USB\VID_045E&PID_028E\01`: ViGEm gives the lowest free serial). **Hidden anew after SDL saw it, a game polling XInput opens it first** (xinput-watch did, 140 ms before the hide, and kept it: HidHide refuses only new opens). **With the entry kept on the blacklist**, it was hidden from its first moment, twice: nothing reached xinput-watch |

So the session keeps a remapper pad's entry while the pad is gone. Two probe runs were killed rather than stopped (they left their marker); the next start showed the pad again (`restore_stale`), as for a crashed host.

**The extra hop** (`couch-cli remapper-hop`: an unmarked pad stands in for the remapper's, A pressed and let go 200 times at uneven intervals, each timed from the write to the host's reader): p50 1.0 ms, p95 1.7 ms, max 36.8 ms (one), none lost; after the review's fixes to the reader, the same (p50 1.03 ms, p95 1.67 ms, max 22.1 ms); after the second review's (the full lookups every 5 s instead of every second), p50 1.02 ms, p95 1.68 ms, max 8.0 ms, the stalls the second review suspected gone. What follows (the seat router, our pad, the game) is the same as for a physical controller; the remapper's own reading of its controller comes on top.

## What changed

**A remapper's pad is one of "Your controllers"** (`couch_input::local::ControllerKind`). The host's reader lists another program's virtual pad only when it is proven so: under ViGEmBus, its serial in no host's marker, no host plugging a pad. Ours, and pads in doubt, are never listed, so the host never reads one of its own seat pads back. A choice is a kind beside the ids (`ControllerChoice`): a remapper's pad has a real Xbox 360 controller's ids. The reader looks again at pads it skipped every second while a follow has none (a pad plugged again may have had no player light yet), and at a remapper's pad it follows: every second by its serial against the hosts' markers, every 5 s in full. A follow whose remapper pad went takes back the pad under its last device, or another only if it is the one on offer, never one that was there beside its own (another player's); a pad plugged under the serial its own freed can't be told from its own coming back (ViGEm gives the lowest free serial).

**This process knows its own pads without their files** (`virtual_pads`): a host plugging a pad records it in memory too, and a markers folder that can't be read means any ViGEm pad may be a host's. Before M22 an unreadable folder read as "no hosts".

**Hiding by device** (`hidhide::hide_virtual`): only a pad proven another program's now; its device and the filtered devices under it, never by container or ids; never on HidHide's session list (cleared as a whole, and these entries come off one at a time). The rule "nothing under a virtual bus is ever hidden" became "never one of ours; another program's only when the host chose it, by its device".

**Kept hidden while gone** (`pipeline::virtual_hiding`): a thread of the session hides each device the reader follows, keeps it while the pad is gone and while it is back but not yet read again, and looks once a second: a device comes off when it is one of the hosts' own pads by its serial, when it is plugged in and no longer that follow's, or when it is gone while a host on this PC is plugging a pad (which may take its id; looked for every 100 ms while a kept device is gone, so a host plugging faster than that may find its pad hidden for a moment: two hosts on one PC only). Hiding at the start is all or nothing, as for physical controllers, and everything is shown again at the end, after the pads are unplugged.

**The dialog** lists it as "Xbox 360 Controller (a remapper's virtual pad)"; the host panel names it "(a remapper's)". With one ticked, it warns about each physical controller left unticked: games still see it unless the remapper hides it ("Hide DS4 controller" in DS4Windows), or tick it too.

No wire or IPC change.

## Code review

A review found the teardown order, the guards dropping apart, the serde default and the old remembered choices right, and seven things to fix, all fixed:
- **A marker file that couldn't be written left our own pad unmarked** (the pad was plugged anyway): it would have read as a remapper's, and a follow whose pad went could have taken it, our own seat's pad driving itself. Now this process records its pad in memory whatever happens to the file.
- **The keeper showed a pad that was back before the reader had it again** (its player light, the reader's next look: up to a second or so), which is the very race M22 closes. A follow reading nothing now keeps its last device as its own. The scenario now waits with the device back but not yet read.
- **A follow whose remapper pad went took any other remapper pad with the same ids**, a couch friend's DS4Windows pad say, and hid it mid-game. Now it never takes one that was there beside its own when that went, and takes the one under its last device first.
- **"Something else's" lumped one of ours with doubt:** another host here plugging a pad would have shown our remapper pad to games for a moment. Now proven ours (by its serial) comes off, and doubt keeps a device that is plugged in and the follow's (no other pad can have its id meanwhile). A follow also re-checks its remapper pad every second and drops it if it proves ours.
- **The once-a-second look re-examined our own seat pads**, enumerating the Xbox driver's devices each time: a pad proven ours by its serial is set aside for good (a pad SDL lists keeps its owner). And the follows now stop before our pads unplug, so an unplugging pad of ours, its marker gone, is never taken for a remapper's.
- **Any software bus counted as another program's**: should ViGEmBus ever not be recognised, our pads would have looked like that. A pad with our ids now counts as a remapper's only under ViGEmBus with a serial no host has (`pad_origin::proven_theirs`).
- **`couch-cli host-pad --feed` hid a remapper's pad before plugging its own**, which could then take that hidden id: it plugs first now.
- Smaller: stale comments (the controllers list, hiding by ids, the reader's looks).

**A second review** (you asked for one, after those high findings) verified six of the seven fixes as sound, and found:
- **The pinning only half closed**: a follow whose pad went could still take any remapper pad plugged afterwards. Now only its last device, or the one pad on offer, never a device passed over (by number or device); a pad given its freed serial can't be told from its own coming back, and the docs say so.
- **The reader's once-a-second look** enumerated the Xbox driver's devices on the thread every controller's states wait on: now by serial each second and in full every 5 s, the look-again of skipped pads only while a follow has none, and slow passes logged.
- **"Gone while a host plugs"** was looked for once a second, while plugging takes moments: now every 100 ms while a kept device is gone, the rest of the race documented.
- **`host-pad --feed`'s fix reversed the drop order on errors** (the controller shown while our pad was still plugged): the guard is declared first again.
- A device that can't be hidden is warned about once, not every second; attaching a remapper's pad looks it up in full first; the choice of pad is a function of its own, with tests; the general "a controller is plugged in" warning stays beside the remapper's when another remapper pad is left unticked; `remapper-hop` checks for other remappers before plugging.
- Left as it is: the follows' Unfollow at the session's end reaches the reader a few milliseconds later, possibly after our pads unplug; harmless (their states go nowhere, and whatever the keeper hid is undone as it drops).

## Tests

- `hidhide`: a remapper's pad hidden by its device and the filtered devices under it alone, never another pad in its container (ViGEm's may share the PC's), case ignored.
- `pad_origin`: proven a remapper's (ViGEm, a serial no host has; another software bus only without our ids), proven ours (a host's serial), and neither while a host plugs.
- `virtual_pads`: this process's pads count without their files, also when the file can't be written; markers that can't be read may name any pad, a missing folder none.
- `virtual_hiding`: when a kept device stays hidden (the follow's, plugged in or in doubt; gone with no host plugging) and when it comes off (ours, no follow's, gone while a host plugs); a follow reading nothing keeps its last device.
- `couch-engine`: `a_remappers_pad_plays_a_seat_hidden_by_its_device`, with a fake remapper pad: listed beside the physical controllers, hidden by its device as the session starts (no physical controller hidden), its input on the seat's pad, rumble back to it; plugged again under the same device, kept hidden while gone, while back but not yet read and while in doubt, and never hidden anew; gone while a host plugs, its entry off, and back under another device, that one hidden; proven ours, shown again and not hidden again, then hidden again once it is theirs; all shown at the end.
- `couch-gamepad`: which pad a follow takes (`choose`): physical controllers in the order found; a remapper's follow its own device back, the one on offer, never another player's (by number or device), never a guess between two. All 56 session scenarios, three runs in a row.
- `cargo test --workspace`, clippy, fmt, `tsc`.

## This PC (2026-10-20)

- `release-check`: viewer killed, suspended after 251 ms; its window process killed, gone after 80 ms; window closed, gone after 176 ms; frozen, suspended after 350 ms and active again after thawing; host stopped, 0 ms. All passed.
- `input-sink` and `lifecycle-check`: passed.
- Again after both reviews' fixes (the teardown order changed): `release-check` suspended after 259 ms, gone after 102 ms and 193 ms, frozen suspended after 377 ms and active again, stopped after 0 ms; `input-sink` and `lifecycle-check` passed.

## Your run (2026-10-20, this PC)

DS4Windows running with the DualSense, a session with two controller seats, DS4Windows's pad ticked as your controller:
- "Your controllers" listed it as a remapper's pad beside the physical controllers, and the dialog warned about the DualSense; the host panel named it; it played Controller 1 through DS4Windows.
- DS4Windows's pad stayed hidden through its "Hide DS4 controller" toggle and the DualSense reconnecting, and was shown again at the end.
- The game first counted 3 controllers (the two seats' pads and the DualSense); with "Hide DS4 controller" on, 2, one per seat: the warning's case.
- **One PC only:** alice's stream window had opened the DualSense itself (it reads physical controllers, P9), so with her on Controller 2 one press on the DualSense drove both seats, yours through DS4Windows and hers through her window. On two PCs a guest's window sees only their own controllers. To try a guest here, give her a controller the remapper doesn't read, with "Hide DS4 controller" on before her stream window opens.

**After the review's fixes** (the same day): DS4Windows's pad and the DualSense both ticked, two seats: both played, and the swap showed in the seats. One press on the DualSense drove both seats, its own and, through DS4Windows, the pad's: as designed, since DS4Windows reads that DualSense, and "Hide DS4 controller" changed nothing, since we hid the DualSense ourselves. The dialog now says so when a remapper's pad and a physical controller are both ticked: tick only the remapper's pad for the controller it reads.
