# M13, part B results: a profile's backup

## Why

The owner is a single point for a room's joins and bans (ADR 0006). An owner whose identity is lost (a new PC, Windows reinstalled: the key is sealed to the Windows user with DPAPI) leaves a room that can't grow or ban anyone; its members keep playing, and the fix was a new room. M13 is in two parts, your choice: **B, a backup of the profile**, which also brings back a member's identity and every room, and **A, handing a room over** (next). A designated successor and admins were rejected: an owner and a successor who can both sign, or several signers, fork the roster.

## What changed

**A backup file** (`couch_engine::backup`; Settings → Backup):
- **What goes in:** the identity key, the remembered rooms and the settings files (`settings.json`, `shortcuts.json`, `network.json`, `audio.json`, `host-audio.json`; not the webview's own storage). **Never an owned room's password key or invites** (your choice, after the security question): an old backup must never bring back an invite revoked since, or a password changed since. Rooms on their way out (left, still telling the others) stay out too.
- **Sealed** with a passphrase of at least 12 characters, trimmed and NFKC-normalized as room passwords are: Argon2id (64 MiB, 3 passes) for the key, then XChaCha20-Poly1305 over the body, with the header (format, Argon2's parameters, salt, nonce) as associated data, so nothing in the file changes unnoticed. A crafted file can't ask for more than 512 MiB or 16 passes, nor be over 16 MiB.
- **Restored only into a profile with no rooms** (your choice), by the room actor, so no room is created or joined while it is written; after it, creating, joining and renaming are refused until the app starts again (a development build asks you to). The identity replaces this profile's, each room is written with a mark (`rooms\<id>.restored`, before the room), and only the settings files we write ourselves are written, whatever names the file holds. Whatever a crafted file carries that a backup never does (a password key, invites, a room being left) is dropped. A restore that fails partway is undone; a mark left without its room is swept at the next start.
- **The file is chosen in the app's own dialogs,** opened from Rust (`couch_engine::BackupFiles`): the webview never names a path, and no window may open a dialog itself. The backup is written beside the chosen name and renamed over it, so a failure never leaves half a file where a good backup was.

**A restored room catches up first** (`couch_room::room::Room::catch_up_first`). What it holds may be older than the room: signing from it would reuse versions the members hold, and they would ignore what it signed. So it invites and signs nothing (no admission, no ban, no departure) until it has heard from **every** member on its roster: any one of them may hold the newest state, a ban made after the backup say, and the first back may hold an older one. A member's room catches up from any one member. The roster it waits on is the newest it has heard of, so someone banned after the backup stops being waited for once a member brings that ban. A member who left meanwhile is taken off the roster once it has caught up. Then `Effect::CaughtUp` clears the mark, which until then survives restarts. A room with nobody else on its roster has no one to wait for. Leaving is allowed while catching up: a departure signed from older state is ignored only by members holding newer state, which takes nothing from anyone, and a room whose members never return must still be leavable.
- **Stop waiting:** the owner may stop waiting for members who won't come back (**Stop waiting…** under the room's banner, which names who it still waits for), after a warning: a change made after the backup that only they hold, a ban among them, can be lost.
- **Then it signs ahead.** Caught up (or done waiting, or with nobody to wait for), the owner signs the room again at once, 65,536 versions ahead (`RESTORE_SKIP`), saved before the mark goes. Members it never heard from may hold what the lost PC signed after the backup: someone let in while everyone else was away, or after a backup made while the owner was alone in the room. Without the skip, the owner's next policy would reuse such a version with different contents, and the room would fork. With it, they find the newer policy: whoever only the lost PC knew is dropped (a new invite brings them back), and a change only they held is lost, never split. A room closed while catching up skips ahead the same way, so members holding newer state close it too, and so does the record a restored member leaves with.

**The restored owner sets a new password** before inviting: the room shows "came back from a backup without its password" with **Set a new password**, invites are refused until then (`EngineError::NeedsPassword`), and members already in aren't affected. An invite from before the backup is refused as expired, before and after the new password.

**Safety notes in the app:** keep the file and its passphrase apart (together they are you, in every room); use one identity on one PC at a time.

**The app** gained Tauri's dialog plugin (MIT/Apache), used from Rust only. New crates: `argon2` 0.6, `chacha20poly1305` 0.11, `zeroize`, all MIT/Apache; `cargo deny`'s licences pass.

**After review** (a review pass on the first version found these, all fixed):
- one stale snapshot ended the catching up, so a restored owner could sign a version a member away already held, and the room forked: now it waits for every member;
- the webview passed the file's path, so a script there could write the identity to any folder or share, or read any file: now the dialogs are the app's own;
- leaving was blocked while catching up, so a room whose members never return couldn't be left;
- the restore wrote the profile beside the running actor, which could create or join a room under the old identity meanwhile; a restore failing partway left half of one; a stray file in `rooms\` counted as a room; the export wasn't written atomically.

A second review pass on the reworked version found the room could still fork for members the restored owner never hears from (one let in after the backup while the others were away; any member of a room backed up while the owner was alone in it), and that closing a room while catching up was ignored by members holding newer state: hence the skip ahead. Also fixed: the one-backup-at-a-time flag is let go even if the request is dropped; undoing a restore takes back a file named twice correctly; forgetting a room removes its mark after it, never before; the passphrase is zeroized; the version is a minor bump.

No wire or IPC change: version 0.15.0 (a milestone's minor bump), still wire revision 5.

**Known limits:**
- Whoever only the lost PC knew is dropped (above), and a member let in after the backup that only members who never return heard of stays in a room that was closed meanwhile, alone.
- One identity can still run in two places: a backup restored on two PCs, or into two profiles on one. The app says to use one identity on one PC at a time; nothing enforces it.
- A room file the engine can't read (damaged, or a crafted one that fails its checks) still counts as a room, so a restore is refused until it is deleted by hand.

## Your run (2026-10-21, this PC)

Again on the final version: the backup saved through the app's own dialog, restored into an empty profile; with one member back and another away, the banner named who it still waited for; Stop waiting warned first; a new password, then an invite, and the newcomer joined. Everything as described.

On the first version (the dialogs in the webview, catching up from the first member back). Alice owned a room with Bob, backed her profile up from Settings → Backup, and closed; Bob closed too. An empty profile (dana) restored the backup and started again as Alice, owning the room: it said the room came back without its password and was catching up, and refused another restore (the profile has rooms). Bob came back: the catching up ended and they connected. A new password, then an invite: Carol joined with it. Everything as described.

## Tests

- `couch-room`: `a_restored_owner_signs_nothing_until_a_member_catches_it_up` (a ban made after the backup comes back with the catching up; nothing signed before it; then it signs above what the member holds), `a_stale_first_snapshot_doesnt_end_the_catching_up`, `the_owner_can_stop_waiting`, `a_restored_member_catches_up_from_anyone`, `a_lone_owner_restored_has_nobody_to_catch_up_with` (someone the lost PC let in is dropped, not forked), `a_member_only_the_lost_pc_knew_is_dropped_not_forked`, `closing_while_catching_up_closes_for_members_holding_newer`, `a_restored_member_leaving_while_catching_up_is_heard`, `a_departure_heard_while_catching_up_is_taken_once_caught_up`. The last four, and the lone owner's, fail with the skip set to 1.
- `couch-engine` backup: opens with its passphrase alone (trimmed, normalized), any changed byte refused, short passphrases and other files refused, a newer format named, a greedy or invalid Argon2 header refused, a file replaced whole and a huge one never read, nothing secret in its `Debug`.
- `couch-engine` store: a backup leaves out password keys, invites and rooms being left, and a restore drops them from a crafted file; restores only into a profile with no rooms, marks each room, writes no file outside the profile; a failed restore is undone; a mark without its room is swept.
- `couch-engine` rooms:
  - `an_owner_restored_from_a_backup_catches_up_and_invites_again`: without the app's dialogs a backup is refused; an invite is made, then the backup, then a ban; the PC is lost; the backup comes back into a new profile on the old port, so the old invite reaches it (a wrong passphrase refused; nothing created before the restart); owned, needs a password, catching up with both members (the ban isn't in the backup); banning and inviting refused; the old invite refused as expired, before and after a new password; still catching up after a restart; a profile with rooms refused; the member comes back with the ban, and a newcomer joins.
  - `a_restored_owner_waits_for_every_member_or_stops_waiting`: one member back isn't enough; a member can't stop the wait, the owner can, and then invites; caught up for good after a restart.
  - `a_restored_member_may_leave_while_catching_up`.
- The secrets test covers the new commands and the reply.
