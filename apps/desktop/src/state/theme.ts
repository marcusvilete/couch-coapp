// Settings → Appearance (M21): the theme follows Windows, or is chosen. The
// choice is this profile's (web storage), applied to the page (`data-theme`)
// and to the window's title bar.
import { useCallback, useEffect, useState } from "react";
import { platform } from "../platform";
import { KEYS, readStored, writeStored } from "./storage";

export type ThemeChoice = "system" | "dark" | "light";

function stored(): ThemeChoice {
  const value = readStored(KEYS.theme);
  return value === "dark" || value === "light" ? value : "system";
}

function apply(choice: ThemeChoice) {
  const root = document.documentElement;
  if (choice === "system") delete root.dataset.theme;
  else root.dataset.theme = choice;
  void platform()
    .setTheme(choice === "system" ? null : choice)
    .catch(() => {});
}

/** Applies the stored choice at once, before the first paint of the app. */
export function applyStoredTheme() {
  apply(stored());
}

export function useTheme(): [ThemeChoice, (choice: ThemeChoice) => void] {
  const [choice, setChoice] = useState<ThemeChoice>(stored);
  useEffect(() => apply(choice), [choice]);
  const choose = useCallback((next: ThemeChoice) => {
    writeStored(KEYS.theme, next);
    setChoice(next);
  }, []);
  return [choice, choose];
}
