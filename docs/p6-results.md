# P6 results: the stream volume

## Why

A guest could change how loud a stream plays only in Windows' volume mixer, for the whole stream window. Nothing in the window itself, and nothing remembered between sessions.

## What changed

**In the stream window** (`couch-viewer`):
- **The bar** has Mute and a volume slider, 0 to 100% (your choice): never louder than the host sent it, so it can't clip.
- **Three new shortcuts** in the one table (`couch_input::shortcuts`): Ctrl+Alt+Shift+`-` turns it down by 10%, `=` up (unmuting), `0` mutes or unmutes. They show a note ("Volume 70%", "Muted (volume 70%)"), are listed and movable in Settings → Shortcuts like the others, and, as chords, are never forwarded to the game.
- **Applied on the audio thread** after decoding, from an atomic: no lock in the callback. The gain is the level squared, so equal steps sound about equal (50% is −12 dB; linear steps barely changed it near the top and halved it near the bottom). A change ramps over the next buffer (about 10 ms), so it never clicks. At full volume the samples are left as they came.

**Remembered per profile** (`audio.json` in the profile folder, written by the engine):
- the main process sends it to every stream window right after the shortcut keys and before any sound (IPC `VOLUME`); the pipe reader applies it at once, so a muted profile never hears a burst while the window starts. A restarted window gets the latest;
- the window sends a change back (`VOLUME_CHANGED`): a shortcut or Mute at once, the slider once the user lets it rest for half a second (not at every step of a drag), and before `Leave`. The engine saves it even when the watch has just ended. A slider change in the last half second before the host ends the session can still be lost;
- a level read from a file or the pipe is kept within 0 to 100%.

**IPC v11.** No wire change: what members exchange is the same (`PROTOCOL_MINOR` stays 4).

**Version 0.11.0**, for the next friends' installer: it carries M12c, M19, M18 and P6, and refuses 0.10.x builds (wire revision 4).

**Left out** (your choice): a level on the host for what guests hear. The game's own slider and the volume mixer already lower what we capture (the `audio-checks` skill, from M3's listening), so a host can't play quietly without the stream going quiet; a gain before Opus would fix that, as a later candidate.

**The new default keys can displace a user's own.** Someone who had moved another shortcut to Ctrl+Alt+Shift+`-`, `=` or `0` finds it back on its default key (logged), since the new rows take those keys. Move it again in Settings.

## Code review

A review found the audio callback safe (no lock, allocation or panic path), the keys allowed and never forwarded, the version bump complete, and the order on close right. Fixed from it:
- **A muted profile heard full volume while a window started** (the level went only to the window's loop, which starts after the GPU is set up): the pipe reader now applies it before any sound.
- **A change could be lost when the host ended the session** within half a second of it: shortcuts and Mute are now remembered at once (the slider still waits for its rest); a flush that never ran is gone.
- **Levels above 100% from a hand-edited file** are clamped where they're read.
- **Linear steps sounded uneven:** the gain is now the level squared.
- **`docs/protocol.md`'s IPC table** lists the new frames, and M18's (seat state, rumble, controller slots, giving back one seat), which it had missed.

## Tests

- `couch-proto`: the volume's steps stay within 0 to 100%, turning down keeps it muted and up unmutes, a value above 100 plays at full volume, and it round-trips.
- `couch-viewer`: the gain ramps across a buffer, both channels alike, ending at the new level; a held level scales every sample; full volume leaves samples untouched.
- `couch-engine`: `audio.json` round-trips, and a missing or damaged file means full volume.
- `couch-input`: the three new rows are in the table, valid, with unique ids.
- `cargo test --workspace`, clippy, fmt, `tsc`.

## This PC (2026-10-16, release build)

`latency-probe --secs 30 --relay none --audio`, twice (full volume, as `couch-cli view` always plays):

| Run | A1: capture to device, p95 | Click to playback, p50 | Concealed | G1 |
|---|---|---|---|---|
| 1 | 15 ms, PASS | 53.7 ms (59 of 61 clicks) | 0 of 2900, no late, no underruns | PASS |
| 2 | 17 ms, PASS | 55.0 ms (59 of 61 clicks) | 0 of 2898 | PASS (p50 11.1 ms) |

The sound path is as before: the gain leaves full-volume samples untouched.

## Your run (2026-10-16, this PC, the app with alice)

The volume works: the slider, Mute and the shortcuts change the stream window's sound.
