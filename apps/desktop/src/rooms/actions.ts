// What can be done to a room and its members, one function each, shared by
// the buttons, the "⋯" menus and the right-click menus. The confirm texts
// are the ones the app has always shown.
import type { MemberView } from "../bindings/MemberView";
import type { RoomView } from "../bindings/RoomView";
import { errorText, request } from "../engine";
import type { Confirm } from "./dialogs";

/** The dialogs the app shows; rooms are named by id, so a dialog always
 * reads the room as it is now. */
export type AppDialog =
  | { kind: "add" }
  | { kind: "create" }
  | { kind: "join"; invite: string }
  | { kind: "invite"; room: string }
  | { kind: "password"; room: string }
  | { kind: "accept"; room: string }
  | { kind: "name" }
  /** `remote`: on one of our PCs, run from here (M25), by member id. */
  | { kind: "startSession"; room: string; remote?: string }
  | { kind: "streamPicker"; room: string; remote?: string }
  | { kind: "unlockRemote"; room: string; member: string }
  | ({ kind: "confirm" } & Confirm);

export type RoomActions = {
  invite: () => void;
  setPassword: () => void;
  revoke: () => void;
  leave: () => void;
  ban: (member: MemberView) => void;
  /** Owner only, restored from a backup (M13). */
  stopWaiting: () => void;
  /** Handing the room over (M13): the owner offers or withdraws; the member
   * offered accepts (with a new password) or declines. */
  offer: (member: MemberView) => void;
  withdraw: () => void;
  accept: () => void;
  decline: () => void;
  moveBy: (delta: number) => void;
};

export type Context = {
  open: (dialog: AppDialog) => void;
  notify: (text: string, tone?: "info" | "warn") => void;
  putRoom: (room: RoomView) => void;
  move: (room: string, delta: number) => void;
};

export function roomActions(room: RoomView, ctx: Context): RoomActions {
  const answer = async (run: () => Promise<RoomView>) => {
    try {
      ctx.putRoom(await run());
    } catch (e) {
      ctx.notify(errorText(e), "warn");
    }
  };
  return {
    invite: () => ctx.open({ kind: "invite", room: room.id }),
    setPassword: () => ctx.open({ kind: "password", room: room.id }),
    revoke: () =>
      ctx.open({
        kind: "confirm",
        title: "Revoke all invites?",
        body: `Every invite link to “${room.name}” you've sent stops working. Members stay in the room.`,
        action: "Revoke invites",
        run: async () => {
          await request({ type: "revokeInvites", room: room.id }, "done");
          ctx.notify(`Invites to “${room.name}” revoked.`);
        },
      }),
    leave: () =>
      ctx.open(
        room.isOwner
          ? {
              kind: "confirm",
              title: `Close “${room.name}”?`,
              body: "The room is gone for good, for everyone. Members who are offline find out when they come back.",
              action: "Close room",
              danger: true,
              run: () => request({ type: "leaveRoom", room: room.id }, "done"),
            }
          : {
              kind: "confirm",
              title: `Leave “${room.name}”?`,
              body: "To come back you'll need a new invite from the owner.",
              action: "Leave room",
              danger: true,
              run: () => request({ type: "leaveRoom", room: room.id }, "done"),
            },
      ),
    ban: (member) =>
      ctx.open({
        kind: "confirm",
        title: `Ban ${member.name}?`,
        body: `${member.name} is removed from “${room.name}” and can't come back, even with an invite. Every invite is revoked too, in case they hold one.`,
        action: "Ban",
        danger: true,
        // The engine answers with the room as it is now, without them.
        run: async () => ctx.putRoom(await request({ type: "banMember", room: room.id, member: member.id }, "room")),
      }),
    stopWaiting: () =>
      ctx.open({
        kind: "confirm",
        title: "Stop waiting for the others?",
        body:
          "Your room came back from a backup and may be older than the room is now. Members still away may hold " +
          "newer changes, a ban made after the backup among them; if you stop waiting, those can be lost. Wait if " +
          "they'll be back soon.",
        action: "Stop waiting",
        danger: true,
        run: async () => ctx.putRoom(await request({ type: "stopCatchingUp", room: room.id }, "room")),
      }),
    offer: (member) =>
      ctx.open({
        kind: "confirm",
        title: `Hand “${room.name}” over to ${member.name}?`,
        body:
          `If ${member.name} accepts, they own the room: they let people in, ban, and leaving closes it for ` +
          `everyone. They choose a new password, and the invites you sent stop working. You stay as a member, ` +
          `and can't take it back.`,
        action: "Offer the room",
        run: async () => ctx.putRoom(await request({ type: "offerRoom", room: room.id, member: member.id }, "room")),
      }),
    withdraw: () => answer(() => request({ type: "withdrawOffer", room: room.id }, "room")),
    accept: () => ctx.open({ kind: "accept", room: room.id }),
    decline: () =>
      answer(() => request({ type: "answerOffer", room: room.id, accept: false, password: null }, "room")),
    moveBy: (delta) => ctx.move(room.id, delta),
  };
}
