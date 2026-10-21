# M13, part A results: handing a room over

## Why

A room's owner was fixed for good: the room id binds its founder, and only the founder signed the roster, admitted newcomers and banned. An owner who stops playing left a room that could no longer grow or ban anyone; the fix was a new room. Part B (a [profile's backup](m13b-results.md)) brings a lost identity back. Part A lets the owner hand the room to another member.

Your choices (2026-10-21):
- **two steps:** the owner offers, the new owner accepts, both online;
- **the new owner sets a new password:** nothing secret travels, and the old owner's invites stop working.

Admins and a designated successor were rejected: several signers fork the roster.

## What changed

**The owner is the end of a chain** (`couch_proto::room::Handover`, `OwnedPolicy`; `docs/protocol.md`, Handing a room over).
- **The chain.** The room id still binds the founder. Each handover `{room, from, to, after}` is signed by the owner handing over, and the chain from the founder travels with every policy. The owner now is its last `to`.
- **A valid policy** is signed by that owner, past the last handover's version, or by the one who handed over, at exactly that version.
- **The policy body and its signature are unchanged,** so every room's stored signatures stay valid.

**Which policy wins.**
- A policy replaces ours only if its chain extends ours, and then a longer chain wins, whatever the version.
- So a former owner's later policies always lose to the new owner's, and a chain that parts from ours is never taken.
- That order comes before any signature is checked, and only the part of a chain we don't hold is verified.

**The handover, in three messages and one signature.**
1. **Make owner** (beside Ban) sends `HandoverOffer` to a member online.
2. They answer with `HandoverAnswer`. Accepting takes a new password, typed in a dialog, whose key waits in the engine. Accepting is for good: the owner may sign at once, so there is no taking it back, and the member can't leave until the room is theirs, the owner withdraws, or the owner signs anything else.
3. The owner signs the handover after its current version. That policy is the last it signs: its password key and invites go before the save, and the policy goes to everyone, who passes it on.
4. The new owner signs its first policy at once, with the password it chose. If it restarted in between, it owns the room without a password and is asked for one.

What voids an offer:
- any new policy, or its connection closing (`HandoverWithdraw`, sent when the member connects again, so an acceptance lost on the way ends);
- a stale answer (another offer id, another connection, another policy version), which hands nothing over.

**Make owner** shows only when the room can be handed over now (`RoomView::can_offer`): not while catching up or unconfirmed after a restore, not with an offer standing, not past 32 handovers. Past 32 the room goes on as usual; only handing it over again is refused.

**Afterwards.**
- The former owner is a member: it can be banned, and its leaving is a member's.
- Its old invites, and a newcomer mid-check, are refused as expired.
- Everyone gets a notice ("Bia now owns Sofá"); the member offered gets one when the offer arrives.

**Invites name the founder** (version 2): the same layout, then the founder, so the invite page reads the name where it did. A room never handed over still makes version 1 invites.

**Restores (part B) follow the chain.**
- A former owner restored from an older backup becomes a member once it catches up.
- A new owner restored from a backup made before it took over catches up as an owner, waiting for everyone, and signs ahead.
- One that finds a newer policy it signed and doesn't hold has lost state: it catches up again.
- After Stop waiting, the room can't be handed over until every member has been heard (the mark says `stopped`).

**Storage.** Room files are `CCR3`; `CCR2` files still read, through `store::legacy`, and are written back as `CCR3`. Backups are format 2; format 1 still opens.

Wire revision 6, version 0.16.0: builds before it, the friends' 0.12.0 among them, can't talk to it.

**A design review before coding** (a Plan agent) replaced the first sketch, in which the old owner sent the new one a grant to sign. That could leave a room with no owner: the new owner restarting and losing its acceptance, or the old one leaving or never returning. The handover policy is now the old owner's own last word, so anyone relays it. The review also found:
- the version-only comparison;
- the stranger with a made-up chain;
- the lost-state case after a restore;
- replayed answers (now bound to an offer id and connection);
- the save order for the password key (`Effect::Ownership` before the save);
- leaving with an accepted offer.

**A second review, of the built change**, found and confirmed by probe tests:
- **high:** a member could relay a newer policy with the signatures of handovers we already hold swapped for junk. We skipped verifying those (same body), stored the junk, and the room failed to load on the next start. Now our own verified copies are kept, and only the new handovers are taken;
- **medium:** an acceptance lost to a dropped connection, or to a decline sent after it, followed by leaving, could leave a room nobody owned (the owner may already have signed). Now accepting is for good, as above, and a voided offer is withdrawn when the member connects again;
- **medium, documented** (below): a restored owner handing the room over a second time;
- **low:** a former owner could sign, under the start of our chain, a late policy naming a stranger, who would then be shown the roster. Now a prefix counts only for policies from before the next handover;
- smaller: a pending password key outliving its offer, the invite preview, and docs saying a fork heals (it doesn't).

## Known limits

- **A former owner's key can still fork.** It could sign a second handover after the same version. Members holding the real chain never take it; a member that missed the handover could, and then stays on the fork (it never takes the real chain either) until it leaves and is invited again. The app never signs that; a stolen key could. Breaking ties between parted chains was weighed and rejected: it would let such a stolen key take the room from everyone. (ADR 0006.)
- **A restored owner can hand over twice.** Restored from a backup made before it handed the room over, with the new owner away and known to no member it hears (one let in after the backup), it doesn't learn of the handover and may offer the room again: the chains part, as above. Rare; leaving and inviting again mends it.
- **No taking back.** Once signed, the handover stands. A new owner who accepted and never comes back leaves the room ownerless, as a lost owner identity did before.
- **An acceptance holds the member** until the owner's word: if the owner never comes back, the member can't leave (the owner may have signed). Restarting the app clears it (it lives in memory), which reopens a tiny window for an ownerless room.
- **At most 32 handovers per room.**

## Tests

- `couch-proto`: the messages' round trips and must-understand bit; a full snapshot with 32 handovers fits a frame; invites of version 1 (pinned, unchanged) and version 2 (pinned, round trip, a non-canonical one refused).
- `couch-room` (116):
  - the happy path, with a member away meanwhile who converges on return;
  - a former owner refused for invites, admissions, bans and revoking;
  - a former owner's later policies losing everywhere, both ways;
  - diverging chains refused, and a stranger with one shown nothing;
  - a table of invalid chains;
  - an earlier tenure's policy not replayed;
  - stale answers and impossible offers;
  - a newcomer mid-check;
  - the others carrying the handover when the old owner is gone;
  - a new owner that restarted meanwhile;
  - an acceptance standing until the owner hands over (no decline, no leaving), and one lost on the way withdrawn on reconnecting;
  - a relayed policy keeping our verified handovers;
  - a former owner's late policy proving no stranger a member;
  - offers while catching up (declined) or to a departed member;
  - the new owner banning the old one and closing the room;
  - a joiner taking only the owner its invite names;
  - restores following the chain, and the lost-state check;
  - no handover after Stop waiting until everyone is heard;
  - the property test with a handover at a random point among crashes (checked to reach one).

  Seven of them were checked to fail with their rule broken: the version-only order, the lost-state check, the former-member check, keeping our verified handovers, the prefix's version, the acceptance standing, and the withdrawal on reconnecting.
- `couch-engine`:
  - rooms scenarios:
    - `a_room_is_handed_over_and_the_new_owner_lets_people_in`: offer, a password required, accept, notices; the old owner can't invite or change the password; its old invite is expired; the new owner's version 2 invite works with the new password only; a member away meanwhile converges; ownership survives a restart;
    - `an_offer_declined_or_withdrawn_changes_nothing`;
  - store: a `CCR2` room reads and is rewritten as `CCR3`; a restore keeps the chain; the restore mark's states;
  - backup: a format-1 backup opens;
  - the secrets test covers the new password.
- The invite page reads both versions (`node deploy/invite-page/check.js`).

## Your run (2026-10-21, this PC)

Three profiles, one owning a room the other two were in:
- **Make owner** offered the room, and the owner saw "Waiting for … to accept" with Withdraw.
- The member got a notice, accepted with a new password, and was told it couldn't leave until handed the room.
- Everyone saw the ♛ move and the "… now owns …" notice.
- The old owner's Invite button was gone, and its old invite was refused as expired.
- The new owner invited a fresh profile, which joined with the new password, then banned the old owner and closed the room.
- A decline and a withdraw changed nothing.

Everything as described.
