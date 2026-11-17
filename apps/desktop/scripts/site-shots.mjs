// The landing page's pictures of the app (deploy/landing-page/images), from the preview's
// `site-*` scenarios, and the self-hosting guide's (docs/images), from its `guide-*` ones,
// dark and light, through the Microsoft Edge already on Windows
// (headless; Playwright downloads nothing):
//
//   npm run site-shots                   every picture
//   npm run site-shots -- --only step    those whose name contains "step"
//
// Retake them when the app's look changes. Dialogs are cut out alone, on transparency, at
// twice the pixels; the rest are whole windows. The drawing over a real game (drawing.jpg) is
// the author's capture, never written here.
import { mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { createServer } from "vite";

const app = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const site = resolve(app, "../../deploy/landing-page/images");
const guide = resolve(app, "../../docs/images");

const i = process.argv.indexOf("--only");
const only = i >= 0 ? process.argv[i + 1] : "";

const click = (name) => async (page) => page.getByRole("button", { name, exact: false }).first().click();
const settings = (section) => async (page) => {
  await page.getByRole("button", { name: "Settings (Ctrl+,)" }).click();
  await page.getByRole("button", { name: section, exact: true }).click();
};
const DIALOG = '[role="dialog"]';

/** [name, scenario, steps, what to keep (null: the window), size, folder (the landing page's)]. */
const SHOTS = [
  // At the top of the page.
  ["hosting", "site-hosting", null, null, [1280, 860]],
  // How it works
  ["step-room", "site-first-run", async (page) => {
    await click("Create a room")(page);
    await page.getByLabel("Room name").fill("Sofá");
    await page.getByLabel("Password", { exact: true }).fill("couch-night-42");
    await page.getByLabel("Password again").fill("couch-night-42");
  }, DIALOG, [1100, 760]],
  ["step-invite", "site-owner", click("Invite"), DIALOG, [1100, 760]],
  ["step-stream", "site-hosting-idle", async (page) => {
    await click("Start streaming…")(page);
    await page.getByText("Fling to the Finish", { exact: true }).click();
  }, DIALOG, [1100, 800]],
  ["step-play", "site-playing", null, null, [1280, 640]],
  // Features
  // Four controllers, a keyboard seat, both of the host's controllers on seats.
  ["seats", "site-owner", async (page) => {
    await click("Start a session")(page);
    for (let n = 0; n < 3; n++) await click("One controller more")(page);
    await page.getByRole("radio", { name: "A seat of their own" }).click();
    await page.getByRole("checkbox", { name: /Xbox Wireless Controller/ }).click();
    // Ticking it scrolled the dialog: back to its title.
    await page.locator(`${DIALOG} *`).evaluateAll((els) => els.forEach((el) => (el.scrollTop = 0)));
  }, DIALOG, [1100, 1300]],
  ["bay", "site-hosting", async (page) => {
    await page.locator(".bay").evaluate((el) => el.scrollIntoView({ block: "center" }));
    await page.waitForTimeout(150);
  }, ".bay", [1280, 1300]],
  ["together", "site-together", null, null, [1280, 640]],
  ["remote", "site-remote", null, null, [1280, 760]],
  ["linux", "linux-keyboard-idle", click("Start streaming…"), DIALOG, [1100, 800]],
  // Relays
  ["relays", "site-owner", settings("Network"), null, [1280, 760]],
  // The self-hosting guide's "How to know it worked"
  ["settings-network", "guide-own-relay", settings("Network"), null, [1100, 650], guide],
  ["settings-about", "guide-about", settings("About"), null, [1100, 650], guide],
];

const server = await createServer({
  root: app,
  configFile: join(app, "vite.config.ts"),
  server: { port: 1431, strictPort: true },
  optimizeDeps: { entries: ["index.html", "preview.html", "src/**/*.tsx"] },
  logLevel: "warn",
});
await server.listen();
const base = "http://localhost:1431/preview.html";

for (const dir of [site, guide]) mkdirSync(dir, { recursive: true });
const browser = await chromium.launch({ channel: "msedge", headless: true });
let failures = 0;
try {
  for (const look of ["dark", "light"]) {
    for (const [name, scenario, steps, keep, [width, height], out = site] of SHOTS.filter(([n]) => n.includes(only))) {
      // A fresh context each: the preview remembers the last room picked.
      const context = await browser.newContext({
        viewport: { width, height },
        colorScheme: look,
        reducedMotion: "reduce",
        deviceScaleFactor: keep ? 2 : 1,
      });
      const page = await context.newPage();
      const errors = [];
      page.on("pageerror", (e) => errors.push(e.message));
      await page.goto(`${base}?scenario=${scenario}&bare&nosplash&latency=0&theme=system`);
      await page.waitForLoadState("networkidle");
      await page.waitForTimeout(300);
      try {
        if (steps) await steps(page);
        await page.waitForTimeout(400);
        const file = join(out, `${name}-${look}.png`);
        if (keep) {
          // A dialog alone, on transparency: the app behind it and its dimming hidden.
          if (keep === DIALOG) {
            await page.locator(DIALOG).first().evaluate((dialog) => {
              // Everything beside the dialog's line of ancestors hidden, nothing inside it
              // touched (it keeps its own invisible text, a button's reserved label).
              for (let el = dialog; el && el !== document.body; el = el.parentElement) {
                for (const sibling of el.parentElement.children) {
                  if (sibling !== el) sibling.style.visibility = "hidden";
                }
              }
              document.documentElement.style.background = "transparent";
              document.body.style.background = "transparent";
              // The whole dialog, nothing scrolled away inside it.
              // (Dialogs keep one height whatever they show: here it grows to its content.)
              for (const el of [dialog, ...dialog.querySelectorAll("*")]) {
                if (el === dialog || el.scrollHeight > el.clientHeight + 1) {
                  Object.assign(el.style, { height: "auto", minHeight: "0", maxHeight: "none", flex: "none", overflow: "visible" });
                }
              }
            });
            await page.waitForTimeout(150);
          }
          const box = await page.locator(keep).first().boundingBox();
          const m = keep === DIALOG ? 28 : 0;
          const x = Math.max(0, box.x - m);
          const y = Math.max(0, box.y - m);
          await page.screenshot({
            path: file,
            clip: { x, y, width: Math.min(width - x, box.width + 2 * m), height: Math.min(height - y, box.height + 2 * m) },
            omitBackground: keep === DIALOG,
          });
        } else {
          await page.screenshot({ path: file });
        }
      } catch (e) {
        errors.push(e.message.split("\n")[0]);
      }
      if (errors.length) {
        failures++;
        console.log(`✗ ${name} (${look}): ${errors.join("; ")}`);
      }
      await context.close();
    }
  }
} finally {
  await browser.close();
  await server.close();
}
console.log(failures ? `${failures} failed` : `Pictures in ${site} and ${guide}`);
process.exit(failures ? 1 : 0);
