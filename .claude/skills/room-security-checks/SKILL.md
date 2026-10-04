---
name: room-security-checks
description: Check that Couch Co-App's rooms still keep strangers out after a change: who may join, what a peer's word is worth, what a refusal reveals, and that leaving and bans reach everyone. Runs the room simulator, admission over iroh and the three-engine scenarios, then walks a review list. Use after changing couch_room (room, signing, limits), couch_net::admission or control, couch-proto's room, admission or invite types, or couch-engine's rooms.rs, store.rs or secrets.rs; and before reporting or committing such a change.
---

# Room security checks

**What the checks protect.** A room is who gets to see and later control someone's PC. Only the owner lets people in, with an invite and the password. Everything else (members meeting, gossip, bans, leaving) must never open another way in, or leak who is in the room.

**The design** is ADR 0006; the wire rules are `docs/protocol.md` (Rooms, Admission, Invites). Two code reviews of M4a found real holes; `docs/m4a-results.md` ("Found and fixed") lists them. Most of the tests below exist because of one. Never weaken or delete one to make a change pass. If a rule seems wrong, say so and ask the user.

## Run

```powershell
. .\scripts\dev.ps1
cargo test -p couch-proto -p couch-room -p couch-net   # types, simulator, property test, admission over loopback
cargo test -p couch-engine --test rooms                 # three engines, ~20 s
```

- Run the engine scenarios **three times** after touching the actor, timings or dialing. They use real iroh on loopback, and a flaky pass hides races.
- **Through a relay** (needs internet), after touching invites, addresses or the endpoint: `cargo test -p couch-engine --test rooms -- --ignored`. It prints how B reaches A.
- `cargo clippy --workspace --all-targets -- -D warnings`, and `cd apps/desktop; npx tsc --noEmit` if engine types changed (the bindings regenerate with `cargo test -p couch-engine`).

## Where the guards are

| Rule | Enforced in | Guarded by |
|---|---|---|
| Only the owner admits, makes invites, bans | `Room::join_request`, `admit`, `create_invite`, `ban` | `only_the_owner_admits_invites_and_bans`, engine `only_the_owner_makes_invites` |
| Re-check at the end of the password check (ban, invite, room full) | `Room::admit` | `admission_is_checked_again_after_the_password` |
| At most 5 checks per invite at once, failures included | `Room::join_request` (`checking`), `join_failed`, `join_released` | `parallel_checks_cannot_outnumber_an_invites_allowance` |
| Every started check ends in the limits (admitted, failed or cancelled) | `rooms.rs`: `answer_join`, the `JoinQuery` handler, `admitted` | rate-limit unit tests, engine `many_wrong_passwords_pause_joining` |
| Only owner-signed policies and self-signed records count | `couch_room::signing`, `merge_policy`, `merge_record` | `forged_policies_and_records_are_ignored`, `a_proof_must_come_from_the_owner_and_drop_us` |
| A record counts only within its member's current stay (`since`) | `Room::current_stay` | `a_replayed_left_record_does_not_remove_a_returning_member` |
| A stranger sees no roster; a former member sees the proof | `Room::member_hello` (`former`) | `a_stranger_learns_nothing_from_a_member_hello`, `a_former_member_is_shown_why` |
| Leaving and closing reach the absent | `Leaving`, `mark_told`, `refused` | `a_member_who_leaves_alone_tells_the_room_later`, `a_closed_room_reaches_members_who_were_offline`, engine `a_member_who_leaves_alone_tells_the_room_later` and `a_closed_room_reaches_a_member_who_was_offline` |
| A dialer gets a snapshot first | `Room::connected`, `final_word` | the check in the simulator's `link` |
| Saved before sent | `Actor::apply` | none automatic: read the diff |
| Secrets never in logs | manual `Debug` on `Command`, `Response`, `RememberedRoom`, `InviteToken`, `PasswordKey` | `passwords_and_invites_stay_out_of_debug` (engine commands and replies; extend it for new ones), and read the diff for `tracing` calls |
| An opened link never joins by itself, nor replaces an invite a password was typed for | `Engine::open_link` keeps it for the UI; `App.tsx` queues it; `JoinRoomDialog` takes it only while idle and clears the password when the invite changes | engine `an_opened_invite_is_previewed_before_joining`; read the UI diff |
| One engine per identity | `lock_profile` (`engine.lock`), lowercased profile names, the single-instance plugin | `one_engine_per_profile`, `names_ignore_case` |
| Only the owner changes the password | `Actor::change_password` (`owned`) | engine `the_owner_changes_the_password` |

## Review the diff for

- **A new way onto the roster** or into `Room::conns`, other than the owner's `admit` and `member_hello` accepting a roster member.
- **Acting on a peer's word about someone else**: a close code, a presence report or a relayed record. Only a signature from the right writer counts.
- **Verification order**: cheap checks first (room, stay, version), then the signature. Never act before the signature.
- **Anything sent to a dialer that isn't yet accepted**: only the proof for a former member, and nothing for a stranger.
- **A new way for outside input to start a join or reach the network**: a link, a command-line argument, a drop. It must stop at the join screen.
- **A new effect order in `apply`**: Save before any Send; Removed and Departed only after the last word is queued.
- **A new field or type in a signed body**: it changes the signed bytes, so it needs the `protocol-change` skill. A change to `SavedRoom` needs a new `ROOM_MAGIC` in `store.rs`.
- **Anything in `couch_net::admission`**: labels, inputs and order change what is proven. Update `docs/protocol.md` and ADR 0006 together, and keep the outside-review follow-up.
- **New `Debug`, `tracing` or error text** near passwords, password keys, invite tokens or links.
- **Waits and timeouts in the accept path**: a timeout must never drop an answer the actor already acted on.

## What isn't covered automatically

- **Several profiles on one machine**: `npm run tauri dev` in `apps\desktop`, then `.\scripts\profiles.ps1` (alice, bob, carol). Ask the user to join, ban, leave and close a room with one of them closed meanwhile, and to open an invite link from a browser. `docs/m4b-results.md` has the full list.
- **Across networks and NATs**: needs our relay (M7) and a second machine.
- **The admission composition itself**: needs an outside review before any public release.

## After the checks

- Report the results, with the scenario runs and any flaky pass.
- If behaviour changed on purpose, update `docs/protocol.md`, ADR 0006 if the design moved, and the milestone's results file.
