// The update notice's words (M26c-2, ADR 0014): what the shell's strip says
// when a newer version is out.
import type { NewVersionView } from "../bindings/NewVersionView";

/** One line for the strip. With another wire revision, the new version plays
 * only with the same, so everyone in the user's rooms needs it. */
export function newVersionText(v: NewVersionView): string {
  return v.wireChanged
    ? `Couch Co-App ${v.version} is out. Everyone in your rooms needs it to play together.`
    : `Couch Co-App ${v.version} is out.`;
}
