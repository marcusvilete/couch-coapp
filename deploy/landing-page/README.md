# The landing page

The project's front page, at the root of its GitHub Pages site (`https://<owner>.github.io/<repository>/`; ours is https://marcusvilete.github.io/couch-coapp/). The invite page lives beside it at `j/` ([its README](../invite-page/README.md)). It says what the app is, how it works in plain words, what relays are, its features, what keeps a host safe, what you need, and common questions, and offers the download for the visitor's system.

**What it never does,** like the invite page: no requests but its own files (no web fonts, analytics or cookies), no referrer. Its Content-Security-Policy enforces it.

## Files

- `index.html`, `landing.css`, `landing.js`: the page, in the app's look (dark or light as the visitor's system is, Windows' High Contrast respected). How it works and Features are **pickers**: on a wide screen, the items in a column and the chosen one's picture beside them, in a frame of one size (choosing moves nothing; arrow keys move along); on a narrow screen or without scripts, each item's text, then its picture. Its links name the upstream repository (`marcusvilete/couch-coapp`, and its Pages address in the canonical and Open Graph tags); the site's build replaces them with the repository it publishes from, so a fork's page points at the fork.
- The download comes from the invite page's `j/download.js` (the visitor's system, the file to pick on the release page), shared by both pages; `landing.js` only fills it in. Without scripts, the button opens the latest release as it is.
- The icon is the invite page's `j/icon.svg`.
- `images/`: pictures of the app, each `-dark` and `-light` (the page shows the one matching the visitor's theme), taken from the preview's `site-*` scenarios (`apps/desktop/src/preview/scenarios.ts`: a public build on the public relays, everyone on this version, Fling to the Finish streamed) by one script, which writes them here:

  ```
  cd apps/desktop
  npm run site-shots                  # every picture
  npm run site-shots -- --only step   # those whose name contains "step"
  ```

  Its list (`scripts/site-shots.mjs`) says what each shows. Dialogs are cut out alone on transparency at twice the pixels, grown to their whole content; the rest are whole windows. Retake them when the app's look changes; a picture whose size changes needs its `width` and `height` in `index.html` changed too (the site's build fails until they match).
- **HDR, before and after** (`hdr-before.jpg`, Windows' own capture; `hdr-after.jpg`, converted): the author's captures of the same frame streamed from a monitor in HDR with Settings → Streaming's conversion off and on, shown as a slider (`.compare`: the converted picture below, the other over it cut at the line; a range input over both moves it, so mouse, touch and the arrow keys work; without scripts, split at the middle). Both must be the same size and frame.
- **Drawing over a real game** (`drawing.jpg`): the author's own capture, one picture whatever the theme, never written by the script. Made from a full-size PNG through Edge's canvas: cropped to leave a stray doodle out, JPEG at quality 0.84, about 370 KB (the original was 4.5 MB). Originals stay out of the repository. The author also took a friend's stream window over the game, and asking for the controller; the page keeps the app's own pictures there (the author's choice).

## Building and trying it

`deploy/build-site.mjs` puts the site together, both pages and the repository written in, and fails if a page names a file of its own that isn't there:

```
node deploy/build-site.mjs target/site
npx --yes http-server target/site -p 8080
```

Then open `http://localhost:8080/`. The repository defaults to the workspace's (`Cargo.toml`'s `repository`); a second argument (`owner/name`) names another. The site's workflow (`.github/workflows/invite-page.yml`) runs the same on every push to main that changes either page, with the repository it runs in.

Edit the page's files with an editor that keeps them UTF-8 without a byte-order mark: PowerShell 5.1's `Get-Content`/`Set-Content` mangle its `·` and `á`.
