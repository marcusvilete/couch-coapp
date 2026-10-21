# ADR 0006: Rooms without a server: the owner admits, members accept by the owner's signed roster

**Status:** accepted (2026-10-04), with M4a.

## Context

A room is up to 8 friends (12 at most). There is no server: our only infrastructure is an iroh relay (M7), which forwards packets and knows nothing about rooms. Members come and go, often not at the same time, and must find each other again after restarts. Joining needs an invite link and a password (the plan's choice), and the owner must be able to remove someone for good.

Remote control is part of the product: a room member can be handed a game's controls on another member's PC. Who gets into a room matters more here than in a chat app.

The wire format is in [protocol.md](../protocol.md#rooms). This record covers the choices behind it.

## Decision

**The owner is the room's creator, bound into the room id.** The room id is a hash of the owner's id and a nonce, so nobody can hand out an invite claiming another owner for the same room.

**Only the owner admits newcomers.**
- A newcomer runs a password check (SPAKE2) with the owner, mixing the room password with the invite's own token.
- Key confirmation is bound to the QUIC connection through the TLS exporter.
- On success, the owner puts the newcomer on the roster, which it signs. The newcomer then shows that roster to the other members.

**Members accept each other by the owner's signed roster.**
- iroh authenticates every endpoint id, so a member only checks that the dialer is on the roster. The dialer sends its copy of the roster first, so a member who missed a change can catch up.
- There is no password check, and no shared secret, among members.

**Taking someone out is a new roster.**
- A ban, a member leaving, and the owner closing the room are all the owner signing a roster without them.
- Connections to anyone no longer listed are closed. A former member is first shown the signed policy; it holds nothing secret, so it works as proof: a removed member forgets the room on the owner's word, never on a peer's. A stranger is shown nothing, so knocking reveals nobody.
- Leaving takes delivery. A member keeps trying until one member has its departure; an owner closing the room keeps trying until every former member has heard. Until then the room stays behind, hidden, across restarts.
- A ban also revokes every outstanding invite, since the banned member may hold one.

**Invites are the owner's, one token each.**
- An invite lasts 7 days. It can be revoked, and it dies after 5 wrong passwords.
- Only the owner keeps the tokens and the password key; no member stores the password or anything derived from it.
- A guesser who has an invite meets the owner's rate limits: per id, per room, and per invite.

**Each member signs its own record** (name, addresses, leaving), so nobody can rename or move anyone else. A record belongs to one stay, named by the roster entry that let the member in, so records from before a member left and came back count for nothing. Everyone relays everything and keeps the highest version that verifies; with one writer per fact, rooms converge without consensus.

**Invites carry relays, never IP addresses**, when relays are configured: invites end up in chat channels. Right after startup, before our relay is known, making an invite waits for it.

## Alternatives considered

- **Any member admits newcomers, with a shared room secret.** This was the first version built.
  - It lets newcomers join whenever anyone is online. But it needed secret generations, so members who missed a ban or a replacement of invites could catch up, and that made the secret's distribution the security boundary.
  - A code review and our own pass found two holes in it. The member being banned was sent the new secret. A member who missed a rotation would let a stranger with an old invite back in, and then hand them the new secret.
  - Patching it (a secret hash and a roster in the policy, deciding per connection who is owed the secret) worked but stayed intricate. Members could also bring in identities of their own.
  - Admission by the owner alone removed all of it. Suggested by the user.
- **Only the owner may make invites, but anyone admits.** Not a boundary: every member held the room secret and could make an invite.
- **Any member may ban.** A member being banned could ban back, or unban themselves, with no way to decide who wins. Rejected.
- **A random room id, with the owner named in the invite.** A member could hand out invites naming themselves as owner, and the people they invited would accept forged policies.
- **A password check between members too.** Redundant: iroh authenticates ids and the owner's roster lists them. A stolen member key comes from a DPAPI-sealed file, which is no easier to get than the password.

## Consequences

- **Joining needs the owner's app running.** No clicks are needed, but it must be running. If the owner is away, someone else can create a room and invite the others. Members reconnect among themselves without the owner.
- **The owner is a single point** for joins and bans. A lost owner identity leaves a room that can't grow or ban, though members keep playing in it. Since M13 the owner can hand the room over, and a backup brings a lost identity back (below).
  - *2026-10-21 (M13):* a profile's **backup** (identity, rooms and settings, sealed with a passphrase) brings the owner identity back on another PC. It never carries an owned room's password key or invites, so an old backup can't revive what was revoked or changed: the restored owner sets a new password. A restored room signs nothing until every member on its roster has sent its snapshot (any one, for a member's room), then signs the room again far ahead (65,536 versions), so whatever the lost PC signed after the backup, held by members it never hears from, is superseded rather than forked; the owner may stop waiting for members who won't come back, accepting that a change only they hold can be lost. Admins and a designated successor were rejected: several signers, or an owner and a successor who can both sign, fork the roster.
  - *2026-10-21 (M13, part A):* **handing a room over.** The room id still binds the founder; the owner now is the last `to` of a chain of handovers, each signed by the owner handing over, which travels with every policy ([protocol.md](../protocol.md#handing-a-room-over)). Two steps, both online: the owner offers, the member accepts with a new password of its own (nothing secret travels); then the owner signs the handover after its current version, and that policy is the last it signs. A policy replaces ours only if its chain extends ours: a longer one wins whatever the version, so a former owner's later policies always lose to the new owner's. What this leaves:
    - **A former owner's key can still fork.** It could sign a second handover after the same version. Members holding the real chain refuse it; a member that missed the handover could take it, and then never takes the real chain (a diverging chain is never taken, either way) until it leaves and is invited again. The app never does this; a former owner's own key, stolen, could. Breaking ties between parted chains (say, the earlier handover wins) was weighed and rejected: it would let such a stolen key take the room from everyone.
    - **A restored owner can hand over twice.** Restored from a backup made before it handed the room over, with the new owner away and known to no member it hears (one let in after the backup), it catches up honestly yet doesn't learn of the handover, and may offer the room again: the chains part as above. Rare (a backup older than a handover, plus a member nobody else knows, plus a second offer before the new owner is back); leaving and inviting again mends it.
    - **Accepting is for good.** The member who accepted can't take it back or leave until the owner hands the room over, withdraws or signs anything else: the owner may have signed already, and leaving then would leave the room ownerless.
    - **No taking back.** Once signed, the handover stands: taking it back would be the fork above. A new owner who accepted and never returns leaves a room without an owner, as a lost owner identity did before.
    - **Restores follow the chain.** A former owner restored from an older backup becomes a member once it catches up; a new owner restored from a backup made before it took over catches up as an owner. A room whose owner stopped waiting hands itself over only once it has heard every member.
- **Removal races.** Someone admitted while a member was away is unknown to that member until shown the newer roster; the `MemberHello` exchange does that. A removed member reaches only members that haven't heard yet, and is cut off when they do.
- **Members are trusted with the room.** A member can't forge others' facts or let anyone in, but it can spam snapshots (each news item costs a signature check), and it hears everything said in the room. Removing it is a ban.
- **Guessing is bounded at the owner:**
  - 5 wrong passwords per invite;
  - about 10 failures an hour per room, after which joining pauses;
  - one check at a time per id.

  A weak password is still weak; the UI should say so (M4b).
- **The cryptography is our composition** of standard parts: SPAKE2, HKDF, HMAC, Ed25519, the TLS exporter. It needs an outside review before any public release.
- **Without a relay, saved addresses go stale** when every member restarts on a new port. With the relay (the normal case), members find each other by id. `EngineOptions::port` pins a port for LAN-only setups and tests.
- **A per-room, not per-pair, connection.** Two people sharing two rooms run two connections. Simpler state, a little more traffic.
