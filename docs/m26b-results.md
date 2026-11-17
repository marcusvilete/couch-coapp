# M26b results: relay codes passed on by themselves

## Why

M26a let a profile hold several relays and made the app say why someone can't be reached. One case still needed a person: a member on a relay with a code you don't have. The app could only say "ask them for its code", and someone had to copy a code by hand. [ADR 0014](adr/0014-public-builds-and-registered-relays.md) settled that nobody should copy a code to play with their own room: the members pass on the codes the room needs, by themselves, and an invite carries the owner's.

**Your choices (2026-11-13):**
- one PR, 0.37.0, wire revision 20;
- a personal code that gets refused is never asked for again: it was taken back on purpose. The app says to ask the relay's owner;
- a relay you registered yourself (not personal) whose code is refused, then renewed by a room member, is updated in place: it stays yours, still passed on to all your rooms, and Settings says "new code from Bia";
- the remote-play device saying why it can't reach its home PC, left over from M26a-2, is in this PR;
- the ADR's "In parts" wins over its body's "(M26b)" labels: the update link, the antivirus submission and the invite page's download stay in M26c.

## What changed

**The wire** (revision 20; the `protocol-change` skill; `docs/protocol.md` "Relay codes passed on")
- `RelayAsk { url }` and `RelayAnswer { url, answer: Code(token) | None(Unavailable | Busy) }`, room messages `0x8508` and `0x8509`, on the room's control stream, never relayed. One "none" for every reason, so asking can't tell which relays a member holds. The token is a `RelaySecret`: 1 to 512 bytes of visible ASCII (what an HTTP header carries), `..` in Debug.
- Invites of version 3 carry the code of the owner's relay after the fields of version 1, then the founder if it isn't the owner. The invite page reads the room's name at the same offset as before. The oldest layout that holds an invite is written, so an invite without a code reads in earlier builds.

**Asking and answering** (`couch-engine`'s `rooms::passing`)
- **Asking:** a member asks the members of a room it is connected to for a relay's code:
  - when a member's reason says their relay needs a code we lack (M26a-2's probe);
  - or when one of our own relays refuses our code, unless the code is personal.

  At most once a minute per room and relay (`NetworkTimings::ask_every`), not while a code it got is being checked, and never with "Use relays my rooms pass on" off or for a relay the user removed.
- **Answering:** only a member of the room, on that room's current connection, at most 6 asks a minute from each member. It answers with the code only if:
  - the relay is ours to pass on there (`SavedRelay::passable`): it has a token, isn't from a personal code, its switch is on, and it is either registered by the user or received for that very room;
  - and the room really uses it: it is our home relay (our signed record), or one of our connections in that room, to someone other than the asker, runs through it now (`couch_net::health::relay_paths`).

  So claiming a home relay you aren't on gets nothing, and a code received for one room never goes to another.
- **Taking an answer:** only on a connection we asked on, once, for the relay asked. The code is checked against the relay with a throwaway endpoint before it is kept:
  - let in: kept, as received for that room from that member, or, for a relay of ours refusing our code, its token replaced in place;
  - refused: dropped, and never tried again;
  - no answer: a new relay is kept unchecked and tried again by the regular checks.
- **Noticing a rotation:** iroh reports refusals of the home relay alone. So when a member on one of our other relays can't be reached, that relay's code is checked again, at most every 30 s (`Network::suspect`, `NetworkTimings::suspect_every`). Without it, a rotation went unseen for up to 10 minutes, the regular check's interval.
- **The member's reason** (`MemberReason::NeedsCode.passing`):
  - "getting its code from the room…" while asking;
  - "needs a code nobody here can pass on: ask Bia for it" once everyone answered without one;
  - "ask Bia for it (you don't take relays your rooms pass on)" with the switch off.

**What is kept** (`relays.rs`, in the sealed `relays.sealed`)
- A received relay records the rooms it came for, who gave it (id and name then), and when each room last needed it.
- It goes when the last of its rooms is forgotten (left, closed, banned): not while a room being left is still saying its goodbyes, perhaps through it. It also goes after 30 days without a member of those rooms naming it (`SavedRelays::needed`, looked at hourly).
- It counts towards the 16.
- **Share never copies a received relay's code**, so it stays with its rooms.
- Removing any relay stops asking for it until it is added by hand (`dont_ask`, at most 64 remembered).
- Backups carry received relays like the user's own.

**Joining with the code in the link**
- `Response::Invite { link, code: InviteCodeView { host, passable }, without_code }`: one invite, its link carrying the code when we may pass it on, and the same invite without it.
- The invite dialog has a switch, "Include relay.example.com's code", on by default, which picks between the two links (never a new invite, so toggling can't push out links already sent), with one sentence that stays the same either way, so the row never changes height: "Joining needs no code from you then; but whoever has the link can use relay.example.com, password or not, until its code changes." A personal code, one kept to you, or one received for another room, says the link can't carry it.
- The newcomer lends the link's code to its endpoint for the join alone (`RelayHub::lend`), and keeps it, as received for the room from the owner, only once admitted. A wrong password, or any other failure, takes it back.
- The join dialog says nothing about a relay whose code the link carries (`InviteRelay::Carried`). A link's code the relay refuses is said as such: `JoinProblem::LinkCodeRefused`, "ask for a new link".

**One way to change relays** (`RelayHub`)
- `Engine::change_relays` and the room actor share one handle for saving and applying relays: one change at a time, saved, then applied to the running endpoint, whether the user, a passed-on code or a join's lent code made it.

**Settings → Network**
- **A received relay** shows "from your rooms" and a line naming who passed it on and for which rooms ("From Rafa, for Friday games and Caio, for Sofá").
- **A renewed relay** shows "New code from Dee".
- **A relay kept to you** shows "kept to you". Each relay's menu has "Pass it on to my rooms" / "Stop passing it on to my rooms" (`Command::SetPassOn`).
- "Use relays my rooms pass on", on by default (`Command::SetUsePassedOn`).
- The removal dialog of a received relay says your rooms won't pass it on again unless you add it yourself.
- Copy diagnostics marks relays passed on, renewed or kept to you, without naming anyone.

**Remote play: a home PC out of reach** (your choice; ADR 0014's remote-play line)
- An accepted home PC that isn't offering remote play now still has a view (`RemoteAccessView.offline`).
- The device's panel says "Ana's PC, your PC, can't be reached", with the reason when the app can tell, and Forget. Where the fix is at the PC (its relay not answering, or one this app won't use), it says "That has to be fixed at Ana's PC". Where we lack its relay's code, it says to share it from the PC's Settings when you're there.

## Checked

- **Windows:** `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace`; the rooms, sessions and relay suites three times (35, 98 and 12 tests, none failing), and again after each review's fixes.
- **Linux** through WSL: fmt, clippy and the whole workspace, before and after both reviews' fixes (rooms 35, sessions 98, relay 12, engine units 186).
- `bash scripts/check-docs.sh`: nothing stale.
- **New scenarios through real relays** (`couch-engine/tests/rooms.rs`, in-process iroh-relays with tokens, relay-only):
  - `a_member_gets_a_code_through_a_third_and_connects`: Bia and Cai can't reach each other until one asks. Dee answers; Cai's personal code and Ana, who holds neither, give nothing;
  - `a_received_code_stays_in_its_room_and_a_personal_one_with_its_holder`: in a second room without Dee, Eve hears "nobody here can pass it on", and never holds Cai's relay;
  - `a_join_with_the_code_in_the_link_keeps_it_only_once_admitted`: a link without the code fails at that step; with it, the preview says carried, a wrong password keeps nothing, and the right one keeps it for the room from Ana;
  - `a_new_code_reaches_a_member_whose_code_was_refused`: the relay restarts with a new token, Dee is given it by hand, and Bia's own relay is renewed from Dee.
- **Remote play:** `sessions`' `desktop_control_ends_with_the_operator` now checks that an accepted PC with remote play off shows as `offline`.
- **Unit tests:**
  - `passing::only_a_relay_the_room_uses_is_answered` (the answer rule);
  - the store's receiving, rooms, renewal, the switch and `dont_ask`, and the 30 days;
  - invite version 3 (with and without a founder, canonical form, damage);
  - `RelaySecret` never printing, and asks and answers checked;
  - diagnostics with a received relay and no names.
- **The UI:**
  - `npx tsc --noEmit` and `npx vitest run` (116 tests);
  - `npm run shots`: new scenarios `network-passed-on`, `invite-personal-relay`, `join-code-in-link` and `remote-unreachable`, a waiting member in `member-reasons`, shots of the relay's menu and the invite dialog without the code;
  - a new no-jump view, "invite code" (open, leave out, put back).

## Found while checking

- **iroh-relay's `Server` has no `Drop`:** dropping one leaves it running. The test relay's new `restart` (a rotation) shuts it down first.
- **A rotation of a relay that isn't our home went unseen for up to 10 minutes,** since iroh says only the home relay refuses us. Hence `Network::suspect`.
- **A rotation's overlap moves nothing.** The ADR expected a home PC to hear the new code while both tokens work. But codes are asked for only once ours is refused, and once the old token goes, a PC whose only relay is that one can't reach anyone to ask. The deploy guide now says to paste the new code at such a PC, or give it a second relay, before the old token goes. ADR 0014 records it under "Settled while building M26b".
- **Each shots run with `--only` empties the shots folder;** look at the images right after the run that made them.
- **C: had 4.5 GB free** before the Linux run. `target\debug\incremental` (82.6 GB, cargo's cache) was cleared: the next builds are a little slower once.

## The review

A fresh review agent read the diff against main (code only). Its findings were each checked against the code; all held, and all but the test gaps are fixed.

1. **High: a member could plant a code that can never be checked.** A token with a character an HTTP header can't carry (`café`) is never sent: iroh fails building the header, so the check reads "not answering", not "refused". Kept unchecked, it replaced the good code another member had just given, and nothing ever asked again: the member was cut off for weeks, its reason blaming the relay. Now codes on the wire, in invites and added by hand are visible ASCII alone (`RelaySecret::is_valid`, `add_relay`), **a code that didn't let us in never replaces one held** (`SavedRelays::receive`), and the room entry credits whoever gave the code held now.
2. **High: a received relay counted as the user's own.** A user on the public relays alone who joined through a link carrying a code lost the public relays: the received relay became their only one, hence their home, named in their record to rooms it can't be passed on in. And the wizard stopped asking. Now only the user's own relays decide both (`SavedRelays::choice`, `Network::asks_for_relay`; test in `a_received_relay_is_for_its_rooms_alone`).
3. **Medium: renewing a received relay asked every room,** so a code moved from one room to another. It asks only the rooms it came for now.
4. **Medium: a member simply offline had its relay checked every 30 s, for good.** The interval doubles while members on it stay away, up to the regular checks' 10 minutes (`Network::suspect`, `calm`).
5. **Medium: a relay received for one room was never added for a second** whose link named it, so leaving the first took it. An admitted join adds the room (the join scenario now checks it).
6. **The lent relay:**
   - a link anyone made could have become our home relay, signed into our record for every room, during the join: a lent relay is never named in our record now (`refresh_addr`, `Network::lent`);
   - two joins naming one relay shared one entry, so the first to fail took it from the other: lending counts per join;
   - `lend` ignored a restore's freeze;
   - where relays are fixed, nothing was lent but a refusal still read as the link's code: a join now knows what it really lent, and nothing is asked for there.
7. **Relays received for rooms we no longer hold** (a check finishing after we left) stayed in the map for 30 days: the hourly look drops them.
8. **Two peer-fed maps grew without bound:** codes refused are capped at 256, and answer counts older than a minute go.
9. **Logging:** an ask's URL could carry line breaks into the log, and a flood filled it at info. Control characters are refused, URLs logged escaped, and refusals for too many asks go to debug.
10. **Answers told which relays a member holds** (`NotPassed` against `NotUsedHere`): one `Unavailable` now.
11. **The unlock dialog stayed open** when the PC stopped offering, since an offline PC now has a view. It closes on `offline` too.
12. **The invite dialog:** each toggle made a new invite (32 at most, the soonest to expire dropped first, so toggling could push out links already sent). Now it is one invite with both links. The code's row reserves two lines' height.
13. **`unchecked` was never cleared** by a later check: it is when a checked code comes, and the UI doesn't lean on it.
14. **Tests not covering what they claimed:**
   - the claimed-home rule is tested as the pure rule (`answer_for`), not through an engine;
   - the join test can't see the lent relay taken back after a wrong password (lent relays never enter the saved list).

   Both are left as they are (below).
15. **Doc nits:** three doc comments had slid onto the wrong item; protocol.md said renewal was for our own relays only; a link's text didn't name a relay received for another room. All fixed.

### The second review

The first review had two High findings, so a second fresh agent read the diff after the fixes. It confirmed every fix above in the code, found the lock order and the secrets sound, and found eight more, all checked and fixed:

1. **Medium: a link merely claiming a code could carry one room's code into another.** A link naming a relay we hold as received for room A, with any code at all, made us add room B to it once admitted, so we would then answer B's members with A's code. Room B is added now only when the link's code is the one we hold.
2. **Asks never backed off:** every minute for as long as a friend stayed away. An ask nobody can answer waits twice as long each time, up to 10 minutes, and repeats log at debug.
3. **A held token the asker would refuse whole** (too long, or saved before codes were visible ASCII) jammed asks at "getting its code…": it is answered as unavailable, and typed codes over 512 bytes are refused.
4. **Two joins lending one relay:** the second could keep its own code marked checked though the endpoint used the first's. `lend` now returns what is really lent.
5. **The hourly look could drop a relay kept a moment after its snapshot** (a join admitted meanwhile): whatever is newer than the snapshot stays.
6. **"Off" also covers a relay the user removed:** the text is neutral now ("your rooms aren't asked for it: see Settings → Network").
7. **The preview could say a link's code would be used where relays are fixed:** only where relays can change.
8. **A member running several relays could fill the 16:** rooms pass on 8 at most, so the user's own always fit (`MAX_RECEIVED`).

It also pointed again at the limit below: a newcomer on public relays alone who joins through a link keeps that relay, and iroh may make it their home.

## Still to come

- ~~**One test the review asked for:** the claimed-home rule through engines.~~ Done after M26c (0.39.1): `a_relay_a_record_merely_names_gets_no_code` in `tests/rooms.rs`, with no test hook. An offline member's record still names its relay, a claim nobody is on, the same to the others as a record lying about its home. A member holding that relay's code, passable in the room (received for it, so never her home), is asked by a newcomer and gives nothing. With the rule taken out of `answer_for`, the test fails. The other, a join's lent relay seen taken back after a refusal, came with the follow-ups (below).
- **Hand checks with two PCs:** a code passed on through a friend, a join with the code in the link, a rotation reaching a member whose code was refused, and the remote panel with the home PC's relay down. The scenarios above cover each through real relays on loopback.
- **Records don't follow a member to a relay nobody else holds** (M26a-1's finding). Code passing narrows it: a member whose relay changes to one the room can pass on is reached again once someone asks.
- **M26c:** public releases, the update notice, the version mismatch's link, the invite page's download and relay note.

## The follow-ups (0.38.0)

The author chose to close two gaps before M26c (2026-11-16), with three choices made at the start: a passed-on relay may be home only when no other relay answers; a code fetched during a rotation's overlap waits on standby, never replacing the one in use; hints are sent by whoever pastes a new code and spread by whoever takes a standby.

- **A relay passed on to us, or lent for a join, is never our home relay while another answers.** The vendored iroh's second change (`third_party/iroh/PATCHED.md`): relays of the map for dialing only, probed and dialed as before, a candidate for home only when no other relay of the map has a latency over the reports' last 5 minutes; a home relay that becomes dial-only gives way at once. Received and lent relays are dial-only (`SavedRelays::choice`, `RelayState::apply`). No wire change.
- **A rotated code moves during its overlap** (wire revision 21, `RelayHint`; [protocol](protocol.md#a-rotations-overlap)). A member who pastes a new code hints its rooms; a holder, for a relay the room uses, asks the hinter alone and keeps the checked code on standby beside its own; once its own is refused it takes the standby, checked again, with nobody to ask. Answers give the standby, and a new standby hints its holder's rooms. While building it, Claude found that answers had to give the standby, or a member spreading the hint would hand out its old code; and that a hint had to be for a relay the room uses, or hints at made-up relays would show which relays we hold.

**Checked:**
- `cargo test -p couch-engine --test relay`, mod `dial_only`: home leaves a relay that becomes dial-only and comes back to the other when they swap (fails with the rule turned off: iroh keeps the home it has); a dial-only relay is home when the only other relay doesn't answer. The vendored iroh's `net_report::tests::dial_only_relays_are_home_only_when_no_other_answers`, and its other net_report tests, pass from a copy with an empty `[workspace]`.
- `cargo test -p couch-engine --test rooms`: `a_rotated_code_waits_on_standby_and_takes_over_when_the_old_goes` (four engines on real relays: the hint, the standby at Bia and, through her, at Eve in a room Dee isn't in, nothing renewed during the overlap, then the old code dropped while Dee is gone and both switching to their standby); `a_join_with_the_code_in_the_link_keeps_it_only_once_admitted` now also sees the lent relay taken back after a wrong password.
- Units: relays `a_standby_code_never_replaces_the_one_in_use`, `a_received_relay_is_for_its_rooms_alone` (received relays dial-only, the user's own not); proto `RelayHint` round trip and checks.

**The review.** A fresh review agent read the diff and found twelve problems, all checked against the code and fixed:
1. **Relays gone from the map still counted as answering** (removed, or set aside for refusing us), so with our own relay set aside and only a received one left we could keep no working home for up to 5 minutes. Only relays in the map or heard in the report count now.
2. **Reports don't hear every relay** (QAD probes the first five and stops at two answers), so a received relay could be home while ours answered. Ours are probed first, and a full report waits for one of them.
3. **The fallback was dead after a full report**, which clears the last report: the client remembers the home it gave apart.
4. **A standby was handed out unchecked for good,** in answers and invites. Answers give it for a day after it let us in; invites always carry the code in use.
5. **Any member could replace our standby** with a code of its own (a personal one) and have us spread it, or two codes flip back and forth: the first stays for a day.
6. **"Use relays my rooms pass on" off didn't stop a standby being taken:** it is checked when taking one, and turning it off clears them.
7. **A hint went once,** so a member connecting later in the overlap never heard it: rooms are hinted again every 10 minutes for a day.
8. **A standby whose check never answered blocked the room's asks:** after one try without an answer, the room is asked too.
9. **One member hinting every minute held the slot** an honest hint needed: per hinter now.
10. **Hints told rooms that don't use a relay that we hold it:** only rooms that use it are hinted.
11. **A departing dial-only relay was briefly a regular one** while the map changed: both sets are dial-only meanwhile.
12. **A standby outlived the room it came from:** it goes with that room.
Also: the ask after a hint names the relay as we hold it (an odd spelling was dropped), and a code held already isn't checked again. Its notes on the tests stand: Eve's switch in the rotation scenario can't be told from a renewal by Bia (Bia's half proves it), and `home_is`'s wait proves no report came meanwhile; iroh's unit test covers what the engine tests can't steer (a full report's reset, a relay that left the map).

After the fixes: the vendored iroh's net_report tests, `cargo clippy`, and the engine's rooms, relay and sessions scenarios three times each.

**The second review** (the first had high-severity findings) confirmed all twelve fixes and found the rest of part B wanting in two ways, both fixed:
1. **A member could have us spread its code in place of the one the user just pasted:** answering the paster's hint first with a code of its own that works (a personal one), it became the paster's standby, which answers preferred, and spread from there; taken back later, every answer for a day was a refused code. Now answers give the code in use for a day after the user pastes one, and a standby is checked against its relay again every hour, answers giving it only while it let us in within two hours, and dropping it once refused.
2. **Re-hints outlived their cause:** a standby gone (its room left, the switch turned off) kept its rooms hinted for a day, and the answers then gave the old code. A re-hint carries a digest of its code now, and stops once that is no longer the code we hand out.
And the smaller ones: a received relay's standby is given only in the room it came from, and hinted only there; a member is hinted only where the room uses the relay apart from it (as an answer to it would need); a hinter's slot goes by member, so reconnecting doesn't reset it; the docs say a hint follows a *kept* standby. Left as they are: the fall back to a received relay as home lags up to 5 minutes when our only other relay dies without leaving the map (iroh remembers its latency that long; documented in PATCHED.md), and the iroh unit test runs only from a copy of the folder, not in CI. Then: `cargo clippy`, the engine's unit tests (a new one for the answer rules, `answers_give_a_standby_only_where_and_while_it_may_go`), and the rooms, relay and sessions scenarios three times each.

**What this can't prevent:** a member who answers a hint first with a code that works but isn't the rotation's becomes our standby for a day ([protocol](protocol.md#a-rotations-overlap)).

**Still to come:** a rotation by hand on the deployed relay, with two PCs (the deploy guide's steps, with the overlap).
