// The host's board overlay (M27b): the board painted over the game, and,
// while the host draws, the editor's surface, toolbar and keys, as the stream
// window's drawing mode (`couch-viewer`'s `draw_ui.rs`). It knows nothing of
// Tauri: its page (`main.tsx`) or the preview gives it the board's news and
// takes its drawing.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { BoardNewsView } from "../bindings/BoardNewsView";
import type { BoardPoint } from "../bindings/BoardPoint";
import {
  COLOUR_NAMES,
  DRAWING_KEYS,
  type DrawAction,
  PALETTE,
  TOOLS,
  WIDTH_NAMES,
} from "../bindings/boardRules";
import { BoardView } from "../model/board";
import { Editor } from "../model/editor";
import type { BoardBatch, BoardState } from "./events";
import { FadeIcon, ToolIcon, TurnIcon } from "./icons";
import { paintBoard } from "./paint";

/** How long a note stays. */
const NOTE_MS = 3000;
/** How often waiting operations go out (the editor keeps to the host's
 * rate itself). */
const SEND_EVERY_MS = 15;

export interface BoardSurfaceProps {
  state: BoardState | null;
  /** The board's news as it comes; returns the unsubscribe. */
  subscribe: (handler: (news: BoardNewsView) => void) => () => void;
  send: (batch: BoardBatch) => void;
  /** Esc or Done: back to the game. */
  done: () => void;
  /** The preview's clock, frozen for screenshots; `performance.now`
   * otherwise. */
  clock?: () => number;
}

/** What a key does while drawing, if anything: Ctrl and Shift as held (an
 * arrow nudges whether Shift is held or not; Alt is never part of one). */
export function drawingKey(code: string, ctrl: boolean, shift: boolean): DrawAction | null {
  const exact = DRAWING_KEYS.find((k) => k.code === code && k.ctrl === ctrl && k.shift === shift);
  const nudge = DRAWING_KEYS.find((k) => k.code === code && !ctrl && k.action.type === "nudge");
  return (exact ?? nudge)?.action ?? null;
}

export function BoardSurface({ state, subscribe, send, done, clock }: BoardSurfaceProps) {
  const now = useCallback(() => (clock ?? (() => performance.now()))(), [clock]);
  const canvas = useRef<HTMLCanvasElement>(null);
  const board = useRef(new BoardView());
  const editor = useRef<Editor | null>(null);
  const me = state?.me ?? null;
  const drawing = state?.drawing === true;
  const [, setVersion] = useState(0);
  const [note, setNote] = useState<string | null>(null);
  const names = useMemo(() => new Map(state?.names ?? []), [state?.names]);
  const frame = useRef(0);
  // The timer waking the page when a fading element starts to fade.
  const wake = useRef(0);
  const held = useRef(false);

  if (me && editor.current?.me !== me) {
    editor.current = new Editor(me);
    editor.current.sync(board.current);
  }

  const say = useCallback((text: string | null) => {
    if (!text) return;
    setNote(text);
    window.setTimeout(() => setNote((n) => (n === text ? null : n)), NOTE_MS);
  }, []);

  // Painting: once per change, and every frame only while something moves.
  const paintRef = useRef<() => void>(() => {});
  const repaint = useCallback(() => {
    if (frame.current) return;
    frame.current = window.requestAnimationFrame(() => {
      frame.current = 0;
      paintRef.current();
    });
  }, []);
  paintRef.current = () => {
    const c = canvas.current;
    const ctx = c?.getContext("2d");
    if (!c || !ctx) return;
    const t = now();
    board.current.prune(t);
    const dpr = window.devicePixelRatio || 1;
    const [w, h] = [c.clientWidth, c.clientHeight];
    if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) {
      c.width = Math.round(w * dpr);
      c.height = Math.round(h * dpr);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    editor.current?.setAspect(w, h);
    paintBoard(ctx, w, h, {
      board: board.current,
      editor: drawing ? editor.current : null,
      names,
      now: t,
    });
    const told = editor.current?.takeNote();
    if (told) say(told);
    window.clearTimeout(wake.current);
    if (board.current.moving(t)) {
      repaint();
    } else {
      // A fading element stays still a while, then fades: nothing asks for
      // a frame when it starts to, so wake then.
      const next = board.current.nextFade(t);
      if (next !== null) wake.current = window.setTimeout(repaint, Math.max(0, next - t) + 5);
    }
  };
  const changed = useCallback(() => {
    setVersion((v) => v + 1);
    repaint();
  }, [repaint]);

  // The board's news.
  useEffect(
    () =>
      subscribe((news) => {
        const t = now();
        const b = board.current;
        const refused = b.news(news, me, t);
        if (refused) say(refused);
        const e = editor.current;
        if (e) {
          const ours =
            (news.type === "snapshot" && news.first) ||
            (news.type === "cleared" && (news.author === null || news.author === e.me));
          if (ours) {
            e.cleared(b);
            e.finishStrays(b, t);
            // Not drawing, no timer runs: what that finished goes now.
            const batch = e.takeBatch(t);
            if (batch) send(batch);
          } else {
            e.sync(b);
          }
        }
        changed();
      }),
    [subscribe, me, now, say, changed, send],
  );

  // Our operations go out as the editor lets them: while drawing, and after
  // it until the last (a stroke finished as it ended) has gone. Nothing
  // runs while the board only shows.
  useEffect(() => {
    const timer = window.setInterval(() => {
      const e = editor.current;
      const batch = e?.takeBatch(now());
      if (batch) send(batch);
      if (!drawing && !e?.pending()) window.clearInterval(timer);
    }, SEND_EVERY_MS);
    return () => window.clearInterval(timer);
  }, [drawing, send, now]);

  // Drawing begins with the last tool, and ends with what's half done
  // finished.
  useEffect(() => {
    const e = editor.current;
    if (!e) return;
    if (drawing) {
      e.begin();
      canvas.current?.focus();
    } else {
      e.end(board.current, now());
      held.current = false;
    }
    changed();
  }, [drawing, now, changed]);

  useEffect(() => {
    const resize = () => repaint();
    window.addEventListener("resize", resize);
    repaint();
    return () => window.removeEventListener("resize", resize);
  }, [repaint]);

  // Keys go to the board while drawing, never anywhere else; the page's own
  // keys (reload, print, find) never work.
  useEffect(() => {
    const key = (ev: KeyboardEvent) => {
      ev.preventDefault();
      const e = editor.current;
      if (!drawing || !e || ev.type !== "keydown") return;
      const action = drawingKey(ev.code, ev.ctrlKey || ev.metaKey, ev.shiftKey);
      if (!action || ev.altKey) return;
      if (e.key(action, ev.shiftKey, board.current, now()) === "leaveDrawing") done();
      changed();
    };
    const menu = (ev: Event) => ev.preventDefault();
    window.addEventListener("keydown", key);
    window.addEventListener("keyup", key);
    window.addEventListener("contextmenu", menu);
    return () => {
      window.removeEventListener("keydown", key);
      window.removeEventListener("keyup", key);
      window.removeEventListener("contextmenu", menu);
    };
  }, [drawing, done, now, changed]);

  const at = (ev: React.PointerEvent): BoardPoint => {
    const r = (ev.currentTarget as HTMLElement).getBoundingClientRect();
    const fit = (v: number, size: number) => Math.round(Math.min(1, Math.max(0, v / size)) * 65535);
    return { x: fit(ev.clientX - r.left, r.width), y: fit(ev.clientY - r.top, r.height) };
  };
  const onDown = (ev: React.PointerEvent) => {
    const e = editor.current;
    if (!drawing || !e) return;
    ev.preventDefault();
    if (ev.button === 1) {
      e.ping(at(ev), board.current, now());
    } else if (ev.button === 0) {
      ev.currentTarget.setPointerCapture(ev.pointerId);
      held.current = true;
      e.press(at(ev), ev.shiftKey, board.current, now());
    }
    changed();
  };
  const onMove = (ev: React.PointerEvent) => {
    const e = editor.current;
    if (!drawing || !e || !held.current) return;
    e.drag(at(ev), ev.shiftKey, board.current, now());
    repaint();
  };
  const onUp = (ev: React.PointerEvent) => {
    const e = editor.current;
    if (!drawing || !e || !held.current || ev.button !== 0) return;
    held.current = false;
    e.release(at(ev), ev.shiftKey, board.current, now());
    changed();
  };

  const e = editor.current;
  const pick = (f: (e: Editor) => void) => {
    if (!e) return;
    f(e);
    changed();
  };

  return (
    <div className={drawing ? "board drawing" : "board"}>
      <canvas
        ref={canvas}
        className="board-canvas"
        tabIndex={-1}
        onPointerDown={onDown}
        onPointerMove={onMove}
        onPointerUp={onUp}
        onPointerCancel={onUp}
      />
      {drawing && (
        <>
          <div className="board-banner" role="status">
            <strong>Drawing over the game</strong>
            <span>{note ?? "Esc or Done to go back to the game"}</span>
          </div>
          {e && (
            <div className="board-toolbar" role="toolbar" aria-label="Drawing">
              <div className="board-row">
                {TOOLS.map(({ tool, name, key }) => {
                  return (
                    <button
                      key={tool}
                      type="button"
                      className={e.tool === tool ? "board-button on" : "board-button"}
                      title={`${name} · ${key}`}
                      aria-label={name}
                      aria-pressed={e.tool === tool}
                      onClick={() => pick((x) => x.setTool(tool, board.current, now()))}
                    >
                      <ToolIcon tool={tool} laser={rgb(e.colour)} />
                    </button>
                  );
                })}
              </div>
              <div className="board-row">
                {COLOUR_NAMES.map((name, i) => (
                  <button
                    key={name}
                    type="button"
                    className={e.colour === i ? "board-swatch on" : "board-swatch"}
                    title={name}
                    aria-label={name}
                    aria-pressed={e.colour === i}
                    onClick={() => pick((x) => x.setStyle({ colour: i }, board.current, now()))}
                  >
                    <span style={{ background: rgb(i) }} />
                  </button>
                ))}
                <span className="board-gap" />
                {WIDTH_NAMES.map((name, i) => (
                  <button
                    key={name}
                    type="button"
                    className={e.width === i ? "board-button on" : "board-button"}
                    title={name}
                    aria-label={name}
                    aria-pressed={e.width === i}
                    onClick={() => pick((x) => x.setStyle({ width: i }, board.current, now()))}
                  >
                    <span className="board-dot" style={{ width: 4 + 4 * i, height: 4 + 4 * i }} />
                  </button>
                ))}
                <span className="board-gap" />
                {/* Staying or fading, and how long: one button (the author's
                    choice, M27b). A click goes round stays, 2, 4, 8, 15 s;
                    the wheel over it steps up or down. */}
                <button
                  type="button"
                  className={e.fades ? "board-fade on" : "board-fade"}
                  title={
                    e.fades
                      ? `Fades away after ${e.stay} seconds · click: longer, then stays · wheel: longer or shorter`
                      : "Stays until erased · click: fades away · wheel: longer or shorter"
                  }
                  aria-label={e.fades ? `Fades away after ${e.stay} seconds` : "Stays until erased"}
                  aria-pressed={e.fades}
                  onClick={() => pick((x) => x.stepFade("next", board.current, now()))}
                  onWheel={(ev) => {
                    if (ev.deltaY === 0) return;
                    const step = ev.deltaY < 0 ? "longer" : "shorter";
                    pick((x) => x.stepFade(step, board.current, now()));
                  }}
                >
                  <FadeIcon />
                  {e.fades && <span>{e.stay}s</span>}
                </button>
                <span className="board-gap" />
                <button
                  type="button"
                  className="board-button"
                  title="Undo · Ctrl+Z"
                  aria-label="Undo"
                  onClick={() => pick((x) => x.key({ type: "undo" }, false, board.current, now()))}
                >
                  <TurnIcon forward={false} />
                </button>
                <button
                  type="button"
                  className="board-button"
                  title="Redo · Ctrl+Shift+Z"
                  aria-label="Redo"
                  onClick={() => pick((x) => x.key({ type: "redo" }, false, board.current, now()))}
                >
                  <TurnIcon forward />
                </button>
                <button type="button" className="board-done" title="Back to the game · Esc" onClick={done}>
                  Done
                </button>
              </div>
              <div className="board-hints">
                Shift: straight lines, squares and circles · middle click: ping · Esc: back to the game
              </div>
            </div>
          )}
        </>
      )}
    </div>
  );
}

function rgb(index: number): string {
  const [r, g, b] = PALETTE[index] ?? PALETTE[0];
  return `rgb(${r}, ${g}, ${b})`;
}
