# P7 results: the host's stream level

## Why

Guests hear what the host's game plays, captured by WASAPI's process loopback after the game's own volume. So a host who turned the game down to play quietly turned the stream down for everyone (the `audio-checks` skill, from M3). P6 gave each guest a volume; the host had none of their own.

## What we measured first

The stream window's stats panel now ends its audio line with the level of the sound as it arrives, before that window's own volume: `level peak … / avg … dBFS`, over 250 ms windows. With it, your run (2026-10-19, this PC, the app with alice):

| On the host | The stream |
|---|---|
| the game's slider in Windows' volume mixer, muted | silent |
| Windows' master volume, muted | **unchanged** |
| the new Stream volume slider | lower, as set |

**But master volume lowers everything else too** (a Discord call, say). So to play the game quietly while guests hear it loud, the host lowers the game's slider in the volume mixer and boosts the stream to make up for it. That is why the range went from 0–100% to 0–400% (your choice after the first build: set by hand, rather than following the game's mixer slider automatically).

## What changed

**The level** (0 to 400%):
- up to 100% it turns the stream down, on the same squared curve as P6's volume (50% is -12 dB); above, it boosts by that factor (200% doubles the amplitude, +6 dB; 400%, +12 dB);
- **a soft limiter** while boosting (`soft_limit`): samples above 0.9 bend along a `tanh` toward full scale, never past it, so loud moments sound compressed rather than crackling; at 100% or below nothing is touched;
- applied to the sound before Opus, in the host pipeline's `send_sound`, from an atomic (`HostPipeline::set_sound_level`); a change ramps over the next 10 ms frame, so it never clicks; every guest hears the same;
- shared from `couch_media::audio` (`host_gain_for_percent`, `ramp_gain`, `soft_limit`, `LevelMeter`);
- saved per profile in `host-audio.json` (`Command::SetStreamLevel`), applied at each session's start and at once while one runs; the host's `SessionView.soundLevel` shows it;
- `couch-cli host --audio-level <0-400>` for the audio checks.

**In the host panel's Stream section:** a "Stream volume" slider (applied while dragging, at most every 100 ms), and a meter of what guests hear after it: the average as a bar, the peak as a mark, from -60 dB up, four times a second (`Event::StreamLevel`), "silent" when nothing is sent, "loud peaks rounded off" while the limiter works. Only guests hear the change: the host hears the game directly. Its hint: lower the game's mixer slider, then raise this until the meter is back where it was (Windows' mixer slider isn't a plain amplitude, so matching by the meter beats matching numbers).

**No wire change, no IPC change:** what members exchange and the stream window's pipe are as they were (`PROTOCOL_MINOR` 4, `IPC_VERSION` 11). The level line is the stream window's own.

## Code review

A review found the ramp continuous, the gain never above 1 from any entry (command, file, CLI), the audio callback lock- and allocation-free, and the events bounded. Fixed from it:
- **Every stream started with 10 ms at full volume:** each stream's sound loop ramped from 1.0 to the level set, so at 0% guests heard a short fade at each start or restart. It now starts at the level set.
- **The slider:** a slow reply could move it back for a moment (it now waits for every change still on its way); a failed save now says so and puts it back; a change still waiting when the panel closes is sent rather than lost.
- Smaller: one silence constant on each side; a doc comment back on its function.

A second, short review of the 400% range and its limiter found the limiter continuous at its knee, never past full scale, and the frame ramping down out of a boost still limited; the "peaks rounded off" hint now shows only at full scale. **Left as it was before P7:** at 100% or below, a captured sample already past full scale goes to Opus unbent (Opus takes it; decoders clip). Limiting there too would change the sound at unity gain.

The slider's handle stopped short of both ends: the app's text-box style (border, padding) reached it too, and is now reset for it.

## Tests

- `couch-media`: the gain ramps without a jump and lands on its target; percent to gain is squared up to 100 and the factor above, capped at 400; the limiter leaves samples below its knee alone, keeps a 4x boost within full scale with signs and order kept, and zeroes non-numbers; the meter reads peak and average (a constant, half a square wave, silence and a non-number).
- `couch-viewer`: the level passes from the audio thread in one atomic, intact.
- `couch-engine`: `host-audio.json` round-trips, caps at 400 and falls back to 100. A session scenario: the fake game's clicks (peak 0.8) read about -2 dB, -14 dB at 50%; at 400% (3.2 before the limiter) within 1 dB of full scale and never past it; 900% is capped at 400; silence at 0%; the next session starts at the level last set.
- `cargo test --workspace`, clippy, fmt, `tsc`.

## This PC (2026-10-19, release build)

`latency-probe --secs 30 --relay none --audio` (100%, the sound untouched; before the 400% range):

| Run | A1: capture to device, p95 | Click to playback, p50 | Concealed |
|---|---|---|---|
| 1 | 18 ms, PASS | 55.1 ms | 0 of 2902 |
| 2 | 21 ms, PASS | 59.6 ms | 2 of 2896 |
| 3 | 18 ms, PASS | 52.2 ms | 0 of 2898 |

Run 2 is the usual spread between sessions (P6's runs: 15 and 17 ms); the third is back in range.

## Your run (2026-10-19, this PC)

The slider lowers the stream for the guest and not for the host, as meant, and the measurements above. With the 400% range: the game turned down in the mixer and the stream boosted back sound right ("looks perfect"); the slider's handle reaches both ends after the fix.
