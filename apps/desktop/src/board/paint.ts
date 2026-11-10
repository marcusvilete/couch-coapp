// Paints the board on the host's overlay (M27b), as the stream window's
// `draw_ui.rs` paints it over the picture: the same shapes, widths, fading,
// laser trails and pings, so what the host sees over the game is what the
// others see in their windows. The canvas is the picture: the window lies
// exactly over what is captured.

import type { BoardElement } from "../bindings/BoardElement";
import type { BoardPoint } from "../bindings/BoardPoint";
import { LASER_MS, PALETTE, PING_MS } from "../bindings/boardRules";
import { alpha, type BoardView } from "../model/board";
import { bounds, type Editor, lineWidth } from "../model/editor";

const FULL = 65535;
const BLUE = "#4a9ff0";

function rgba(index: number, a: number): string {
  const [r, g, b] = PALETTE[Math.min(index, PALETTE.length - 1)];
  return `rgba(${r}, ${g}, ${b}, ${Math.max(0, Math.min(1, a))})`;
}

export interface Painting {
  board: BoardView;
  /** Ours, while drawing: the selection and the marquee show. */
  editor: Editor | null;
  names: Map<string, string>;
  now: number;
}

/** Paints everything on `ctx`, `width` by `height` CSS pixels. */
export function paintBoard(ctx: CanvasRenderingContext2D, width: number, height: number, p: Painting) {
  ctx.clearRect(0, 0, width, height);
  const place = (q: BoardPoint): [number, number] => [(q.x / FULL) * width, (q.y / FULL) * height];
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  for (const shown of p.board.elements) {
    element(ctx, shown.element, alpha(shown, p.now), height, place);
  }
  if (p.editor) {
    ctx.strokeStyle = BLUE;
    ctx.lineWidth = 1.5;
    for (const id of p.editor.selection()) {
      const s = p.board.get(p.editor.me, id);
      if (!s) continue;
      const [a, b] = bounds(s.element).map(place);
      roundRect(ctx, Math.min(a[0], b[0]) - 6, Math.min(a[1], b[1]) - 6, Math.abs(b[0] - a[0]) + 12, Math.abs(b[1] - a[1]) + 12, 3);
      ctx.stroke();
    }
    const marquee = p.editor.marquee();
    if (marquee) {
      const [a, b] = marquee.map(place);
      ctx.fillStyle = "rgba(74, 159, 240, 0.12)";
      ctx.fillRect(Math.min(a[0], b[0]), Math.min(a[1], b[1]), Math.abs(b[0] - a[0]), Math.abs(b[1] - a[1]));
      ctx.lineWidth = 1;
      ctx.strokeRect(Math.min(a[0], b[0]), Math.min(a[1], b[1]), Math.abs(b[0] - a[0]), Math.abs(b[1] - a[1]));
    }
  }
  const laserWidth = 0.006 * height;
  for (const trail of p.board.lasers.values()) {
    for (let i = 1; i < trail.length; i++) {
      const point = trail[i];
      if (point.start) continue;
      const a = Math.max(0, Math.min(1, 1 - (p.now - point.since) / LASER_MS));
      const [x0, y0] = place(trail[i - 1].at);
      const [x1, y1] = place(point.at);
      for (const [w, k] of [
        [laserWidth * 1.8, 0.4],
        [laserWidth, 1],
      ]) {
        ctx.strokeStyle = rgba(point.colour, a * k);
        ctx.lineWidth = w;
        ctx.beginPath();
        ctx.moveTo(x0, y0);
        ctx.lineTo(x1, y1);
        ctx.stroke();
      }
    }
    const head = trail[trail.length - 1];
    if (head) {
      const a = Math.max(0, Math.min(1, 1 - (p.now - head.since) / LASER_MS));
      const [x, y] = place(head.at);
      ctx.fillStyle = `rgba(255, 255, 255, ${a})`;
      ctx.beginPath();
      ctx.arc(x, y, laserWidth, 0, Math.PI * 2);
      ctx.fill();
    }
  }
  for (const ping of p.board.pings) {
    const age = (p.now - ping.since) / 1000;
    const phase = ((age * 2) / (PING_MS / 1000)) % 1;
    const [x, y] = place(ping.at);
    ctx.strokeStyle = rgba(2, 1 - phase);
    ctx.lineWidth = 3;
    ctx.beginPath();
    ctx.arc(x, y, height * (0.015 + 0.04 * phase), 0, Math.PI * 2);
    ctx.stroke();
    ctx.fillStyle = rgba(2, 1);
    ctx.beginPath();
    ctx.arc(x, y, 4, 0, Math.PI * 2);
    ctx.fill();
    ctx.fillStyle = "#ffffff";
    ctx.font = '14px "Segoe UI", system-ui, sans-serif';
    ctx.textAlign = "center";
    ctx.textBaseline = "top";
    ctx.shadowColor = "rgba(0, 0, 0, 0.8)";
    ctx.shadowBlur = 3;
    ctx.fillText(p.names.get(ping.author) ?? "Someone", x, y + height * 0.06);
    ctx.shadowBlur = 0;
  }
}

function element(
  ctx: CanvasRenderingContext2D,
  e: BoardElement,
  a: number,
  height: number,
  place: (q: BoardPoint) => [number, number],
) {
  if (a <= 0 || e.points.length === 0) return;
  const seeThrough = e.kind === "highlighter" ? 0.38 : 1;
  const colour = rgba(e.colour, a * seeThrough);
  const width = Math.max(1, lineWidth(e.kind, e.width) * height);
  const pts = e.points.map(place);
  ctx.strokeStyle = colour;
  ctx.fillStyle = colour;
  ctx.lineWidth = width;
  // The highlighter's flat ends, as egui draws them.
  ctx.lineCap = e.kind === "highlighter" ? "butt" : "round";
  ctx.beginPath();
  switch (e.kind) {
    case "pen":
    case "highlighter":
      if (pts.length === 1) {
        ctx.arc(pts[0][0], pts[0][1], width / 2, 0, Math.PI * 2);
        ctx.fill();
        return;
      }
      ctx.moveTo(pts[0][0], pts[0][1]);
      for (const [x, y] of pts.slice(1)) ctx.lineTo(x, y);
      ctx.stroke();
      return;
    case "line":
      ctx.moveTo(pts[0][0], pts[0][1]);
      ctx.lineTo(pts[1][0], pts[1][1]);
      ctx.stroke();
      return;
    case "arrow": {
      const [[ax, ay], [bx, by]] = pts;
      ctx.moveTo(ax, ay);
      ctx.lineTo(bx, by);
      const len = Math.hypot(bx - ax, by - ay);
      if (len > 1) {
        const [dx, dy] = [(bx - ax) / len, (by - ay) / len];
        const head = Math.min(Math.max(width * 4, 10), len * 0.5);
        for (const s of [1, -1]) {
          ctx.moveTo(bx, by);
          ctx.lineTo(bx - dx * head - dy * head * 0.55 * s, by - dy * head + dx * head * 0.55 * s);
        }
      }
      ctx.stroke();
      return;
    }
    case "rectangle": {
      const [[ax, ay], [bx, by]] = pts;
      roundRect(ctx, Math.min(ax, bx), Math.min(ay, by), Math.abs(bx - ax), Math.abs(by - ay), 2);
      ctx.stroke();
      return;
    }
    case "ellipse": {
      const [[ax, ay], [bx, by]] = pts;
      ctx.ellipse((ax + bx) / 2, (ay + by) / 2, Math.abs(bx - ax) / 2, Math.abs(by - ay) / 2, 0, 0, Math.PI * 2);
      ctx.stroke();
      return;
    }
  }
}

function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  ctx.beginPath();
  const radius = Math.min(r, w / 2, h / 2);
  ctx.moveTo(x + radius, y);
  ctx.arcTo(x + w, y, x + w, y + h, radius);
  ctx.arcTo(x + w, y + h, x, y + h, radius);
  ctx.arcTo(x, y + h, x, y, radius);
  ctx.arcTo(x, y, x + w, y, radius);
  ctx.closePath();
}
