# P5 results: two small fixes, and the future work as milestones

## What it does

**Invite links in development only when asked** (`apps/desktop/src-tauri/src/lib.rs`, `dev_links`)
- **The trouble:** a development start (default profile) registered `couchco://` for the current user, pointing at `target\debug\couch-desktop.exe`. That outranks an install for everyone. So on a PC with both, an invite link with nothing open started the debug exe, whose window needs the dev server: "connection refused" (found with the 0.10.0 installer).
- **Now:**
  - **with `COUCH_DEV_LINKS=1`,** a development build registers the links for itself, as before;
  - **without it,** a start removes this user's registration if it opens this very exe (`couch_platform_win::links::forget_user_scheme`, reached through `couch_engine::forget_own_link_registration`);
  - **an installed app's own registration,** for everyone or for just one user, points at another exe and is never touched;
  - **one exception:** an app installed for just this user writes the same per-user key, so a `COUCH_DEV_LINKS=1` start overwrites it, and it needs reinstalling afterwards.

**Capture times never later than delivery** (`couch_platform_win::capture`)
- **Checked first** (`couch-cli capture-test`):
  - **The latency clock, captured as a window:** "clock → captured" was 12.3 ms, but "clock → decoded" 11.6. So Windows' timestamp came after we had already encoded and decoded the frame.
  - **Both the window and the whole screen:** the delay from that timestamp to delivery read 0.0 at p50 and p95, meaning negative, since the tool clamps at 0. Windows seems to stamp a frame with when it will be on screen.
  - **This differed from the plan,** which expected screens to be fine. You chose to fix both.
- **Now:** a frame's capture time is the earlier of Windows' timestamp and its delivery to the capture callback.
- **Effects:**
  - **capture to present is honest:** the probe reads 2.0 to 2.6 ms instead of 0.0, and capture-test's delivery delay 0.4 ms (p95 0.9);
  - **screen captures read about 3 ms more than in earlier results,** which were low;
  - **clock to present is unaffected.**
- **Pacing** now works from delivery times; the runs below check it.

**The future work as milestones** (`docs/architecture.md`):
- **M9e, next:** fixing pacing for a capture that stalls (below);
- **M12a to M12d:** AMD and Intel GPUs, a Desktop Duplication fallback, HEVC, NVENC directly;
- **M13:** room ownership;
- **M14:** the fallback encoder;
- **M15:** an outside review of admission;
- **M16 and M17:** Linux, and macOS;
- **ideas, not planned:** voice chat, short room codes, strict control mode.

## Measurements (this PC, release build)

| Run | Result |
|---|---|
| `latency-probe` 60 fps | 60.1 fps; clock to present p50/p95 11.4/11.9 ms; capture to present 2.6/3.0 (0.0 before) |
| 100 fps | 100.0 fps; 10.9/11.5; capture to present 2.2/2.6 |
| 120 fps | 119.9 fps; 10.7/11.2; capture to present 2.0/2.3 |
| 60 fps, the clock at 61 fps (`--clock-fps 61`) | 59.3 fps; 11.6/14.4 (M9d: 59.6–59.8, p95 13.9–14.2) |
| `viewers.ps1 -Screen -Fps 120`, 4 viewers | 115.8–116.4 fps; capture to present p50 5.0–5.6, p95 6.7–6.8 ms |
| `capture-test`, the clock window | delivery 0.4 ms p50, 0.9 p95; clock → captured 8.9 p50 (12.3 before, after decoding finished) |

**A pacing regression, found here, from M9d** (`viewers.ps1 -Screen`, 4 viewers at 60 fps):

| | fps | capture to present p50 / p95 |
|---|---|---|
| Paced, three runs | 55.5–57.3 | 7.9–8.5 / 11–28 ms |
| `COUCH_HOST_PACING=0`, two runs | 59.5–59.9 | 7.2–8.1 / 10.2–10.6 ms |
| Paced, without P5's timestamp change | 56.8–57.2 | 4.8–5.8 / 9.7–21.5 ms |

- **It isn't P5's.** Without P5's change the frame rate was the same, and the p95 still reached 20 to 21 ms.
- **It comes from M9d's final pacer:** a held frame waits a whole interval, and goes out up to two intervals old when the capture stalls. The host still sent about 60 fps; the stream windows showed fewer, skipping the bunched frames.
- **M9e fixes it.** `docs/m9d-results.md` records it too.

## Checks

| Check | Result |
|---|---|
| `cargo test --workspace`, clippy, fmt | pass |
| `couch-cli gates --quick` | all pass: G1 11.4 / 12.0 ms; G3 p95 77.9 ms, drift +0.4, 19.5 Mbit/s; G4 back in 80 ms with exactly 1 keyframe; G5 settled in 0.38 s, latency back in 0.67 s; G7 the second viewer 15.6 ms (unimpaired 12.8), 20.0 Mbit/s |

## Code review

A review by a separate agent; each finding checked against the code before acting on it.

| Finding | Outcome |
|---|---|
| **A bug:** the deep-link plugin's `unregister` deletes the machine-wide `HKLM\Software\Classes\couchco` first. Without administrator rights that fails (access denied) before the user's key is reached, so the fix did nothing. With them, it would delete the installed app's registration | **Fixed:** our own `couch_platform_win::links::forget_user_scheme` reads the user's key and deletes only that, only when it opens this very exe (paths compared canonically). Tested with a throwaway scheme: another program's registration is kept, this program's goes. This PC's machine-wide registration was checked intact |
| An app installed for just one user shares the per-user key, so `COUCH_DEV_LINKS=1` overwrites it | **Documented** (CLAUDE.md, `app-manual-run`): reinstall that app afterwards |
| Since P5 the pacer gets delivery times. Frames bunched after a stall arrive back to back and collapse its slack (the shortest recent gap) | **Recorded for M9e**, a suspected second factor in the regression |
| Capture times can't go backwards: `min` of two non-decreasing clocks; the encoder's pts and the pacer handle equal times; rate control is unaffected (capture time cancels); nothing syncs audio to video capture times | Checked, nothing to do |
| The regression's p95 was overstated as "about 27 ms" (it ranged 10 to 28); `m9d-results.md` still called the 0.0 reading unexplained; the pacer's module comment and `frame_gap_ms`'s description assumed compositor times | **Fixed** |

**A second review, of the replacement alone** (`couch_platform_win::links`), found no soundness bugs. It confirmed:
- the buffer sizes;
- that a missing key reads as `ERROR_FILE_NOT_FOUND`;
- that `HKCU\Software\Classes` is the user's own hive, not the merged view, so the delete can't reach the machine-wide key;
- that the canonical path comparison matches what the plugin writes.

| Finding | Outcome |
|---|---|
| A scheme name is never checked: an empty or `\`-containing one could aim the delete at `Software\Classes` itself (unreachable today: the caller passes a constant) | **Fixed:** only URL-scheme characters are taken; a test tries five bad names |
| The test leaves a key behind if an assert fails midway | **Fixed:** a guard deletes it on drop |
| The key vanishing between the read and the delete surfaces as a warning | **Fixed:** that counts as nothing to remove |
| A registration for a build that no longer exists (another checkout's, deleted) is never removed | **Kept, and said in the doc comment:** it isn't this program's; remove it by hand |
| A value changing between the size query and the read fails with `ERROR_MORE_DATA` | **Left:** one warning, and the next start tries again |

## Your run

**Invite links in development** (with the first version, the plugin's `unregister`, later replaced after the review):
- **Without `COUCH_DEV_LINKS`,** a development start gave the links back: with the app closed, an invite link started the installed app.
- **With `COUCH_DEV_LINKS=1`,** the development build took them again.

**Again with the replacement** (`couch_platform_win::links`), the registry checked after each step:
- **Started with `COUCH_DEV_LINKS=1`:** the per-user entry pointed at the debug exe.
- **Then started from a fresh terminal without it,** with no administrator rights: the per-user entry was gone, and the installer's machine-wide one was untouched.
