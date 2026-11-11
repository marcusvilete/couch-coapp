// The board toolbar's icons, drawn as the stream window paints its own
// (`couch-viewer`'s `draw_ui.rs`: `tool_icon`, `pen_icon`, `fade_icon`,
// `turn_icon`), point for point, so both toolbars look the same on every
// system (the author's one-PC run of M27b: Lucide's laser and fade differed).
// egui paints them in an 18-point square inside a 30-point button; here the
// same square, in `currentColor`.

import type { DrawTool } from "../bindings/boardRules";

const S = 18;
/** A point of the icon's square, as egui's `at(r, x, y)`. */
const at = (x: number, y: number) => `${(x * S).toFixed(2)},${(y * S).toFixed(2)}`;
const line = (a: [number, number], b: [number, number]) => `M${at(...a)}L${at(...b)}`;
const closed = (pts: [number, number][]) => `M${pts.map((p) => at(...p)).join("L")}Z`;

function Svg({ children }: { children: React.ReactNode }) {
  return (
    <svg
      width={S}
      height={S}
      viewBox={`0 0 ${S} ${S}`}
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

export function PenIcon() {
  return (
    <Svg>
      <path
        d={
          closed([
            [0.68, 0.12],
            [0.88, 0.32],
            [0.32, 0.88],
            [0.1, 0.9],
            [0.12, 0.68],
          ]) + line([0.58, 0.22], [0.78, 0.42])
        }
      />
    </Svg>
  );
}

/** A tool's icon; the laser's in `laser`, the colour picked (it draws in it). */
export function ToolIcon({ tool, laser }: { tool: DrawTool; laser: string }) {
  switch (tool) {
    case "select":
      return (
        <Svg>
          <path
            d={closed([
              [0.25, 0.1],
              [0.25, 0.85],
              [0.45, 0.66],
              [0.6, 0.95],
              [0.72, 0.89],
              [0.58, 0.6],
              [0.85, 0.6],
            ])}
          />
        </Svg>
      );
    case "pen":
      return <PenIcon />;
    case "highlighter":
      return (
        <Svg>
          <path d={line([0.2, 0.8], [0.8, 0.2])} strokeWidth={6} strokeOpacity={0.5} strokeLinecap="butt" />
          <path d={line([0.2, 0.8], [0.8, 0.2])} strokeWidth={1.2} />
        </Svg>
      );
    case "laser":
      return (
        <Svg>
          <path d={line([0.15, 0.85], [0.65, 0.35])} stroke={laser} strokeWidth={2} strokeOpacity={0.5} />
          <circle cx={0.7 * S} cy={0.3 * S} r={3.5} fill={laser} stroke="none" />
        </Svg>
      );
    case "arrow":
      return (
        <Svg>
          <path d={line([0.15, 0.85], [0.85, 0.15]) + line([0.85, 0.15], [0.45, 0.15]) + line([0.85, 0.15], [0.85, 0.55])} />
        </Svg>
      );
    case "line":
      return (
        <Svg>
          <path d={line([0.15, 0.85], [0.85, 0.15])} />
        </Svg>
      );
    case "rectangle":
      return (
        <Svg>
          <rect x={0.1 * S} y={0.22 * S} width={0.8 * S} height={0.56 * S} rx={2} />
        </Svg>
      );
    case "ellipse":
      return (
        <Svg>
          <ellipse cx={S / 2} cy={S / 2} rx={0.42 * S} ry={0.3 * S} />
        </Svg>
      );
    // A T, as a letter of lines (the stream window's `tool_icon`).
    case "text":
      return (
        <Svg>
          <path
            d={`M${0.18 * S} ${0.18 * S}H${0.82 * S}M${0.5 * S} ${0.18 * S}V${0.85 * S}M${0.38 * S} ${0.85 * S}H${0.62 * S}`}
          />
        </Svg>
      );
    case "eraser":
      return (
        <Svg>
          <path
            d={
              closed([
                [0.15, 0.6],
                [0.5, 0.2],
                [0.85, 0.5],
                [0.55, 0.85],
                [0.35, 0.85],
              ]) + line([0.32, 0.4], [0.68, 0.72])
            }
          />
        </Svg>
      );
  }
}

/** Stays or fades: a line fading out along its length. */
export function FadeIcon() {
  const steps = 5;
  return (
    <Svg>
      {Array.from({ length: steps }, (_, i) => {
        const t0 = i / steps;
        const t1 = (i + 1) / steps;
        return (
          <path
            key={i}
            d={line([0.1 + 0.8 * t0, 0.5], [0.1 + 0.8 * t1, 0.5])}
            strokeWidth={3}
            strokeOpacity={1 - t0 * 0.85}
            strokeLinecap="butt"
          />
        );
      })}
    </Svg>
  );
}

/** Undo (an arc turning back) or, `forward`, redo. */
export function TurnIcon({ forward }: { forward: boolean }) {
  const flip = (x: number) => (forward ? 1 - x : x);
  const arc = Array.from({ length: 9 }, (_, i): [number, number] => {
    const t = Math.PI * (i / 8);
    return [flip(0.5 - 0.32 * Math.cos(t)), 0.62 - 0.32 * Math.sin(t)];
  });
  const start = arc[0];
  return (
    <Svg>
      <path
        d={
          `M${arc.map((p) => at(...p)).join("L")}` +
          line(start, [flip(0.05), 0.48]) +
          line(start, [flip(0.32), 0.48])
        }
      />
    </Svg>
  );
}

/** A chevron, up or down: the toolbar's handle and pill (`draw_ui.rs`'s
 * `chevron`, 5 points either side and 2.5 up or down from the middle). */
export function ChevronIcon({ up }: { up: boolean }) {
  const dy = up ? 2.5 : -2.5;
  const m = S / 2;
  return (
    <Svg>
      <path d={`M${m - 5},${m + dy}L${m},${m - dy}L${m + 5},${m + dy}`} strokeWidth={1.8} />
    </Svg>
  );
}
