// The chime when someone asks for a seat (M8): two short tones, made with
// Web Audio, so no sound file ships. In true exclusive full screen the
// overlay can't show; this is how the host hears of a request there.
//
// The webview lets a page start sound only after a click, so the audio
// context is made by the Go live click (`prepareChime`) and kept.

import { overlaySettings } from "./state/overlaySettings";

let context: AudioContext | null = null;

/** Call from a click (Go live), so the chime may play later. */
export function prepareChime() {
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
  if (!context || !overlaySettings().sound) return;
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
