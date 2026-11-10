// The board as the host's board overlay shows it (M27b, ADR 0013): every
// element in drawing order, when each finished arriving here (fading is
// timed from that), and the passing marks, laser trails and pings. A port of
// the stream window's `couch-viewer/src/board.rs`; keep the two alike. Pure:
// times are milliseconds on any clock that only goes forward.

import type { BoardElement } from "../bindings/BoardElement";
import type { BoardNewsView } from "../bindings/BoardNewsView";
import type { BoardOpView } from "../bindings/BoardOpView";
import type { BoardPoint } from "../bindings/BoardPoint";
import { FADE_OUT_MS, LASER_MS, PING_MS } from "../bindings/boardRules";

/** One element as shown. */
export interface Shown {
  author: string;
  element: BoardElement;
  /** When it finished arriving here, for fading. */
  doneAt: number | null;
}

/** How long `e` stays once finished, before fading: its author's pick (wire
 * revision 16). */
export function stayMs(e: BoardElement): number {
  return e.stay * 1000;
}

/** How opaque `s` is at `now`: 1 until a fading one has stayed its while,
 * then down to 0 over the fade. */
export function alpha(s: Shown, now: number): number {
  if (s.doneAt === null || !s.element.fades) return 1;
  const age = Math.max(0, now - s.doneAt);
  const stay = stayMs(s.element);
  if (age <= stay) return 1;
  return Math.max(0, 1 - (age - stay) / FADE_OUT_MS);
}

/** A laser trail's point, and when it arrived. */
export interface LaserPoint {
  at: BoardPoint;
  since: number;
  /** The first of a new trail: no line to it from the one before. */
  start: boolean;
  colour: number;
}

export interface Ping {
  author: string;
  at: BoardPoint;
  since: number;
}

export class BoardView {
  generation = 0;
  /** In drawing order. */
  elements: Shown[] = [];
  lasers = new Map<string, LaserPoint[]>();
  pings: Ping[] = [];

  get(author: string, id: number): Shown | undefined {
    return this.elements.find((s) => s.author === author && s.element.id === id);
  }

  /** `author`'s elements, in drawing order. */
  of(author: string): Shown[] {
    return this.elements.filter((s) => s.author === author);
  }

  /** What the host said, to `me`; its refusal's words, if that is what it
   * was. Only a clear of our own elements or everyone's starts a new
   * generation for us. */
  news(news: BoardNewsView, me: string | null, now: number): string | null {
    switch (news.type) {
      case "snapshot":
        if (news.first) this.elements = [];
        this.generation = news.generation;
        for (const a of news.elements) this.apply(a.author, [{ type: "put", element: a.element }], now);
        return null;
      case "update":
        this.apply(news.author, news.ops, now);
        return null;
      case "cleared": {
        if (news.author === null || news.author === me) this.generation = news.generation;
        const author = news.author;
        if (author !== null) {
          this.elements = this.elements.filter((s) => s.author !== author);
          this.lasers.delete(author);
          this.pings = this.pings.filter((p) => p.author !== author);
        } else {
          this.elements = [];
          this.lasers.clear();
          this.pings = [];
        }
        return null;
      }
      case "refused":
        return news.message;
    }
  }

  /** `author`'s operations: the host's update, or our own as we send them. */
  apply(author: string, ops: BoardOpView[], now: number) {
    for (const op of ops) {
      switch (op.type) {
        case "put": {
          const e = op.element;
          const doneAt = e.done ? now : null;
          const s = this.get(author, e.id);
          if (s) {
            // Replaced, it keeps its place; finished before, it keeps its time.
            s.doneAt = e.done ? (s.doneAt ?? doneAt) : null;
            s.element = cloneElement(e);
          } else {
            this.elements.push({ author, element: cloneElement(e), doneAt });
          }
          break;
        }
        case "append": {
          const s = this.get(author, op.id);
          if (s && !s.element.done) {
            s.element.points.push(...op.points);
            s.element.done = op.done;
            if (op.done) s.doneAt = now;
          }
          break;
        }
        case "remove":
          this.elements = this.elements.filter((s) => s.author !== author || !op.ids.includes(s.element.id));
          break;
        case "laser": {
          const trail = this.lasers.get(author) ?? [];
          op.points.forEach((at, i) => trail.push({ at, since: now, start: op.start && i === 0, colour: op.colour }));
          this.lasers.set(author, trail);
          break;
        }
        case "ping":
          // One ring per member at a time.
          this.pings = this.pings.filter((p) => p.author !== author);
          this.pings.push({ author, at: op.at, since: now });
          break;
      }
    }
  }

  /** Lets go of what has faded, the laser points past their time, and the
   * pings that are over. */
  prune(now: number) {
    this.elements = this.elements.filter((s) => alpha(s, now) > 0);
    for (const [author, trail] of this.lasers) {
      const kept = trail.filter((p) => now - p.since < LASER_MS);
      if (kept.length > 0) this.lasers.set(author, kept);
      else this.lasers.delete(author);
    }
    this.pings = this.pings.filter((p) => now - p.since < PING_MS);
  }

  /** When the next fading element starts to fade, if one is still staying:
   * nothing moves until then, so the page must wake itself (the stream
   * window repaints with every video frame anyway; the board overlay has
   * none). `null` if none waits. */
  nextFade(now: number): number | null {
    let next: number | null = null;
    for (const s of this.elements) {
      if (!s.element.fades || s.doneAt === null) continue;
      const at = s.doneAt + stayMs(s.element);
      if (at > now && (next === null || at < next)) next = at;
    }
    return next;
  }

  /** Whether something changes by itself (fading, a laser, a ping): the page
   * then paints again soon. */
  moving(now: number): boolean {
    return (
      this.lasers.size > 0 ||
      this.pings.length > 0 ||
      this.elements.some((s) => s.element.fades && s.doneAt !== null && now - s.doneAt > stayMs(s.element))
    );
  }
}

export function cloneElement(e: BoardElement): BoardElement {
  return { ...e, points: e.points.map((p) => ({ x: p.x, y: p.y })) };
}
