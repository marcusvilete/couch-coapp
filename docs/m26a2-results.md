# M26a results, part 2: saying why

## Why

M26a-1 let a profile hold several relays, and made the app say what is wrong with its own connection. The rest of [ADR 0014](adr/0014-public-builds-and-registered-relays.md)'s "Showing what's wrong" was still to come. With relays registered by the user, a room has new ways to stop working, and until now the app showed only "offline" or "couldn't reach the room's owner". A member could be on a relay we have no code for. A join could fail at the relay rather than at the owner. An owner's links could point at a relay the owner left. A stream could freeze without a word. This part says, for each, what happened, on whose side, and the one thing to do. These are hints, never verdicts: a direct path may still connect, and nothing is refused for them.

**Your choices (2026-11-13):**
- one PR, 0.36.0, the session part included;
- telling "needs a code" from "not answering" for a relay we don't hold by connecting to it without a code now and then: yes;
- stale links: a side file for which relay each open invite named, and a callout in the room and the invite dialog (no room file format bump);
- two hints in the front end, from M26a-1's hand testing:
  - a line at the foot of Create a room and Invite while only the public relays are in use;
  - a Details beside a "personal" code.

## What changed

**Why a member can't be reached** (`couch_engine::reasons`, the actor's `refresh_reasons`)
- A member we can't reach gets a reason, `MemberView.unreachable`, only once a dial to them failed, and only while not connected. The reasons are worked out again on the actor's tick, when a dial, a connection or the network's state changes (`Network::generation`).
- The reasons, in order:
  - our own connection (no internet, a sign-in page, none of our relays answering);
  - their relay is one we won't dial (release rules);
  - their relay is one of ours, and it refuses our code or isn't answering;
  - their relay is one we don't hold, and it wants a code or doesn't answer.
  - Otherwise nothing: they may simply be away.
- **How we tell for a relay we don't hold:** we probe it the way M26a-1 checks a code before saving it, through a throwaway endpoint with a random key, but with no code (`network::Probe`, `due_probes`). We never probe our own relays or iroh's public ones (open to all), only addresses the dial rules allow, and not while the network is down. Each relay at most every 10 minutes; one that didn't answer, every 2.
- **In the app:**
  - the member list adds a few words ("offline · needs a code") and a warning icon, whose tooltip holds the full sentence with the relay's name;
  - the member's menu offers "Paste a code for relay.example.com…" where a code would fix it;
  - a room where nobody else can be reached, all for one cause, says it once at the top, with Paste a code. Our own connection is left to the shell's strip.
- **Copy diagnostics** lists each member's reason, by relay name, with addresses scrubbed.
- **Found while building:** a newcomer's first record named no relay when its home relay had only just come up. Its address was refreshed every 2 s, not as it joined. `joined` now refreshes it first, or a member who joined and left at once was known by no relay.

**Joining** (`reasons::invite_relay`, `check_invite_relay`, `join_problem`)
- **Before the password:** the preview names the invite's relay (`InvitePreview.relay`). One we hold or a public one says nothing. For any other, `Command::CheckInviteRelay` probes it, and the dialog says "This room's owner uses relay.example.com, which needs a code: ask them for it. You can still try.", with Paste a code. The row is reserved, so nothing below it moves.
- **A join that fails says which step did:** for a dial that never reached the owner (`Unreachable`, `NotReached`), the engine connects to the invite's relay afterwards, with our code if we hold it. The outcomes:
  - our own connection;
  - the relay not answering;
  - the relay refusing our code;
  - the relay wanting a code we lack;
  - the relay answering, but the owner not on it ("they may be offline, or the link may be old").

  The answer is `Response::JoinProblem { problem }`. The owner's own refusals (password, expiry, ban) are said as before. An invite naming addresses only stays "owner offline".
- Pasting a code from the join dialog checks the relay again, and the join can go ahead at once.
- `EngineOptions::join_connect` (15 s) makes the connect timeout an option, so the tests wait 6 s.

**Links gone stale** (`store::InviteRelays`, `invite-relays.json`, `Room::open_invites`)
- An owner's app remembers which relay each open invite named, by the invite's tag (a hash of its token, never the token). It's pruned to open invites of rooms it owns, and never backed up, like the invites themselves.
- `RoomView.stale_invites` counts the open links naming a relay other than the one we're reachable through now. The room says so in a callout, with New link…, and so does the invite dialog. Revoking the links clears it.

**Sessions** (viewer IPC v21, the `protocol-change` skill; no wire change)
- `HudState.link` is `Direct`, `Relay(host)` or `Reconnecting(why?)`.
- The stream window shows a card while reconnecting: "Reconnecting to Ana…", then the reason when the app can tell ("relay.example.com stopped answering."). It no longer sits on a frozen picture with a note that fades after a few seconds.
- The stats card's Path row names the relay a picture comes through.
- The host's console names each viewer's relay (`SessionView.viewer_relays`), in the patch bay and in a session to watch together.
- The viewer's preview has a `reconnecting` state (`viewer-shots.ps1`).

**The hints**
- Create a room and Invite end with "Your connections go through the public relays. Settings → Network adds a relay of your own." while no relay of the user's own is held.
- A personal code shows Details beside its badge, in Settings → Network and in a pasted code's preview: "Given to you alone, so the relay's owner can take it back without changing everyone's code. It isn't shared, here or with your rooms."

## Checked

- `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings` and `cargo test --workspace` on Windows, and on Linux through WSL, before and after the review's fixes.
- The rooms and sessions suites 3 times on Windows, and twice more after the review's fixes (31 and 98 tests each time, none failing).
- `bash scripts/check-docs.sh`: nothing stale.
- **New scenarios through real relays** (`couch-testkit`'s in-process iroh-relay):
  - `rooms`' `a_member_on_a_relay_wanting_a_code_we_lack_is_said_so`: Ana is on an open relay and Bia on one with a code. With Bia away, Ana is told Bia's relay needs a code, and once Ana adds the code there's nothing to say;
  - `a_join_says_which_step_failed`: the preview, the check before the password, a join failing at the relay, the join working with the code, and the relay answering without the owner on it;
  - `links_naming_a_relay_the_owner_left_are_marked`;
  - `sessions`' watching-resumes scenario now checks that the window's path is direct again, and that the console lists a path per viewer.
- **Unit tests:** `reasons` (each reason, probes kept only while wanted, never our relays or the public ones), diagnostics (reasons named, addresses scrubbed), the viewer's centre card when reconnecting, and the IPC round-trip with `link`. In the front end, `model/reasons.test.ts`.
- **The UI:**
  - `npx tsc --noEmit` and `npx vitest run` (112 tests);
  - `npm run shots`: new scenarios `member-reasons`, `room-unreachable`, `stale-links`, `join-needs-code`, `join-owner-away` and `network-public-hint`, with shots of the member's menu, Paste a code from a room and from the join dialog, a join failing at each step, the invite dialog with stale links, the hints, and the personal code's Details;
  - a new no-jump view, "join relay row": the relay row, then a failed step, with the password field below never moving;
  - the stream window's `reconnecting` preview, looked at.

## Found while checking

- `Response::JoinProblem(JoinProblem)` would have serialized two `type` keys: both enums are tagged `type`, and the binding read `{ type: "joinProblem" } & JoinProblem`. It's a struct variant now, `JoinProblem { problem }`.
- The member list's short reason first named the relay. Long names were cut off, so the row says "needs a code" or "relay down", and the relay's name is in the tooltip.

## The review

A fresh review agent read the diff against main. All ten findings held up when checked against the code; all are fixed.

1. **Medium: the join diagnosis skipped the dial rules.** An invite naming addresses *and* a relay we won't dial (a LAN address, `localhost`) has the relay dropped from the dial. But when the dial failed, the diagnosis then connected to that relay anyway: exactly the request the release rules forbid, from a link any web page can open, with one click on Join. `join_problem` now takes the dial rules and looks at nothing they don't allow (test `a_join_never_looks_at_a_relay_the_rules_forbid`).
2. **A reached owner could be called "not on the relay".** `AdmissionError::Connection` came both from the connect step and from a connection lost after it. The connect step now has its own `NotReached`, and only it and `Unreachable` are diagnosed.
3. **Probes could run on every flap.** A member whose connection came and went had their relay's answer forgotten each time it connected, then probed again at once. Answers are now kept 10 minutes after no member needs them, and at most four probes run at once (tests `a_probe_s_answer_is_kept_while_a_member_comes_and_goes`, `at_most_four_relays_are_probed_at_once_and_none_offline`).
4. **A tight test margin.** A member's dial used a fixed 15 s timeout against the test's 20 s wait. `EngineOptions::join_connect` now times member dials too (tests: 6 s).
5. **The relay row could still make the password move.** It held two lines, and the longest text with a long relay name wraps to three. It reserves three now, and a new no-jump view, "join owner away", uses a long relay name.
6. **A reason could name a member's old relay** until the next failed dial. A merged room fact (`Effect::Changed`) now makes the reasons be worked out again.
7. **`invite-relays.json` outlived its rooms and links.** It's now pruned when a room is left or handed over, and as links expire. The address check looks at each room's stale mark every 2 s, so the callout goes when the last stale link expires.
8. **`dial_errors` kept members who left.** It now keeps only members still on a roster.
9. **The `link` field's note was in the wrong table row** of `docs/protocol.md` (a control message's); it's moved to the IPC HUD state's.
10. **The paste dialog trusted its own relay name.** A code for another relay was announced as the one asked for. The app now names the relay actually added, and says when it isn't the one needed.

The review found nothing in: secrets (probes and checks never carry our key or a token where it doesn't belong; nothing secret reaches logs, views, diagnostics or `invite-relays.json`), lock order, announce storms, the error-to-response mapping, the IPC change, the join dialog's races, and the front end's rules.

## Still to come

- **Hand checks with two PCs,** put off until two are at hand (your choice, 2026-11-13; the scenarios above cover each through real relays on loopback):
  - a member on a relay wanting a code;
  - a join to an owner whose relay you lack;
  - the reconnecting card with a relay really going down (stop the relay while watching).
- **M26b** passes codes on by themselves. Then "Getting relay.example.com's code from the room…" and "nobody here can pass it on" become possible. Until then a member's reason says to ask them for it.
- A member whose only relay changes to one nobody else holds still can't publish its new record (M26a-1's finding). The reasons show it from the others' side, as their old relay failing or wanting a code. Keeping the old relay until the record has gone out is M26b's.
- A device that can't reach its home PC saying why (ADR 0014's remote-play line) isn't done: a device's member reason covers it in the room, but the remote panel says nothing yet.
