import { useEffect, useState } from "react";

/** Whether the window is narrower than `px` (the shell's breakpoints: 1180
 * for the members drawer, 900 for the room rail). */
export function useNarrow(px: number): boolean {
  const query = `(max-width: ${px - 1}px)`;
  const [narrow, setNarrow] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const list = window.matchMedia(query);
    const change = () => setNarrow(list.matches);
    change();
    list.addEventListener("change", change);
    return () => list.removeEventListener("change", change);
  }, [query]);
  return narrow;
}
