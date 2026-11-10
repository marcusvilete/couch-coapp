// The chime when someone asks for a seat (M8): two short tones, made with
// Web Audio, so no sound file ships. Over a game in exclusive full screen the
// overlay doesn't show (`model/overlay.ts`'s `overGame`); this is how the
// host hears of a request there. Guests
// never hear it: a Windows host streams the game's sound or everything but
// this app, and a Linux host leaves this app's processes out (M16c).
//
// On Linux the engine plays it (`Command::PlayChime`): the AppImage's
// webview has no sound output (no GStreamer plugins it can load).
//
// The webview lets a page start sound only after a click, so the audio
// context is made by the Go live click (`prepareChime`) and kept.

import { request } from "./engine";
import { overlaySettings } from "./state/overlaySettings";

let context: AudioContext | null = null;
let byEngine = false;

/** On a host whose webview can't play sound (Linux), from the engine's info. */
export function chimeByEngine(on: boolean) {
  byEngine = on;
}

/** Call from a click (Go live), so the chime may play later. */
export function prepareChime() {
  if (byEngine) return;
  try {
    context ??= new AudioContext();
    // Unlocked by this click; held only while a chime plays.
    void context.resume().then(() => context?.suspend());
  } catch {
    // No audio device: no chime.
  }
}

/** Two rising tones, about 0.35 s, unless turned off in Settings. */
export function playChime() {
  if (!overlaySettings().sound) return;
  if (byEngine) {
    // Only a convenience.
    request({ type: "playChime" }, "done").catch(() => {});
    return;
  }
  if (!context) return;
  try {
    void context.resume();
    const start = context.currentTime + 0.02;
    for (const [i, frequency] of [660, 880].entries()) {
      const at = start + i * 0.14;
      const tone = context.createOscillator();
      const level = context.createGain();
      tone.type = "sine";
      tone.frequency.value = frequency;
      level.gain.setValueAtTime(0.0001, at);
      level.gain.exponentialRampToValueAtTime(0.25, at + 0.015);
      level.gain.exponentialRampToValueAtTime(0.0001, at + 0.2);
      tone.connect(level).connect(context.destination);
      tone.start(at);
      tone.stop(at + 0.22);
    }
    // Let go of the sound device between chimes; the next one resumes.
    const ctx = context;
    window.setTimeout(() => void ctx.suspend().catch(() => {}), 800);
  } catch {
    // Only a convenience.
  }
}
