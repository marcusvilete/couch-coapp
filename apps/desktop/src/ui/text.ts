// Small text rules shared by the screens.

/** Names travel as UTF-8, at most this many bytes (`couch_proto::room`). */
export const MAX_NAME_BYTES = 96;

/** `text` cut to at most `max` UTF-8 bytes, between characters, with an
 * ellipsis if anything was cut. */
export function truncateBytes(text: string, max: number = MAX_NAME_BYTES): string {
  const encoder = new TextEncoder();
  if (encoder.encode(text).length <= max) return text;
  const ellipsis = "…";
  let room = max - encoder.encode(ellipsis).length;
  let out = "";
  for (const char of text) {
    const size = encoder.encode(char).length;
    if (size > room) break;
    out += char;
    room -= size;
  }
  return out.trimEnd() + ellipsis;
}

export function nameProblem(name: string): string | null {
  if (name.trim() === "") return "Type a name.";
  if (new TextEncoder().encode(name.trim()).length > MAX_NAME_BYTES) return "That name is too long.";
  return null;
}

/** "3 days", "5 hours", "12 minutes", "40 seconds". */
export function duration(secs: number): string {
  const units: [number, string][] = [
    [86400, "day"],
    [3600, "hour"],
    [60, "minute"],
  ];
  for (const [size, unit] of units) {
    if (secs >= size) {
      const n = Math.round(secs / size);
      return `${n} ${unit}${n === 1 ? "" : "s"}`;
    }
  }
  const n = Math.max(1, Math.round(secs));
  return `${n} second${n === 1 ? "" : "s"}`;
}
