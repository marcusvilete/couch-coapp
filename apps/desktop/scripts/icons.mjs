// The app's icon files, rendered from the drawings in src-tauri/icons/source
// through the Microsoft Edge already on Windows (headless; Playwright
// downloads nothing). `tauri icon` makes every PNG and the .icns from the
// large drawing; then icon.ico is packed again with the heavier drawings at
// 16, 24 and 32 px, which Windows picks for the tray, small icons and the
// taskbar (a large drawing scaled down goes faint there).
//
//   npm run icons
import { execSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

const app = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const icons = join(app, "src-tauri", "icons");
const source = (name) => readFileSync(join(icons, "source", `${name}.svg`), "utf8");

// Each size of icon.ico and the drawing it comes from.
const ICO = [
  [16, "icon-16"],
  [24, "icon-24"],
  [32, "icon-32"],
  [48, "icon"],
  [64, "icon"],
  [256, "icon"],
];

const browser = await chromium.launch({ channel: "msedge", headless: true });
const page = await browser.newPage();

async function render(name, size) {
  await page.setViewportSize({ width: size, height: size });
  const svg = source(name).replace(/width="1024" height="1024"/, `width="${size}" height="${size}"`);
  await page.setContent(`<html><body style="margin:0;background:transparent">${svg}</body></html>`);
  return page.locator("svg").screenshot({ omitBackground: true });
}

const work = mkdtempSync(join(tmpdir(), "couch-icons-"));
try {
  const large = join(work, "icon-1024.png");
  writeFileSync(large, await render("icon", 1024));
  execSync(`npx tauri icon "${large}" --output "${icons}"`, { cwd: app, stdio: "inherit" });
  // `tauri icon` also writes Android and iOS sets and a 64 px PNG, which this
  // app doesn't ship (tauri.conf.json's bundle names the rest).
  for (const name of ["android", "ios", "64x64.png"]) rmSync(join(icons, name), { recursive: true, force: true });

  const images = [];
  for (const [size, name] of ICO) images.push([size, await render(name, size)]);
  writeFileSync(join(icons, "icon.ico"), ico(images));
  console.log(`icon.ico: ${ICO.map(([s, n]) => `${s} px from ${n}.svg`).join(", ")}`);
} finally {
  await browser.close();
  rmSync(work, { recursive: true, force: true });
}

/** An .ico holding PNG images (Windows Vista and later read them). */
function ico(images) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2); // an icon, not a cursor
  header.writeUInt16LE(images.length, 4);
  const entries = [];
  let offset = 6 + 16 * images.length;
  for (const [size, png] of images) {
    const entry = Buffer.alloc(16);
    entry.writeUInt8(size >= 256 ? 0 : size, 0); // 0 means 256
    entry.writeUInt8(size >= 256 ? 0 : size, 1);
    entry.writeUInt16LE(1, 4); // colour planes
    entry.writeUInt16LE(32, 6); // bits per pixel
    entry.writeUInt32LE(png.length, 8);
    entry.writeUInt32LE(offset, 12);
    entries.push(entry);
    offset += png.length;
  }
  return Buffer.concat([header, ...entries, ...images.map(([, png]) => png)]);
}
