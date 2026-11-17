// Puts the project's web site together: the landing page (deploy/landing-page) at its
// root and the invite page (deploy/invite-page/j) under j/, for GitHub Pages
// (.github/workflows/invite-page.yml) or for trying it here:
//
//   node deploy/build-site.mjs <out folder> [owner/name]
//   npx --yes http-server <out folder> -p 8080
//
// The repository (by default the workspace's, Cargo.toml's `repository`) names whose
// releases the downloads open and whose source the links show, so a fork's site points at
// the fork. The landing page is written with the upstream's addresses, replaced here. It
// fails if a page links to a file of its own that isn't there.
import { cpSync, existsSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const DEPLOY = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(DEPLOY, "..");
const UPSTREAM = "marcusvilete/couch-coapp";
const REPOSITORY = /^[A-Za-z0-9-]+\/[A-Za-z0-9._-]+$/;

function fail(message) {
  console.error(`build-site: ${message}`);
  process.exit(1);
}

const [outArg, repoArg] = process.argv.slice(2);
if (!outArg) fail("usage: node deploy/build-site.mjs <out folder> [owner/name]");
const out = resolve(outArg);
if (out === ROOT || ROOT.startsWith(out + "/") || ROOT.startsWith(out + "\\") || out.startsWith(DEPLOY)) {
  fail(`refusing to write into ${out}`);
}

function workspaceRepository() {
  const cargo = readFileSync(join(ROOT, "Cargo.toml"), "utf8");
  const found = cargo.match(/^repository\s*=\s*"https:\/\/github\.com\/([^"]+?)\/?"/m);
  return found ? found[1] : UPSTREAM;
}
const repository = repoArg || workspaceRepository();
if (!REPOSITORY.test(repository)) fail(`not a repository: ${repository}`);
const [owner, name] = repository.split("/");

rmSync(out, { recursive: true, force: true });
cpSync(join(DEPLOY, "landing-page"), out, {
  recursive: true,
  filter: (path) => !/(README\.md|check\.js)$/.test(path),
});
cpSync(join(DEPLOY, "invite-page", "j"), join(out, "j"), { recursive: true });

// The landing page: the upstream's addresses become this repository's.
const landing = join(out, "index.html");
let page = readFileSync(landing, "utf8");
page = page
  .replaceAll(`https://github.com/${UPSTREAM}`, `https://github.com/${repository}`)
  .replaceAll("https://marcusvilete.github.io/couch-coapp/", `https://${owner.toLowerCase()}.github.io/${name}/`);
writeFileSync(landing, page);

// The invite page: the repository whose releases its download opens.
const invite = join(out, "j", "index.html");
const placeholder = '<meta name="couch-releases" content="">';
const named = `<meta name="couch-releases" content="${repository}">`;
const invitePage = readFileSync(invite, "utf8");
if (!invitePage.includes(placeholder)) fail("the invite page's couch-releases placeholder is gone");
writeFileSync(invite, invitePage.replace(placeholder, named));

// Every file a page names of its own must be there.
for (const html of [landing, invite]) {
  const text = readFileSync(html, "utf8");
  for (const [, ref] of text.matchAll(/(?:src|href|srcset)="([^"]+)"/g)) {
    if (/^(https?:|mailto:|#|couchco:)/.test(ref)) continue;
    const file = resolve(dirname(html), ref.split(/[?#]/)[0]);
    if (!file.startsWith(out) || !existsSync(file)) fail(`${html.slice(out.length + 1)} names ${ref}, which isn't there`);
  }
}

// Every picture the landing page declares at its own size (so nothing moves as it loads),
// its dark twin the same.
function sizeOf(file) {
  const bytes = readFileSync(file);
  if (bytes.readUInt32BE(0) === 0x89504e47) return [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
  for (let at = 2; at + 9 < bytes.length; at += 2 + bytes.readUInt16BE(at + 2)) {
    const marker = bytes[at + 1];
    if (marker >= 0xc0 && marker <= 0xc2) return [bytes.readUInt16BE(at + 7), bytes.readUInt16BE(at + 5)];
  }
  fail(`can't read the size of ${file}`);
}
for (const [, src, width, height] of page.matchAll(/<img src="(images\/[^"]+)" width="(\d+)" height="(\d+)"/g)) {
  const twins = [src, src.replace("-light.", "-dark.")];
  for (const file of new Set(twins)) {
    const [w, h] = sizeOf(join(out, file));
    if (`${w}x${h}` !== `${width}x${height}`) fail(`${file} is ${w}x${h}, but the page says ${width}x${height}`);
  }
}

const files = readdirSync(out, { recursive: true }).length;
console.log(`build-site: ${files} files in ${out}, for ${repository}`);
