// A member or room as a round (or rounded-square) badge with initials, its
// colour picked from its id so it is the same on every PC, and an optional
// presence dot.
import type { MemberLink } from "../bindings/MemberLink";
import "./Avatar.css";

/** Hues that read well behind white initials in both themes. */
const HUES = [18, 36, 152, 172, 200, 222, 262, 292, 330, 350];

export function hueOf(id: string): number {
  let h = 2166136261;
  for (let i = 0; i < id.length; i++) {
    h ^= id.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return HUES[Math.abs(h) % HUES.length];
}

export function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  const letters = words.length > 1 ? [words[0], words[1]] : [words[0] ?? "?"];
  return letters
    .map((w) => Array.from(w)[0] ?? "")
    .join("")
    .toUpperCase();
}

export function Avatar(props: {
  id: string;
  name: string;
  size?: 20 | 24 | 32 | 36 | 40 | 48;
  square?: boolean;
  presence?: MemberLink | null;
  dimmed?: boolean;
}) {
  const size = props.size ?? 32;
  return (
    <span
      className={["avatar", props.square ? "square" : "", props.dimmed ? "dimmed" : ""].join(" ")}
      style={{ "--hue": hueOf(props.id), "--size": `${size}px` } as React.CSSProperties}
      aria-hidden="true"
    >
      <span className="avatar-letters">{initials(props.name)}</span>
      {props.presence && <span className={`presence presence-${props.presence}`} />}
    </span>
  );
}
