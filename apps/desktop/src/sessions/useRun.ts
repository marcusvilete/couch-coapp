import { useState } from "react";
import type { Command } from "../bindings/Command";
import type { RoomView } from "../bindings/RoomView";
import { engineCommand, errorText } from "../engine";

export type Notify = (text: string, tone?: "info" | "warn") => void;
/** Resolves to whether the engine took the command. */
export type Run = (key: string, command: Command) => Promise<boolean>;

/** Runs one session command at a time, showing failures as notices; `busy`
 * names the one under way. */
export function useRun(notify: Notify, onRoom: (room: RoomView) => void): { busy: string | null; run: Run } {
  const [busy, setBusy] = useState<string | null>(null);
  const run: Run = async (key, command) => {
    setBusy(key);
    try {
      const reply = await engineCommand(command);
      if (reply.type === "room") onRoom(reply);
      return true;
    } catch (e) {
      notify(errorText(e), "warn");
      return false;
    } finally {
      setBusy(null);
    }
  };
  return { busy, run };
}
