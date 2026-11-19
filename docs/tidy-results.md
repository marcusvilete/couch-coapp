# The pre-1.0 tidy pass

## Why

Before 1.0, one pass removes what the app no longer needs, so 1.0 starts from the code it actually runs. rustc never calls a `pub` item of a library crate unused, and almost everything here was `pub`; tests, the test kit and the fake backend keep more looking alive. Planned as a dead-code pass on 2026-11-11, widened on 2026-11-18.

**Your choices:**
- (2026-11-11) one pass near feature-completeness, before 1.0, rather than as we go;
- (2026-11-18) also a pass over the tests for stale or useless ones, and one applying DRY where it fits;
- (2026-11-18) the tools: `cargo-machete`, `cargo-llvm-cov`, `knip`, `jscpd`;
- (2026-11-18) the measuring switches of settled comparisons go; the code that exists only to read what earlier versions wrote goes too, and **rooms and profile data from before 1.0 may be lost**; then tag 1.0;
- (2026-11-18) the profile files' `serde(default)`s stay: a field added after 1.0 must not reset the user's other settings in its file;
- (2026-11-18) narrow every unreachable `pub` and keep `unreachable_pub` on.

Three pull requests: **A**, dead code, compatibility code and switches (0.42.1); **B**, the tests; **C**, DRY. Then 1.0.

## Part A (0.42.1)

### How it was found

- **Inside crates:** clippy's fix for `unreachable_pub` narrowed 379 items to `pub(crate)` (Windows, and 3 more in Linux-only code in WSL); rustc's dead-code lint then flagged nothing, not even code only tests use. The lint stays on (`[workspace.lints.rust]`), so from now on rustc sees it. NVENC's generated bindings are left out.
- **Across crates:** `scripts/tidy/unused-pub.sh` lists each library's `pub` items whose name appears nowhere else in the workspace (`TESTS_ONLY=1`: nowhere but tests). It found `VideoReceiver` and `AudioSink`, two traits from M1 nothing implements (and `ReceivedFrame`, which only the first used), and `DEFAULT_MAX_VIEWERS`, which nothing read. Used by tests alone: four engine methods and a `couch-net` setter, the scenarios' seams; they stay.
- **Dependencies** (`cargo-machete`, each checked by hand): `couch-cli` → `couch-codec-opus`, `couch-codec-ffmpeg` → `thiserror`, `couch-net` → `iroh-tickets`, `couch-testkit` → `couch-proto`.
- **The front end** (`knip`, given the four pages' scripts as entries: it doesn't follow the HTML): no unused file or package; 25 exports and 15 types nothing imported. 38 were used inside their own file and lost `export`; `whoPlays` and the preview's `fakeRelayCode` were used nowhere and went.

### What went

**Code for what earlier versions wrote** (profiles from before 1.0 start fresh):

| What | It read | Since |
|---|---|---|
| room files `CCR2` and the store's `legacy` module | rooms saved before handovers (such a file is now set aside as `*.room.old`) | M13 (0.16) |
| backup formats 1 and 2 | backups made before M13 and before M26a; now refused with "made before Couch Co-App 1.0" | M13, M26a |
| `network.json`, carried into `relays.sealed` | the relay setting before registered relays | M26a (0.35) |
| a keyframe request with no reason, taken as a decoder error | stream windows before 0.42 (one without a reason is now ignored, logged) | M12d-2 |
| the UI's `myPad` key and `withController1` | the single controller before `myPads`; the input setup before M24 | M23, M24 |

**Measuring switches** whose comparisons are settled, with the code only they reached:

| Switch | What it compared | Since |
|---|---|---|
| `COUCH_HOST_ZERO_COPY` | the capture through memory (the memory capture stays, for when frames can't stay on the GPU) | M9b |
| `COUCH_HOST_PACING` | the capture unpaced: `CaptureRequest::paced` and the unpaced paths are gone, the pacer always runs | M9d |
| `COUCH_HOST_MEMORY_PACING` | a host whose encoders take memory frames capturing in memory | M12a |
| `COUCH_HOST_MAPPED_FRAMES` | the read-back's memcpy (it stays as the fallback past 16 mapped frames) | M12a |
| `COUCH_CAPTURE_BUFFERS` | one capture buffer (two, a constant now) | M9f |
| `COUCH_VIEWER_SCALER` | the bilinear scaler: its two pipelines and shader path are gone | P8 |
| `COUCH_VIEWER_FRAME_LATENCY` | the swap chain's frame latency (1) | M9a |
| `COUCH_NVENC_PRESET`, `_SPATIAL_AQ`, `_AQ_STRENGTH`, `_MULTIPASS` | NVENC's quality settings over FFmpeg's (P8 kept none): `OptionSet::open_with` folded into `open` | P8 |
| `COUCH_HOST_HDR` | the 8-bit capture of a monitor in HDR (Settings → Streaming keeps its switch) | M29 |

`viewers.ps1 -CopyThroughMemory` went with the first. Kept: `COUCH_HOST_CODEC` (the gates use it), `COUCH_HOST_ENCODER`, `COUCH_VIEWER_ZERO_COPY` (`=1` tries zero-copy on another GPU), `COUCH_VIEWER_HEVC`, `COUCH_HOST_NVENC_DIRECT` and `COUCH_HOST_CONTINUE`, with `capture-test --ffmpeg-nvenc` and `viewers.ps1 -CopyFrames`, which compare against them.

**M12d-2's spike:** the two NVENC configurations that weren't shipped (the driver's references with LTR, and a DPB of 8 with 4 references), with the settings only they used (`Continuation::max_ref_frames`, `FrameControl::use_ltr`). Kept: `capture-test --recover-every`, the test's production case and its "invalidation alone" case, and `COUCH_SPIKE_SIZE`.

## Checked

- Windows: `cargo fmt`, clippy with `-D warnings` (and the NVENC GPU tests' build), every test (48 binaries), the GPU tests (`couch-codec-nvenc`'s continuation and gpu, `couch-codec-ffmpeg`'s gpu_input, `couch-platform-win`'s ignored `gpu_frames`), the front end's `tsc` and 117 unit tests. Linux (WSL): `cargo fmt`, clippy with `-D warnings` and every test (48 binaries).
- `scripts/check-docs.sh`: nothing stale. CLAUDE.md, the `performance-checks`, `rate-control-gates`, `app-manual-run` and `room-security-checks` skills, `architecture.md` and `protocol.md` no longer name what went.

## Found while checking

- **Git Bash's `sed -i` writes LF** into this checkout's CRLF files; and a Perl substitution with `|` as its delimiter turns `\|` back into alternation, which once spliced text into a file's first line (caught by reading every diff). Every edit here was read back as a diff.
- **`cargo-machete` reads source text,** so each finding was checked by hand; all four held.
- **`knip` and `jscpd` need settings to see everything:** `knip` the pages' scripts as entries, `jscpd` `--max-size` and `--max-lines` lifted (by default it skips files over 1,000 lines, most of the engine).
- **Coverage** (`cargo llvm-cov`, Windows): 61.5% of lines; what no test reaches is the CLI tools, the Tauri shell and platform code that needs hardware or a desktop. For part B.

## The review

A fresh review agent read the whole diff (and ran clippy, with the GPU tests' build, and the docs check). It found no behaviour beyond the intent and no accidental removal, and checked each switch against its old default: the pacing now always on and `in_memory` reduced to the capture's own flag (both defaults were on, and nothing ever set `paced: false`), zero copy and the mapped read-back on with their fallbacks kept, two buffers, HDR by Settings alone, bicubic on luma in both pipelines, a frame latency of 1, NVENC opened with nothing laid over it; a fresh profile's relays and format-3 backups as before; nothing narrowed or un-exported that anything outside needed. What it found, checked and held:

- **The `room-security-checks` skill still named the removed code** (medium) for a change to `SavedRoom`: it now says what holds after 1.0 (the previous format still read and converted, and the backup's previous format still opening), since a user's rooms must survive upgrades from then on.
- **A room file from before 1.0 was skipped but still counted as a room** (low): a restore into that profile was refused (`HasRooms`) while no room showed, and it couldn't be forgotten. Such a file (ours, unsealed, not `CCR3`) is now set aside as `*.room.old`, with a test; a file another Windows user sealed stays as before.
- **Stale wording** (low): CLAUDE.md's pacing-trace row (nothing is unpaced now), `architecture.md`'s P8 row (the NVENC switches) and the tidy row's count (12 switches, not 13), this page's version for `CCR3` (0.16) and its Linux line, two sentences in the skills.
- Not changed: older results pages and ADRs (0001's `VideoReceiver`, 0014's `network.json`) keep what was true then.

## Still to come

- **Part B, the tests:** stale or useless ones (a duplicate assertion, behaviour that's gone, one that can't fail, an ignored test nothing tells anyone to run, `print_now` in the Linux clock), each kept, merged or removed with its reason here.
- **Part C, DRY:** `jscpd` found 0.35% duplication, 41 clones, mostly tests' setup. In production code: the engine's command mapping beside `couch-proto`'s remote ops, a connection lookup in three room modules, the two platforms' clocks, the two SDL build scripts, some Windows device-tree code, a front-end control, and the env-switch pattern.
- **1.0:** the version, and the release notes saying profiles from before 1.0 start fresh.
