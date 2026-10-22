// An invite link the app was opened with (any web page can open a
// `couchco://` link) goes to the join screen, and nothing is sent until the
// password is typed and Join clicked. It never replaces what the user is in
// the middle of: it waits while anything else is up, and an open join dialog
// only offers it.

/** What is up in the app when a link arrives or waits. `other`: any dialog,
 * the Settings page, the stream picker, a shortcut capture, a restart. */
export type Surface = "none" | "add" | "join" | "other";

/** A link just opened: it waits as the pending link; with something else up,
 * the user is told it will show when that closes. */
export function linkOpened(surface: Surface): { notify: boolean } {
  return { notify: surface === "other" };
}

/** The pending link opens the join dialog now: nothing is up, or only the
 * "Add a room" choice, which the join dialog replaces. */
export function opensJoin(surface: Surface): boolean {
  return surface === "none" || surface === "add";
}

/** In an open join dialog, a link offered meanwhile replaces its invite by
 * itself only while no password is typed and nothing is under way; otherwise
 * the dialog asks ("Use it instead"), so a link can't slip in under a
 * password typed for another room. */
export function takesOfferItself(dialog: { busy: boolean; password: string }): boolean {
  return !dialog.busy && dialog.password === "";
}

export const LINK_WAITS_TEXT = "An invite link was opened. It will show when you close this window.";
