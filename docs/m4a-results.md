# M4a results: rooms in the engine

M4a gives the engine rooms:
- an identity per profile;
- invite links, made by the room's owner;
- a password check between a newcomer and the owner, with rate limits;
- a mesh that dials, deduplicates and redials, with members accepting each other by the owner's signed roster;
- presence;
- remembered rooms that reconnect after a restart, with or without the owner.

The owner can ban a member, revoke invites, and close the room by leaving. Everything is reachable through engine commands, which M4b's lobby UI will call. There is no UI yet.

**It works.** Every check below passes, measured 2026-10-04 on the M1 PC. The design is [ADR 0006](adr/0006-rooms-without-a-server.md); the wire format is [protocol.md](protocol.md#rooms).

## How the design got here

1. **First version:** any member admitted newcomers, with a room secret every member held and replaced on a ban.
2. **The code review** found that a ban sent the new secret to the banned member. Our own pass found that a member who missed a rotation could let an old invite back in. Both were fixed (a secret hash and a roster in the policy), but the fix was intricate.
3. **You then proposed that only the owner lets people in.** That is the design now. The holes can't happen by design, the secret generations and their distribution are gone, and guessing is bounded at a single place. The cost: joining needs the owner's app running.

## Checks

| Check | What | Result |
|---|---|---|
| Protocol (50 tests) | Admission and room messages round trip; presence is the only optional message; a full snapshot (64 records, a 64-member roster and 64 bans, all at their largest) fits a 64 KiB frame; invites round trip, survive chat-app wrapping and case changes, and reject other base32 text, damage and trailing bytes; names with direction overrides or zero-width characters are refused | pass |
| Signatures | Records verify only against their own member, policies only against the owner; any change to the body, or a replay across domains or rooms, fails; an invalid key fails closed; invite tags never reveal the token | pass |
| Rate limits | Backoff doubles from 1 s to a 10 min cap and is forgotten after an hour. 10 failures by strangers pause joining for 10 minutes, then one at a time; members are never paused. Parallel checks can't overshoot the threshold; one check at a time per id and room; unreported checks expire; tracked ids are bounded | pass |
| Room machine, simulated (30 tests) | A deterministic network of members with per-connection ordering and random interleaving, where an accepted dialer must get a snapshot first: full mesh; crossed dials keep the lower id's; redial after a drop; crash and restore; members reconnect without the owner; only the owner admits, invites and bans; joining needs the owner; a member who missed a join catches up from the newcomer; a ban removes the member everywhere, refuses its id even with a new invite, and revokes older invites; a member banned while offline learns it on return; proofs must come from the owner; leaving takes a member off the roster (and back needs an invite); a replayed `left` record from an earlier stay doesn't remove a member who came back; a member who leaves while alone tells the room later, across its own restart; the owner leaving closes the room, and a member offline then hears it on return, across the owner's restart; a stranger's `MemberHello` gets no roster, even with a policy it signed itself, while a former member is shown why; at most 5 checks run per invite; a ban, a revoke, an expiry or a full room during the check stops admission; forged policies, forged records and outsiders' records are ignored; the 12-member cap; presence; a tampered save is rejected; invites expire, can be revoked, survive the owner's restart and die after five wrong passwords; invites name the relay and never an IP when asked; dial backoff | pass |
| Room machine, property test | 32 random runs of crashes and restarts (the owner's included) and a late join: every run ends in one mesh where every member has the same policy and records | pass |
| Password check (unit) | Same password admits both ways; a wrong password, the wrong invite token, another connection's keying material, a claimed identity or another invite tag all fail; malformed SPAKE2 messages fail; passwords are trimmed and NFKC-normalized, and differ per room | pass |
| Admission over iroh | Real endpoints on loopback: a join admits both sides; a wrong password fails on both (the owner sees `admission_failed`); a member hello is answered with the snapshot; a refused member is shown the policy; the gate refuses before our protocol starts | pass |
| DPAPI | Round trip for this user; wrong entropy and a flipped byte fail | pass |
| **Three engines** (real iroh, loopback, own profile folders; 18 scenarios) | **Join** through the owner's invite and a full mesh; a rename seen everywhere; only the owner makes or revokes invites | pass |
| | **Wrong password**, then the right one straight away is refused ("too many attempts"), then admitted after the backoff | pass |
| | **Three strangers guessing** pause joining: the owner gets `JoinsPaused`, a newcomer with the right password is refused, a member restarting still gets in | pass |
| | **An invite dies** after five wrong passwords; another works | pass |
| | **Ban**: the banned member learns it from the owner's signed policy and forgets the room; the others keep their mesh; the banned member is refused even with a fresh invite; older invites stop working | pass |
| | **Banned while offline**: on return, the member is shown the proof and forgets the room | pass |
| | **The owner offline**: a join reports "the room's owner isn't online" and keeps nothing; members still reconnect to each other | pass |
| | **A member who missed a join** meets the newcomer with only the two of them online | pass |
| | **Simultaneous restart** of all three: one connection per pair, both ends agreeing which | pass |
| | **Leaving** is final and survives restarts; the owner can still ban the member who left; **the owner leaving** closes the room for everyone | pass |
| | **Leaving while nobody else is online**: the room disappears from the list at once, stays on disk across a restart, and is forgotten once the others are back and have heard | pass |
| | **Closing the room while a member is offline**: the owner keeps it, hidden, across a restart, until that member returns and is told | pass |
| | **Revoked and expired invites** stop working; invites survive the owner restarting | pass |
| | **Invites with relays configured** (an unreachable one) carry no IP address | pass |
| | Bad input (not an invite, blank password or name, unknown room) gives clear errors | pass |
| | All 18 scenarios, five runs in a row | pass, about 18 s each |
| Through iroh's public relay (ignored test, needs internet) | An invite naming only the owner's relay; B joins, then reaches A directly | pass |
| Desktop app | The app starts the engine (identity, endpoint, iroh's public relays) before its window opens, and closes cleanly (you, 2026-10-04, before the owner-only rework) | pass |
| Workspace | 274 tests; clippy (all targets) and fmt clean; `cargo deny` advisories, bans, licenses, sources ok; the front end type-checks against the new bindings | pass |

## Decisions made along the way

These go beyond the plan; ADR 0006 has the reasons.
- **Only the owner admits newcomers, makes invites, bans and removes.** The owner is the room's creator, bound into the room id, and signs the roster. Members accept each other by that roster, without a password check among themselves.
- **Invites are per invite:** a random token each, kept by the owner. They last 7 days, can be revoked, die after five wrong passwords, and are all revoked by a ban.
- **Only the owner keeps the password key**, and nobody keeps the password.
- **The owner leaving closes the room.**
- **One connection per pair of members per room**, not per pair.
- **SPAKE2 is `spake2` 0.5.0-pre.0**, pinned exactly. It is the same algorithm as 0.4.0, but uses curve25519-dalek 5 and sha2 0.11 like iroh, where 0.4 would add older copies of both.
- **Invites never carry IP addresses when relays are configured**, and wait for our relay right after startup.
- **`EngineOptions::port`** pins the UDP port. Without a relay, members who all restart on new ports can't find each other again; the tests use it, and so can a LAN-only setup.

## Found and fixed

While building:
- **Other base32 text read as "an invite from a newer version".** "hello there" is valid base32. Invites now start with two magic bytes, and other text is "not an invite".
- **Presence could overtake the record it mentioned.** A member listed in a peer's presence before its record arrived was dropped from that report, so it never showed as online. Reports now keep every id; only members are shown or dialed.
- **A connect timeout read as "the password check timed out".** It now reads "no answer from the other member".

**From the code review** (a review subagent, plus my own pass; each finding checked against the code). The two high findings belonged to the first design and are gone with it:
- **High:** banning handed the new room secret to the banned member. (Found by both.)
- **High:** an old invite got back in through a member who missed the rotation. (Mine.)

The rest were fixed and carried into the owner-only design:
- **Medium: parallel guesses overshot the rate limit**, about 32 per burst instead of 10. Checks still running now count against the threshold. (Both.)
- **Medium: a peer that stopped reading made our outgoing queue grow without bound**, and could keep a ban's close stuck behind it. The queue is bounded, and overflowing it closes the connection. Every close happens within 2 s even if the writer is stuck. Messages from peers being cut off are ignored. (Both, and mine.)
- **Medium: invites made right after startup could carry IP addresses**, before the relay was known. (Both.)
- **Medium: a member banned while offline never found out.** It only saw refusals, which a peer could fake. Members it dials now show it the owner's signed policy. (Mine.)
- **Low:**
  - Records of departed members were never pruned; after 64 distinct members, newcomers were ignored. Records are now kept only for the roster. (Reviewer.)
  - Signatures were checked even for records that weren't news. (Mine.)
  - Names allowed invisible and direction-reversing characters. (Mine.)
  - A failing random-number source panicked the engine instead of returning an error. (Reviewer.)

The reviewer found no problems in the admission key schedule, the constant-time comparisons, secret redaction, the DPAPI code, the protocol rules or duplicate resolution.

**From the second review**, of the owner-only rework (a review subagent; each finding checked against the code, all confirmed and fixed):
- **A replayed `left` record removed a member who had come back.** Records had no notion of which stay they belonged to, so an old `left` record with a high version beat the member's new one, and members could disagree at equal versions. Roster entries now carry `since`, records carry it too, and only records of the current stay count. The record a joiner used to send before its snapshot is gone; the snapshot carries it.
- **A stranger's `MemberHello` was answered with the full roster** as "proof". Only a former member (its own copy of the owner's policy lists it) is shown the policy now; a stranger gets a bare close.
- **Leaving and closing reached only the members connected at that moment.** A member who left while alone stayed on the roster for good; members offline when the owner closed the room never heard. Leaving is now a saved state that lasts until the word is out (see [protocol.md](protocol.md#rooms)).
- **One invite allowed 10 parallel guesses**, twice its five-failure allowance. Checks running with an invite now count against it.
- **Nothing was checked again when the password check ended.** A ban, a revoke, an expiry or the room filling up during the 10 s check didn't stop admission. The owner now checks again before adding the newcomer.
- **A policy could go out before it was saved.** After a crash the owner could sign a different policy under the same version. The engine now saves before it sends anything.
- **A check could start and never end in the rate limits**, if the accept task timed out while the actor was answering. The actor is now asked outside the timeout, and an answer nobody receives releases what it reserved (`AdmissionLimits::cancel`, `Room::join_released`).
- **Passwords and invite links reached `Debug` output** (`Command`, `Response::Invite`, a remembered room's password key). They print as `..` now.
- **Only current members could be banned.** The owner can now ban any id but its own, such as a member who already left.
- **Docs:** the `version` close code was documented but never sent. The control stream now reports must-understand types it doesn't know, and admission and the engine close with `version`. A comment still described the room secret of the first design.

While fixing: **a leaving member's last word broke the dialer's handshake.** A member who dials expects a snapshot first, and a leaving member answered with its record, so the dialer dropped the connection while the leaver counted it as told. The last word is now a snapshot holding the `left` record, and the simulator checks that every accepted dialer gets a snapshot first (it catches the old behavior).

## Things to know

- **Joining needs the owner's app running.** No clicks are needed. If the owner is away, someone else can create a room and invite the others. Members reconnect among themselves without the owner.
- **The owner is a single point** for joins and bans. A lost owner identity leaves a room that can't grow or ban, though members keep playing in it. Handing over ownership, or admins the owner picks, is future work.
- **No password change yet.** It would now be simple, since only the owner holds the password key; it just isn't built. (Added in M4b.)
- **Guessing budget**, all at the owner:
  - 5 wrong passwords per invite;
  - about 10 failures an hour per room, after which joining pauses;
  - one check at a time per id.

  M4b's create-room form should still ask for a reasonable password.
- **A room being left stays behind, hidden.** It keeps dialing (backing off to once a minute) until the word is out. For a closing owner that means until every former member has heard, so a member who never comes back keeps it dialing for good. A cap on that, or a "forget anyway", may be worth adding with the lobby UI.
- **Room files changed format** (magic `CCR2`): rooms saved by earlier development builds aren't read.
- **A dial to a member who is offline holds for up to 15 s** (our connect timeout), and nothing else is dialed to that member meanwhile. If that member starts up during it, its own dial gets through regardless.
- **The CLI's `host` and `view` still connect by ticket without rooms.** Streaming moves into rooms in M5.
- **`Hello`'s version fields aren't checked yet.** Nothing has changed since `couch-coapp/0`.

## Follow-ups

- **M4b**: the lobby UI over these commands, with the members' direct and relay badges, the `JoinsPaused` warning, error texts for a failed join, and the invite's expiry.
- **M7**: our relay. Production invites will then name it, and members find each other by id after restarts. Its rate limits also matter here, because anyone who knows an endpoint id can occupy the 32 admission slots for 10 s at a time.
- **Before any public release:** an outside review of the admission composition (ADR 0006).
- **Later:** ownership transfer or admins; a password change (done in M4b).

## How to reproduce

```powershell
. .\scripts\dev.ps1
cargo test -p couch-proto -p couch-room -p couch-net -p couch-engine
cargo test -p couch-engine --test rooms                    # the three-engine scenarios, ~18 s
cargo test -p couch-engine --test rooms -- --ignored       # join through iroh's public relay (internet)
cargo test -p couch-platform-win dpapi
```
