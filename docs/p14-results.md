# P14 results: full-screen games and Wi-Fi gaps

## Why

Your real use and a Steam Deck run (reported 2026-11-09, 0.27.x) turned up two problems on a Windows host:
- **A note minimized a game.** The "someone left" note came up over a game set to plain "Full screen" (exclusive, not "Windowed full screen"), and the game left full screen by minimizing. Any window over an exclusive full-screen game does that, our overlay included, even though it never takes the focus (M8).
- **A Deck on Wi-Fi kept losing its seat and picture quality.** The Deck watched a wired Windows host. The host's log (2026-11-06, about 12 minutes) has "viewer silent: paused" 20 times, often 2 to 6 within a few seconds, each back in 0.1 to 0.8 s. So the Deck's Wi-Fi dropped out for 0.5 to 1.3 s at a time. Each gap cost more than it should:
  - **The seat.** Over 300 ms of silence suspends the seat and releases what the guest held (the dead-man timer), which is right. But every third suspension within 5 minutes also took the seat away, so the guest had to ask again: the "controllers disconnected" the Deck saw.
  - **The bitrate.** Each pause halved it. Stream 3 opened at 10 Mbit/s and was at 1.5, the minimum, 5 s later; every later resume was at 1.5. Climbing back at 8 % a second takes about 10 s, longer than the time between bursts.

Rejected for the overlay: drawing into the game's own frames, as RivaTuner does. Anti-cheat flags such injection, the drawing would be in the stream (our overlay is kept out of capture), and it puts our code inside other programs.

No wire change: **version 0.27.4**, which still talks to 0.27.3.

## What changed

- **No overlay over a game in exclusive full screen** (Windows).
  - The engine answers `Command::FullScreenGame` from `SHQueryUserNotificationState` (`couch_platform_win::foreground::exclusive_full_screen`, through `Backend::exclusive_full_screen`; no elsewhere).
  - While hosting, the overlay asks every second, and again just before it opens with more than the banner: a game may have gone full screen since the last answer, and a window hidden in the tray polls slower. Only the latest wish acts on its answer.
  - Over such a game, only desktop control's banner comes up (`model/overlay.ts`'s `overGame`). Requests come by the chime, the host's keys and the console; join and leave notes are skipped. A borderless or windowed game keeps every toast.
  - **Your choice (2026-11-10):** the desktop-control banner stays over a full-screen game for now, though it may minimize it: whoever is at the PC must know someone controls it. To be seen in use.
  - A Linux host is unchanged: its notices are the desktop's notifications.
- **Short gaps keep the seat** (`couch_input::router`, `FLAP_AFTER`, **your choice, 2026-11-10**). The 300 ms dead-man release is unchanged: every gap still releases everything at once. Only a suspension lasting 1.5 s or more counts toward the three in 5 minutes that return the seat, counted as it reaches 1.5 s.
- **Short gaps keep the bitrate** (`couch_media::ratecontrol`, `SHORT_GAP`, **your choice, 2026-11-10**). Reports silent for less than 1.5 s resume at the bitrate from before the pause, within the ceiling the path comes back to (a relay's, a smaller upload share). A longer outage halves it as before (gate G4's 2 s blackout). If the path did shrink, the catch-up cuts by its delivery rate within a second or so.

## Checked

**The Wi-Fi gaps, reproduced on loopback** (`couch-cli host --pads 1 --netem` with six blackouts of 0.6 to 1.2 s at 15, 17, 19, 25, 26.5 and 40 s, and `view` for 50 s):

| | 0.27.3 | 0.27.4 |
|---|---|---|
| The seat | back with the host at the third gap (19 s) | kept through all six; each gap still released at 300 ms |
| Bitrate at each resume | 10 → 5 → 2.5 → 1.7 → 1.5 Mbit/s | 20 Mbit/s every time |
| Received, p50 | 2.7 Mbit/s | 19.9 Mbit/s |

**Rate-control gates** (`couch-cli gates`, full run, all pass, within the reference):

| Gate | Measured |
|---|---|
| G1 | p50 / p95 11.5 / 12.6 ms |
| G3 | p95 78.8 ms (p50 45.6), drift +0.2 ms, mean 19.9 Mbit/s, no catch-ups |
| G4 | first frame and latency back 81 ms after the 2 s outage, 1 keyframe; resumed at 10 of 20 Mbit/s (still halved) |
| G5 | settled 0.35 s, latency back 0.49 s, worst frame 320 ms, p95 39.0 ms |
| G7 | second viewer p95 12.7 ms (unimpaired 12.9), 20.0 Mbit/s; first viewer throttled to 4.3 Mbit/s |

**After the review's fixes,** `gates --quick` passed again (G1 11.7 / 14.1 ms, G4 back after 95 ms with 1 keyframe, G5 settled 0.35 s, G7 unharmed), but G3 read p95 108.9 ms at 16.2 Mbit/s, and again 108.1 ms alone, against about 79 ms before. Its trace had no pause or resume, the only place the change acts. An A/B under the same load settled it: `main`'s rate controller gave G3 p95 79.1 ms, 19.5 Mbit/s, and P14's right after it 79.2 ms, 19.5 Mbit/s. The slow runs were the PC's load at the time (a browser at about 110 % CPU), not the change.

**Input safety:** `couch-input` (new `short_gaps_release_but_keep_the_seat`; the flap tests now use suspensions that last), `couch-net`, `couch-room session`, `couch-engine --test sessions` (87) pass. `release-check`: main process killed, suspended after 240 ms; window killed, gone after 80 ms; window closed, gone after 221 ms; window frozen, suspended after 350 ms and active again after thawing; host stopped, 0 ms. All within their limits.

**Everywhere:** `cargo fmt`, `clippy -D warnings` and `cargo test --workspace` pass on Windows and on Linux (WSL, Ubuntu 24.04), 44 test binaries each; `check-docs.sh` finds nothing stale.

**The overlay:** `overGame`'s unit tests, `tsc`, the 63 front-end tests. Nothing on screen changed, so no new shots. **Not tried over a real game:** one PC can't show it here, and your run is deferred.

## The review

A fresh review agent found nothing serious, and two minor defects, both confirmed and fixed:
- **A resumed bitrate wasn't clamped to the ceiling.** A short gap restored the old bitrate before the cap was worked out, so a gap that moved the path to a relay (2 Mbit/s) would send the resume keyframe at the direct path's 20 Mbit/s. The old code had the same hole at half the rate. Now clamped (`RateController::cap`), with the test `a_short_gap_resumes_within_the_ceiling_it_comes_back_to`.
- **The full-screen answer could be a second old** (longer in the tray), so a request just after the game went full screen could still minimize it. The overlay now asks again before it opens with more than the banner, and a stale answer opens nothing.

Also fixed from it: the `app-manual-run` skill and `chime.ts` still said the overlay simply can't show over exclusive full screen.

## Still to come

- ~~**Your run over a real exclusive full-screen game**~~: done, the author's (2026-11-12): no overlay over a real game in exclusive full screen.
- **The Deck's gaps themselves** may come from its Wi-Fi power saving, which this doesn't touch: worth trying with power saving off for that network (NetworkManager's `wifi.powersave`).
- The other field findings: a Windows host with HDR on gives washed-out colours; the Deck's Desktop Mode can't host (X11, M28).
