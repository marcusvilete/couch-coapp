# M9a results: the cheap fixes from the Sunshine and Moonlight research

M9 (performance) follows the order suggested by [research/sunshine-moonlight.md](research/sunshine-moonlight.md):
1. **cheap fixes first (M9a, this);**
2. **zero-copy on the host (M9b);**
3. **zero-copy on the viewer (M9c);**
4. **120 fps and pacing (M9d).**

**What could be tested here.** Only NVIDIA: this PC has an RTX 3060 Ti, with the i7-14700K's Intel graphics turned off, and the other PC is NVIDIA too. Neither PC has Wi-Fi. So the AMD, Intel and Wi-Fi changes are built to be safe without being exercised. Every new encoder option falls back, in the end, to exactly the settings from before, and each change logs what it did, so a friend's `desktop.log` shows it.

## What it does

**A crash on any failed encoder open, fixed** (found while testing)
- **What happened:** `couch-cli probe` crashed (`0xC0000005`). Narrowed down: any encoder open that *fails* while carrying options crashed the process. That's AMF or Quick Sync on a PC without that GPU, but it would equally be NVENC past the driver's session limit.
- **The cause** is in rsmpeg 0.18's `AVCodecContext::open`. When `avcodec_open2` fails, FFmpeg has already freed the options dictionary and handed back a new one, and rsmpeg frees the original again: a double free.
- **What it meant before M9a:** the app tried NVENC first and, on NVIDIA, never reached the others. But on a PC with only Intel graphics, or with no GPU encoder, Go live tried AMF with options and **crashed the app**. The probe opened AMF without options, so it never showed.
- **The fix:** `options::OptionSet::open` calls `avcodec_open2` itself and frees whatever dictionary FFmpeg hands back, exactly once. It also reports the options FFmpeg didn't recognize, which the encoder logs as a warning.
- **A regression test:** `couch-codec-ffmpeg/tests/open.rs` opens every encoder twice and must not crash. It runs on any machine: which encoders open depends on the GPU, so only the absence of a crash is checked.

**Encoder options that fall back** (`couch-codec-ffmpeg/src/options.rs`)
- **Each encoder has an ordered list of option sets.** The first that opens wins. The **last is always the settings from before M9a** (a unit test checks), so no driver can leave us worse off.
- **AMD (`h264_amf`), four sets:**
  1. Sunshine's tuning: `latency`, `async_depth 1`, **`forced_idr`**, `frame_skipping 0`, no filler;
  2. the same with `usage lowlatency`: the workaround for AMF issue 410, when ultra-low-latency won't open;
  3. the old settings plus `forced_idr`;
  4. the old settings.

  `forced_idr` matters most. FFmpeg's AMF wrapper defaults it to off, so a requested keyframe may have been a plain I-frame, which a new epoch or a joining viewer can't start from cleanly.
- **Intel (`h264_qsv`):** the old settings plus `low_power` (the GPU's fixed-function encoder), then the old settings.
- **NVIDIA and Media Foundation:** unchanged; NVENC's settings already match Sunshine's.
- **Reopening for a new bitrate** (AMD and Intel reopen on every change) starts from the set that worked.
- **Logs:**
  - **The host's "encoding … with …" line** names the set, e.g. `h264_nvenc (options: p1+ull)`.
  - **`couch-cli probe`** opens each encoder exactly as a stream would, and prints its set.

**Wi-Fi media streaming mode** (`couch_platform_win::wifi`)
- **While a stream is live or watched,** every connected Wi-Fi adapter is put in Windows' media streaming mode, which stops background scans that cause jitter.
- **Where it's held:** `Backend::low_latency_network`, as a guard in `HostStream` and `Watch`.
- **Undoing it:** closing the WLAN handle undoes it, and Windows does the same if the app dies.
- **On a PC without Wi-Fi** it does nothing and logs "no WLAN service". It never fails a stream.
- **Administrator rights:** unknown. Sunshine runs as a service with full rights; the log will say whether a normal app may set it.

**The decoder fails fast** (`err_recognition = AV_EF_EXPLODE`, as Moonlight does): corrupt input becomes an error, which the viewer already answers by asking for a keyframe, instead of smeared frames until the next keyframe.

**The viewer's frame latency, measured:** unchanged at 1. `COUCH_VIEWER_FRAME_LATENCY` (1 to 3) stays as a developer switch for measuring on other GPUs, and the stats line shows the value in use.

## Measurements (this PC, release build)

**Frame latency** (`latency-probe --secs 20 --relay none`, two runs each):

| | Latency 1 | 2 | 3 |
|---|---|---|---|
| 60 fps: median / p95 | 13.5–13.6 / 15.1–15.2 ms | 13.4–13.5 / 14.2–15.1 ms | 13.5 / 14.3–14.6 ms |
| 120 fps: median / p95 | 11.8–11.9 / 13.2–13.3 ms | 11.8–12.1 / 13.2–13.3 ms | 11.7 / 13.1–13.3 ms |

- **The settings are the same within noise.** Moonlight's warning doesn't show on our setup.
- **No decode errors in any run,** so the fail-fast decoder never fired on a healthy stream.
- **An occasional ~150 ms maximum** appears at every setting: the stream's start, not the setting.
- **120 fps already works:** about 119 fps, median 11.8 ms.

**Gates** (`couch-cli gates --quick`, after all changes): all pass, matching M5a's references.

| Gate | Now | M5a |
|---|---|---|
| G1 median / p95 | 13.6 / 14.8 ms | 13.6 / 15.1 ms |
| G3 p95 under 1% loss | 83.3 ms, no drift, 19.5 Mbit/s, no catch-ups | 83.2 ms |
| G4 after a 2 s blackout | 88 ms, exactly 1 keyframe | 85 ms, 1 keyframe |
| G5 throttle to 5 Mbit/s | settled in 0.33 s, latency back in 1.0 s | 0.35 s, 0.88 s |
| G7 the unthrottled viewer | p95 14.6 ms (unimpaired 14.5), 20.0 Mbit/s | pass |

**`couch-cli probe`:**
- NVENC opens with `p1+ull`, and the software encoder with its set;
- AMD, Intel and Media Foundation fail cleanly on this PC.

Media Foundation reports "Invalid argument" here. It's never used on this PC (NVENC comes first), so it's left for when someone needs it.

## Checks

| Check | Result |
|---|---|
| Unit tests: every encoder's last set is the old one; AMD and Intel ask for real keyframes first; labels are distinct | pass |
| `tests/open.rs`: every encoder opened twice, no crash | pass |
| Wi-Fi: enabling and dropping is harmless (here: "no WLAN service") | pass |
| Workspace: all tests, clippy, fmt | pass |
| Frame-latency comparison, gates, probe | above |
| Your run | pass: alice watched the default profile; "(latency 1)" and D3D11VA in the stats line, the encoder's option set and the Wi-Fi line in the logs |

## Your run

A normal session on this PC: the default profile goes live, alice watches and gets a controller. Nothing should look different. The stream window's stats line (Ctrl+Alt+Shift+H) now says "(latency 1)". `desktop.log` shows the encoder's option set when you go live, and a Wi-Fi line ("no WLAN service" on your PCs).

**A fix from the run:** a debug build started without `RUST_LOG` (a plain cmd terminal, not `dev.ps1`) logged errors only, so the host's lines never showed. Debug builds now default to the installed app's level, `couch=info,iroh=warn`.

**Later, with friends:**
- an AMD or Intel host's log shows which option set opened;
- a Wi-Fi PC's log shows whether media streaming mode turned on;
- anyone who saw broken video when someone joined their AMD-hosted stream should see that fixed.
