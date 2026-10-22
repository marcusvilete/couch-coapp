// Run after `vite build`: the app's build must hold none of the preview's
// fake engine (M21). The preview is chosen by its own entry, never as a
// fallback, so this only fails if something imports it by mistake.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const dir = join(import.meta.dirname, "..", "dist", "assets");
const marks = ["fakeEngine", "loadWorld", "__couchScenarios", "someoneAsks"];
const found = readdirSync(dir)
  .filter((f) => f.endsWith(".js"))
  .flatMap((f) => marks.filter((m) => readFileSync(join(dir, f), "utf8").includes(m)).map((m) => `${f}: ${m}`));
if (found.length) {
  console.error(`The build carries preview code:\n  ${found.join("\n  ")}`);
  process.exit(1);
}
