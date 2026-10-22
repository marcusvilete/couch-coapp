// Screenshots of the preview (development only): every scenario, in each
// look and size, through the Microsoft Edge already on Windows (headless;
// Playwright downloads nothing). Then the no-jump check: the main regions'
// boxes must not change as the user moves between views.
//
//   npm run shots                        everything, into target/ui-shots/latest
//   npm run shots -- --only hosting      scenarios whose name contains "hosting"
//   npm run shots -- --set before        into target/ui-shots/before
//   npm run shots -- --looks dark        only the dark look (dark, light, contrast)
//   npm run shots -- --sizes 1200x760    only that size
import { mkdirSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { createServer } from "vite";

const app = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repo = resolve(app, "../..");

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : fallback;
}

const only = arg("only", "");
const set = arg("set", "latest");
const looks = arg("looks", "dark,light,contrast").split(",");
const sizes = arg("sizes", "1200x760,960x640,760x520")
  .split(",")
  .map((s) => s.split("x").map(Number));
const out = join(repo, "target", "ui-shots", set);

/** Runs in the page: elements holding text that is wider than they are,
 * other than those meant to cut it with an ellipsis or to scroll. */
function overflowing() {
  const found = [];
  for (const el of document.querySelectorAll("body *")) {
    if (el.closest(".preview-panel, .preview-overlay, .splash, .btn-reserve, .sr-only, svg, input, textarea, select")) continue;
    const text = [...el.childNodes].some((n) => n.nodeType === Node.TEXT_NODE && n.textContent.trim() !== "");
    if (!text || el.clientWidth === 0) continue;
    const style = getComputedStyle(el);
    if (style.textOverflow === "ellipsis" || ["auto", "scroll"].includes(style.overflowX)) continue;
    if (el.scrollWidth > el.clientWidth + 1) {
      found.push(`"${el.textContent.trim().slice(0, 40)}" (${el.className || el.tagName.toLowerCase()})`);
    }
  }
  return [...new Set(found)].slice(0, 6);
}

const LOOKS = {
  dark: { colorScheme: "dark", forcedColors: "none" },
  light: { colorScheme: "light", forcedColors: "none" },
  contrast: { colorScheme: "dark", forcedColors: "active" },
};

/** Shots that need steps after loading: [name, scenario, steps]. The
 * scenarios alone are shot as they open. */
const { SHOTS, STABLE } = await import("./shots.config.mjs");

const server = await createServer({
  root: app,
  configFile: join(app, "vite.config.ts"),
  server: { port: 1430, strictPort: true },
  // Every module's dependencies found up front: one found late makes Vite
  // reload the page mid-shot.
  optimizeDeps: { entries: ["index.html", "preview.html", "src/**/*.tsx"] },
  logLevel: "warn",
});
await server.listen();
const base = "http://localhost:1430/preview.html";

const browser = await chromium.launch({ channel: "msedge", headless: true });
let failures = 0;
try {
  // The scenario list comes from the preview itself.
  const probe = await browser.newPage();
  await probe.goto(`${base}?bare&nosplash&latency=0`);
  const scenarios = await probe.evaluate(() => window.__couchScenarios ?? []);
  await probe.close();

  const shots = [
    ...scenarios.map((s) => [s, s, null]),
    ...SHOTS,
  ].filter(([name]) => name.includes(only));

  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });

  for (const look of looks) {
    for (const [width, height] of sizes) {
      const context = await browser.newContext({
        viewport: { width, height },
        reducedMotion: "reduce",
        ...LOOKS[look],
      });
      for (const [name, scenario, steps] of shots) {
        const page = await context.newPage();
        const errors = [];
        page.on("pageerror", (e) => errors.push(e.message));
        const which = scenario === "sheet" ? "sheet" : `scenario=${scenario}`;
        await page.goto(`${base}?${which}&bare&nosplash&latency=0&theme=system`);
        await page.waitForLoadState("networkidle");
        await page.waitForTimeout(250);
        if (steps) {
          try {
            await steps(page);
            await page.waitForTimeout(250);
          } catch (e) {
            errors.push(`steps: ${e.message.split("\n")[0]}`);
          }
        }
        const file = join(out, `${name}.${look}.${width}x${height}.png`);
        await page.screenshot({ path: file });
        // Text wider than its box, spilling out or cut without an ellipsis.
        if (look === "dark") {
          const spills = await page.evaluate(overflowing);
          if (spills.length) {
            failures++;
            console.log(`✗ ${name} (${width}x${height}): text overflows: ${spills.join("; ")}`);
          }
        }
        if (errors.length) {
          failures++;
          console.log(`✗ ${name} (${look}, ${width}x${height}): ${errors.join("; ")}`);
        }
        await page.close();
      }
      await context.close();
    }
  }
  console.log(`Shots in ${out}`);

  // ---- The no-jump check ----
  for (const [name, scenario, regions, moves] of STABLE.filter(([n]) => n.includes(only))) {
    const context = await browser.newContext({ viewport: { width: 1200, height: 760 }, reducedMotion: "reduce" });
    const page = await context.newPage();
    await page.goto(`${base}?scenario=${scenario}&bare&nosplash&latency=0&theme=system`);
    await page.waitForLoadState("networkidle");
    const boxes = async () =>
      Promise.all(
        regions.map(async (r) =>
          JSON.stringify(await page.locator(r).first().boundingBox({ timeout: 2000 }).catch(() => null)),
        ),
      );
    // The first move opens the view (a dialog, Settings): the boxes after it
    // are the ones every later move must keep.
    let first = null;
    for (const [label, move] of moves) {
      try {
        await move(page);
      } catch (e) {
        failures++;
        console.log(`✗ ${name}: "${label}" failed: ${e.message.split("\n")[0]}`);
        break;
      }
      await page.waitForTimeout(150);
      const now = await boxes();
      if (first == null) {
        first = now;
        continue;
      }
      regions.forEach((r, i) => {
        if (now[i] !== first[i]) {
          failures++;
          console.log(`✗ jump in ${name}: ${r} moved after "${label}": ${first[i]} → ${now[i]}`);
        }
      });
    }
    await context.close();
  }
} finally {
  await browser.close();
  await server.close();
}
if (failures) {
  console.log(`${failures} problem(s).`);
  process.exit(1);
}
console.log("No problems.");
