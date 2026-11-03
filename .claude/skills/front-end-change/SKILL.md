---
name: front-end-change
description: Make a change to Couch Co-App's front end (apps/desktop/src: the shell, rooms, sessions and settings screens, the ui design system, model and state logic, the preview and its fake engine, the host overlay's page) the way the project keeps it — where code goes, the rules every screen keeps (nothing jumps, every action on its button and its right-click, no key in the webview answers a request, platform differences from EngineInfo), a preview scenario for each new state, checking it by eye with `npm run shots`, and the pitfalls already met (Radix Select's empty value, ES2020, effects before the engine's info, CRLF edits). Use when adding or changing a screen, dialog, menu, setting or front-end logic, when a new engine command or view reaches the UI, and before reporting such a change done.
---

# A front-end change

The front end is M21's ([results](../../../docs/m21-results.md)): React and TypeScript on Radix primitives, in the Tauri webview (WebView2 on Windows, WebKitGTK on Linux). CLAUDE.md's "The front end" holds the rules; this is the order to work in, and what has bitten before. Commands run from `apps/desktop`.

## Before writing

- **Where it goes:**
  - logic without React in `src/model` (pure) or `src/state` (the lobby's store, web storage), each with vitest tests beside it (`*.test.ts`);
  - screens in `src/shell`, `src/rooms`, `src/sessions`, `src/settings`; shared pieces in `src/ui` (styles from `src/styles/tokens.css`, never raw colours);
  - **Tauri only through `src/platform.ts`** (`platform/tauri.ts` implements it); the preview's fake (`src/preview`) is installed by its own entry, never as a fallback.
  - **The overlay's page stays apart** (`src/overlay/main.tsx`, `overlay.css` scoped to `.overlay`): no theme tokens, no global styles, no Radix.
- **A new engine command or view:** the TypeScript types come from the engine (`cargo test -p couch-engine` writes `src/bindings`; line-ending-only changes in other bindings drop out on staging). Give the preview's fake engine an answer for it (`preview/fakeEngine.ts`), as the real one would refuse or answer. A room view is kept by its `revision` (`state/lobby.ts`'s `putRoom`): anything new that stores room views does the same.
- **Platform differences come from `EngineInfo`:** `capabilities` (sessions, keyboard seat, desktop control), `homePc`, `platform`; never guessed from the user agent. The engine's refusal is the guard; the UI only hides what's missing.

## The rules every screen keeps

- **Nothing jumps:** the shell's regions and dialogs keep their sizes as you move about; new state appears in reserved space (a note line kept even when empty, a skeleton with as many rows as will come) or below what the user is using. A new view or dialog gets its moves in `scripts/shots.config.mjs`'s `STABLE`.
- **Every action on an object is on its button and its right-click,** from one `MenuEntry[]` for both (`ui/Menu.tsx`); nothing only in a context menu. The webview's own context menu and browser keys stay off (`shell/guards.ts`), but in text fields and `.selectable` text.
- **No key in the webview answers a request or passes a seat** (only the host-keys hook or portal does); a request's Accept waits 1 s and is disabled while control is paused. The patch bay's drops send the same commands as the menus, and `dropOn` refuses what the menus refuse.
- **Words:** the app's own, plain and short, as in the screens around it; errors say what to do. Text the user may copy (an error, a path) is `.selectable`.

## Seeing it

- **A new screen or state gets a preview scenario** (`preview/scenarios.ts`, a `base([...], { ... })` world): then `npx vite` and `http://localhost:1420/preview.html?scenario=<name>` (`?sheet` for the components).
- **`npm run shots`** shoots every scenario in dark, light and High Contrast at three sizes into `target\ui-shots\latest`, runs the no-jump check, and fails on text spilling out of its box or a menu that doesn't open. `-- --only <part of a name>` (one word: arguments split at spaces), `--looks dark`, `--sizes 1200x760`. A shot of a state needing clicks goes in the config's shot list (`[name, scenario, async (page) => …]`), found by role and name, not CSS.
- **Look at the images yourself** (the Read tool shows them) before calling it done: the checks catch spills and jumps, not a wrong look.
- **The real app,** when the preview can't show it: the `app-manual-run` skill; a script can drive a development build over CDP (CLAUDE.md's table), never a release build.

## Pitfalls already met

- **Radix Select treats an empty-string value as no value** (the trigger shows blank; an item can't have it): give "none" a sentinel (`model/sessions.ts`'s `EVERYTHING`).
- **The target is ES2020** (`tsconfig.json`): no `Array.prototype.at` or `Object.hasOwn`. tsc says so; vitest runs them anyway.
- **The engine's info arrives after the first render:** a hook choosing a mode from it (`useOverlay`'s overlay or notices) must follow a change of mode, not read it once; clean up with the mode current at unmount (a ref).
- **Prettier isn't the project's formatter** (it would rewrite 155 files): match the code around.
- **Editing from the shell:** files are CRLF in the working copy, so `perl -pi` patterns with `\n` silently miss; prefer the Edit tool, or normalise in the script. Perl with `{}` delimiters breaks on code with braces.
- **`npm run build` fails if preview code reaches `dist`** (`scripts/check-dist.mjs`): import the preview only from its entry.

## After

- `npx tsc --noEmit`, `npx vitest run`, `npm run shots`: no problems, nothing jumps.
- Each new state has a scenario; the results page names what was looked at.
- CLAUDE.md's front-end rules and this skill stay true; a new pitfall goes in the list above.
