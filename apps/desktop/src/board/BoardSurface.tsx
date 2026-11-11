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
  TEXT_SIZE_NAMES,
  TOOLS,
  WIDTH_NAMES,
} from "../bindings/boardRules";
import { BoardView } from "../model/board";
import { Editor, type TextKey } from "../model/editor";
import type { BoardBatch, BoardState } from "./events";
import { ChevronIcon, FadeIcon, ToolIcon, TurnIcon } from "./icons";
import { canvasMeasure, paintBoard } from "./paint";

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

/** Where the page remembers the toolbar hidden. */
const TOOLS_HIDDEN = "couch.board.toolsHidden";

/** What a key does in a text box, by `KeyboardEvent.key`, if anything (its
 * letters come as text). The stream window's `text_key`. */
export function textKey(key: string, ctrl: boolean): TextKey | null {
  switch (key) {
    case "Escape":
      return "finish";
    case "Enter":
      return ctrl ? "finish" : "enter";
    case "Backspace":
      return "backspace";
    case "Delete":
      return "delete";
    case "ArrowLeft":
      return "left";
    case "ArrowRight":
      return "right";
    case "ArrowUp":
      return "up";
    case "ArrowDown":
      return "down";
    case "Home":
      return "home";
    case "End":
      return "end";
    default:
      return null;
  }
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
  // Where the pointer last was over the board: where pasted text goes.
  const pointer = useRef<BoardPoint | null>(null);
  // The keys' own field (M27c): focused all through drawing, so typing,
  // input methods and the clipboard's events reach the page; it shows
  // nothing (the canvas paints the text).
  const input = useRef<HTMLTextAreaElement>(null);
  const [preedit, setPreedit] = useState("");
  // The toolbar hidden to a pill (its handle or Tab, M27c), remembered on
  // this PC: what it covered can then be drawn on and picked.
  const [toolsHidden, setToolsHidden] = useState(() => {
    try {
      return window.localStorage.getItem(TOOLS_HIDDEN) === "1";
    } catch {
      return false;
    }
  });
  useEffect(() => {
    try {
      window.localStorage.setItem(TOOLS_HIDDEN, toolsHidden ? "1" : "0");
    } catch {
      // Not remembered: it still works for now.
    }
  }, [toolsHidden]);

  if (me && editor.current?.me !== me) {
    editor.current = new Editor(me);
    editor.current.setMeasure(canvasMeasure());
    editor.current.sync(board.current);
  }

  const say = useCallback((text: string | null) => {
    if (!text) return;
    setNote(text);
    window.setTimeout(() => setNote((n) => (n === text ? null : n)), NOTE_MS);
  }, []);

  const preeditRef = useRef("");
  preeditRef.current = preedit;

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
      // Only while a box is typed in: a composition cut short by the box
      // ending never shows at the next one's caret.
      preedit: drawing && editor.current?.typing() ? preeditRef.current : "",
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
      input.current?.focus();
    } else {
      e.end(board.current, now());
      setPreedit("");
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
      const e = editor.current;
      const ctrl = ev.ctrlKey || ev.metaKey;
      if (drawing && e?.typing() && ev.type === "keydown") {
        // A text box has the keys (M27c). An input method or a dead key
        // composing keeps them; characters (AltGr's too) go into the
        // field, Ctrl+V to its paste; the box's own keys to the editor.
        if (ev.isComposing || ev.key === "Process" || ev.key === "Dead") return;
        // Alt with them does nothing, as in the stream window (AltGr's
        // characters still come as text).
        const k = ev.altKey ? null : ctrl && ev.code === "KeyA" ? "selectAll" : textKey(ev.key, ctrl);
        if (k) {
          ev.preventDefault();
          e.textKey(k, board.current, now(), ev.shiftKey, ctrl);
          changed();
        } else if (ctrl && !ev.altKey && ["KeyV", "KeyC", "KeyX"].includes(ev.code)) {
          // The page's own paste, copy and cut carry the clipboard.
          return;
        } else if (Array.from(ev.key).length > 1 || (ctrl && !ev.altKey)) {
          ev.preventDefault();
        }
        return;
      }
      if (drawing && e && ev.type === "keydown" && !ev.altKey) {
        const action = drawingKey(ev.code, ctrl, ev.shiftKey);
        // Copying and pasting go through the page's own copy and paste,
        // which carry the system clipboard.
        if (action?.type === "copy" || action?.type === "paste") return;
        ev.preventDefault();
        if (!action) return;
        const keyed = e.key(action, ev.shiftKey, board.current, now());
        if (keyed === "leaveDrawing") done();
        // Tab held repeats: the toolbar folds once.
        if (keyed === "toggleToolbar" && !ev.repeat) setToolsHidden((h) => !h);
        changed();
        return;
      }
      ev.preventDefault();
    };
    // Nothing to copy, the clipboard keeps what it had: the default action
    // copies the keys' field's selection, which is always empty.
    // Typing, the note's selected text; otherwise our drawings.
    const copy = (ev: ClipboardEvent) => {
      const e = editor.current;
      const text = drawing && e ? (e.typing() ? e.copyText() : e.copy(board.current)) : null;
      if (text === null) return;
      ev.preventDefault();
      ev.clipboardData?.setData("text/plain", text);
    };
    const paste = (ev: ClipboardEvent) => {
      ev.preventDefault();
      const e = editor.current;
      if (!drawing || !e) return;
      e.paste(ev.clipboardData?.getData("text/plain") ?? null, pointer.current, board.current, now());
      changed();
    };
    // Only a note's selected text is cut; drawings never are.
    const cut = (ev: ClipboardEvent) => {
      ev.preventDefault();
      const e = editor.current;
      const text = drawing && e?.typing() ? e.cutText(board.current, now()) : null;
      if (text === null) return;
      ev.clipboardData?.setData("text/plain", text);
      changed();
    };
    const menu = (ev: Event) => ev.preventDefault();
    window.addEventListener("keydown", key);
    window.addEventListener("keyup", key);
    window.addEventListener("copy", copy);
    window.addEventListener("paste", paste);
    window.addEventListener("cut", cut);
    window.addEventListener("contextmenu", menu);
    return () => {
      window.removeEventListener("keydown", key);
      window.removeEventListener("keyup", key);
      window.removeEventListener("copy", copy);
      window.removeEventListener("paste", paste);
      window.removeEventListener("cut", cut);
      window.removeEventListener("contextmenu", menu);
    };
  }, [drawing, done, now, changed]);

  // What's typed or composed in the keys' field goes into the text box; the
  // field stays empty.
  const onInput = (ev: React.FormEvent<HTMLTextAreaElement>) => {
    const field = ev.currentTarget;
    if ((ev.nativeEvent as InputEvent).isComposing) return;
    const text = field.value;
    field.value = "";
    if (text) editor.current?.typeText(text, board.current, now());
    changed();
  };
  const onComposed = (ev: React.CompositionEvent<HTMLTextAreaElement>) => {
    const field = ev.currentTarget;
    const text = field.value;
    field.value = "";
    setPreedit("");
    if (text) editor.current?.typeText(text, board.current, now());
    changed();
  };
  // What an input method composes shows at the caret as it changes (the
  // field itself shows nothing).
  const onComposing = (ev: React.CompositionEvent<HTMLTextAreaElement>) => {
    setPreedit(ev.data);
    changed();
  };
  // The keys' field keeps the focus while drawing: a click on the toolbar's
  // background or the banner would leave letters going nowhere.
  const onBlur = () => {
    if (drawing) window.requestAnimationFrame(() => input.current?.focus());
  };

  const at = (ev: React.MouseEvent): BoardPoint => {
    const r = (ev.currentTarget as HTMLElement).getBoundingClientRect();
    const fit = (v: number, size: number) => Math.round(Math.min(1, Math.max(0, v / size)) * 65535);
    return { x: fit(ev.clientX - r.left, r.width), y: fit(ev.clientY - r.top, r.height) };
  };
  const onDown = (ev: React.PointerEvent) => {
    const e = editor.current;
    if (!drawing || !e) return;
    ev.preventDefault();
    input.current?.focus();
    if (ev.button === 1) {
      e.ping(at(ev), board.current, now());
    } else if (ev.button === 0) {
      ev.currentTarget.setPointerCapture(ev.pointerId);
      held.current = true;
      const wasTyping = e.typing();
      e.press(at(ev), ev.shiftKey, board.current, now());
      // A click that finished the note mid-composition: the input method's
      // composition is cancelled (blurring the field ends it), never
      // committed into the next note (the review's finding).
      if (wasTyping && !e.typing() && preeditRef.current) {
        input.current?.blur();
        input.current?.focus();
        setPreedit("");
      }
    }
    changed();
  };
  const onMove = (ev: React.PointerEvent) => {
    const e = editor.current;
    if (drawing) pointer.current = at(ev);
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

  const onDouble = (ev: React.MouseEvent) => {
    const e = editor.current;
    if (!drawing || !e || ev.button !== 0) return;
    e.doubleClick(at(ev), board.current, now());
    changed();
  };

  const e = editor.current;
  const pick = (f: (e: Editor) => void) => {
    if (!e) return;
    f(e);
    changed();
    // The keys' field keeps the focus: a toolbar button took it.
    input.current?.focus();
  };
  const textTool = e?.tool === "text";
  // The keys' field sits at the caret, so an input method's candidates show
  // beside the text.
  const cursor = drawing ? e?.textCursor() : null;
  const field = cursor
    ? { left: `${(cursor.caret[0].x / 65535) * 100}%`, top: `${(cursor.caret[0].y / 65535) * 100}%` }
    : { left: "0", top: "0" };

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
        onDoubleClick={onDouble}
      />
      {drawing && (
        <>
          <textarea
            ref={input}
            className="board-keys"
            style={field}
            aria-label="Text"
            autoComplete="off"
            autoCorrect="off"
            spellCheck={false}
            onInput={onInput}
            onCompositionUpdate={onComposing}
            onBlur={onBlur}
            onCompositionEnd={onComposed}
          />
          <div className="board-banner" role="status">
            <strong>Drawing over the game</strong>
            <span>{note ?? "Esc or Done to go back to the game"}</span>
          </div>
          {e && toolsHidden && (
            <button
              type="button"
              className="board-pill"
              title={`${TOOLS.find((t) => t.tool === e.tool)?.name ?? ""} · show the drawing tools · Tab`}
              aria-label="Show the drawing tools"
              onClick={() => {
                setToolsHidden(false);
                input.current?.focus();
              }}
            >
              <ChevronIcon up />
              <ToolIcon tool={e.tool} laser={rgb(e.colour)} />
              <span className="board-pill-colour" style={{ background: rgb(e.colour) }} />
            </button>
          )}
          {e && !toolsHidden && (
            <div className="board-toolbar" role="toolbar" aria-label="Drawing">
              {/* The handle: a tab on the toolbar's top edge that hides it
                  (M27c, the author's request). */}
              <button
                type="button"
                className="board-handle"
                title="Hide the drawing tools · Tab"
                aria-label="Hide the drawing tools"
                onClick={() => {
                  setToolsHidden(true);
                  input.current?.focus();
                }}
              >
                <ChevronIcon up={false} />
              </button>
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
                {/* Widths, or with the Text tool the text's sizes. */}
                {(textTool ? TEXT_SIZE_NAMES : WIDTH_NAMES).map((name, i) => (
                  <button
                    key={name}
                    type="button"
                    className={e.width === i ? "board-button on" : "board-button"}
                    title={name}
                    aria-label={name}
                    aria-pressed={e.width === i}
                    onClick={() => pick((x) => x.setStyle({ width: i }, board.current, now()))}
                  >
                    {textTool ? (
                      <span className="board-size" style={{ fontSize: 10 + 4 * i }}>
                        A
                      </span>
                    ) : (
                      <span className="board-dot" style={{ width: 4 + 4 * i, height: 4 + 4 * i }} />
                    )}
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
                {textTool
                  ? "Click to write, or on your text to change it · double click: a word · Esc: done"
                  : "Shift: straight lines, squares and circles · middle click: ping · Tab: hide the tools · Esc: back to the game"}
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
