// The host's board overlay over the game (M27b), run from the main window:
// the engine makes, places and focuses its window; this passes its page what
// it needs (who we are, whether we draw, the board's news) and hands the
// host's own drawing, and its Done, to the engine. The page calls no engine
// command itself.

import { useEffect, useRef } from "react";
import type { RoomView } from "../bindings/RoomView";
import { hosted } from "../model/overlay";
import { platform } from "../platform";
import type { BoardState } from "./events";

export function useBoardOverlay(rooms: RoomView[]) {
  const host = hosted(rooms);
  const overlay = host?.session.boardOverlay ?? null;
  const state: BoardState | null =
    host && overlay?.available
      ? {
          me: host.session.host,
          drawing: overlay.drawing,
          names: host.room.members.map((m): [string, string] => [m.id, m.name]),
        }
      : null;
  const latest = useRef(state);
  latest.current = state;
  const room = host?.room.id ?? null;
  const roomRef = useRef(room);
  roomRef.current = room;

  // The page loads (each time the engine opens the window): it gets the
  // state, then the board whole, which also tells the engine it may show.
  useEffect(() => {
    const board = platform().board;
    const unlisten = [
      board.onReady(() => {
        if (latest.current) board.sendState(latest.current);
        void platform()
          .command({ type: "hostBoard" })
          .catch(() => {});
      }),
      board.onOps((batch) => {
        void platform()
          .command({ type: "hostDraw", generation: batch.generation, ops: batch.ops })
          .catch(() => {});
      }),
      board.onDone(() => {
        void platform()
          .command({ type: "endHostDrawing" })
          .catch(() => {});
      }),
      platform().onEvent((event) => {
        if (event.type === "hostBoard" && event.room === roomRef.current) board.sendNews(event.news);
      }),
    ];
    return () => {
      for (const u of unlisten) void u.then((f) => f());
    };
  }, []);

  const key = JSON.stringify(state);
  useEffect(() => {
    if (latest.current) platform().board.sendState(latest.current);
    // `key` stands for the state's contents.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
}
