// Remote play (M25, ADR 0010): one of our own devices runs a home PC's
// sessions from afar. The host's console, the stream picker and the start
// dialog send their commands through `viaRemote`, which turns the host's own
// into commands for the home PC; anything else stays ours.
import type { Command } from "../bindings/Command";
import type { MemberView } from "../bindings/MemberView";
import type { RemoteAction } from "../bindings/RemoteAction";
import type { RoomView } from "../bindings/RoomView";
import type { SessionView } from "../bindings/SessionView";

/** A home PC we run from here, as met in a room. */
export type RemoteTarget = { room: string; member: string };

/** `command` as it reaches the home PC `target`, if it is one of the host's
 * own; as it is otherwise, or without a target. */
export function viaRemote(target: RemoteTarget | null | undefined, command: Command): Command {
  if (!target) return command;
  const action = actionOf(command);
  return action ? { type: "remote", room: target.room, member: target.member, action } : command;
}

function actionOf(c: Command): RemoteAction | null {
  switch (c.type) {
    case "listSources":
    case "stopStream":
    case "endSession":
    case "allowControl":
    case "stopWaitingForPads":
      return { type: c.type };
    // The home PC's own controllers stay out: nobody is there to play.
    case "startSession":
      return { type: "startSession", title: c.title, max_viewers: c.max_viewers, input: c.input };
    case "startStream":
      return {
        type: "startStream",
        source: c.source,
        name: c.name,
        fps: c.fps,
        max_mbps: c.max_mbps,
        upload_mbps: c.upload_mbps,
      };
    case "setStreamLevel":
      return { type: "setStreamLevel", percent: c.percent };
    case "answerControl":
      return { type: "answerControl", member: c.member, accept: c.accept };
    case "passControl":
      return { type: "passControl", member: c.member, seat: c.seat, controller: c.controller ?? undefined };
    case "takeBackControl":
      return { type: "takeBackControl", seat: c.seat };
    case "playSeat":
      return { type: "playSeat", seat: c.seat, controller: c.controller };
    default:
      return null;
  }
}

/** Members of this room that offered us remote play: our own PCs, and any
 * asking to be accepted as one. */
export function remotePcs(room: RoomView): MemberView[] {
  return room.members.filter((m) => m.remote != null && m.link !== "me");
}

/** The session of one of our PCs we run from here, in this room. */
export function operatedSession(room: RoomView, pc: string): SessionView | undefined {
  return room.sessions.find((s) => s.host === pc && s.operator);
}

/** "45 s", "3 min": how long until a PIN may be tried. */
export function waitText(secs: number): string {
  return secs < 90 ? `${secs} s` : `${Math.ceil(secs / 60)} min`;
}
