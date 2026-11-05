// Naming the game's window on a Linux host (M16c-2): the screen-sharing
// portal never says which window it shares, so guests' keyboard and mouse
// wait until the host names the game here, and then reach it only while it
// is in front. One banner while the stream runs, a warning until the game is
// named and a note after, so naming it moves nothing below.
import { ChevronDown, Keyboard } from "lucide-react";
import { useEffect, useState } from "react";
import type { GameWindowView } from "../bindings/GameWindowView";
import type { SessionView } from "../bindings/SessionView";
import { errorText, request } from "../engine";
import { asksForGame, gameWindowName, isLive, likelyGame } from "../model/sessions";
import { Button } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { DropdownMenu, type MenuEntry } from "../ui/Menu";
import type { Run } from "./useRun";

/** How often the list is read again while the banner shows: windows come and
 * go, and a game may resize to the shared size after it starts. */
const REFRESH_MS = 3000;

const WHY =
  "Your desktop doesn't tell programs which window it shares, so you name the game. Guests' keys and clicks reach it only while it's in front, never another window; if it closes, they wait until you name another.";

const CORNER = "First turn off KDE's top-left screen corner (System Settings → Screen Edges).";
const CORNER_WHY =
  "KDE's Overview, which that corner opens, takes the keys while the game still counts as in front, and a guest's mouse can reach the corner: its search would get their typing.";

export function GameWindowBanner(props: { session: SessionView; busy: string | null; run: Run }) {
  const s = props.session;
  const shown = asksForGame(s) && isLive(s);
  const [windows, setWindows] = useState<GameWindowView[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!shown) {
      setWindows(null);
      return;
    }
    let live = true;
    const load = () =>
      request({ type: "gameWindows" }, "gameWindows")
        .then((r) => {
          if (!live) return;
          setWindows(r.windows);
          setError(null);
        })
        .catch((e) => live && setError(errorText(e)));
    void load();
    const timer = setInterval(() => void load(), REFRESH_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [shown]);

  if (!shown) return null;
  const named = s.gameWindow;
  const likely = named ? null : likelyGame(windows ?? []);
  const name = (id: number | null) => void props.run("game-window", { type: "setGameWindow", id });
  const items: MenuEntry[] =
    windows == null || windows.length === 0
      ? [{ label: windows == null ? "Reading your windows…" : "No windows listed yet", disabled: true }]
      : windows.map((w) => ({
          label: gameWindowName(w),
          hint: w.matches ? "the shared size" : undefined,
          disabled: named?.id === w.id ? "named now" : false,
          onSelect: () => name(w.id),
        }));
  if (named) items.push("separator", { label: "None: keys reach nothing", onSelect: () => name(null) });
  const menu = (label: string) => (
    <DropdownMenu items={items} align="end">
      <Button size="sm" icon={<ChevronDown />} disabled={props.busy != null}>
        {label}
      </Button>
    </DropdownMenu>
  );

  // KDE's top-left corner opens its Overview, which takes the keys while
  // KWin still calls the game active: the host turns the corner off (the
  // author's choice, M16c-2). Said in both states, so they stay one height.
  const why = s.warnHotCorner ? `${WHY} ${CORNER_WHY}` : WHY;
  const corner = s.warnHotCorner && <span className="game-banner-corner"> {CORNER}</span>;
  const namedBanner = (game: GameWindowView) => (
    <Callout tone="info" icon={<Keyboard />} why={why} actions={menu("Change…")}>
      Guests' keyboard and mouse reach <strong>{gameWindowName(game)}</strong>, while it's in front.
      {corner}
    </Callout>
  );
  const asking = (likely: GameWindowView | null) => (
    <Callout
      tone="warn"
      icon={<Keyboard />}
      why={why}
      actions={
        <>
          {likely && (
            <Button size="sm" variant="primary" disabled={props.busy != null} onClick={() => name(likely.id)}>
              That's the game
            </Button>
          )}
          {menu(likely ? "Another…" : "Choose…")}
        </>
      }
    >
      Guests' keyboard and mouse wait until you name the game's window
      {likely ? (
        <>
          : is it <strong>{gameWindowName(likely)}</strong>?
        </>
      ) : windows != null && windows.length === 0 ? (
        ". Your desktop hasn't listed its windows yet."
      ) : (
        "."
      )}
      {corner}
      {error && <span className="field-error"> {error}</span>}
    </Callout>
  );
  // The other state's banner, unseen, in the same cell: the banner is as
  // tall as the taller of the two, so naming the game moves nothing below.
  const placeholder = { id: 0, title: "", app: "", matches: false };
  return (
    <div className="game-banner">
      {named ? namedBanner(named) : asking(likely)}
      <div className="game-banner-reserve" aria-hidden="true" inert>
        {named ? asking(named) : namedBanner(likely ?? windows?.[0] ?? placeholder)}
      </div>
    </div>
  );
}
