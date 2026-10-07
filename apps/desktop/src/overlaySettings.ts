// Settings → Overlay: whether requests and events show over the game, and
// whether a request chimes. Kept in this profile's web storage, read each
// time they matter, so a change applies at once.

const KEY = "overlay";

export type OverlaySettings = { show: boolean; sound: boolean };

const DEFAULTS: OverlaySettings = { show: true, sound: true };

export function overlaySettings(): OverlaySettings {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    if (stored && typeof stored === "object") {
      const s = stored as Partial<Record<keyof OverlaySettings, unknown>>;
      return {
        show: typeof s.show === "boolean" ? s.show : DEFAULTS.show,
        sound: typeof s.sound === "boolean" ? s.sound : DEFAULTS.sound,
      };
    }
  } catch {
    // Unreadable: the defaults.
  }
  return DEFAULTS;
}

export function setOverlaySettings(settings: OverlaySettings) {
  try {
    localStorage.setItem(KEY, JSON.stringify(settings));
  } catch {
    // Only a convenience.
  }
}
