# The invite page

Chat apps such as Discord and WhatsApp don't make `couchco://` links clickable. With an invite page, the app shares invites as `https://<page>/j/#<invite>` instead:
- the page shows the room's name and an "Open in Couch Co-App" button, which is the `couchco://join#<invite>` link;
- it offers "Copy the invite" for pasting into the app's Join box;
- it says how to get the app.

The app's Join box takes both kinds of link.

**What the page never does:**
- **Receive the invite.** The invite is the part after `#`, which browsers never send to a server. The page reads it in the browser.
- **Make requests,** other than for its own two files.
- **Send a referrer, or use cookies or analytics.**

A strict Content-Security-Policy enforces all three.

Opening a `couchco://` link only fills in the app's join screen. Joining still needs the password and a click.

## Files

- `j/index.html`, `j/invite.js`, `j/invite.css`: the page, served at `/j/`.
- `check.js`: run `node deploy/invite-page/check.js` after changing the page. It reads an invite the app made: the payload pinned in `couch_proto::invite`'s `the_layout_is_pinned` test.

## Hosting

Any static host works: it only needs to serve the three files over https.
- **GitHub Pages** (free). Publish this folder from a repository; the page is then at `https://<user>.github.io/<repository>/j/`.
- **The relay's server.** iroh-relay serves only the relay, so this would need a separate web server there (not set up).

Then set `invite_page` in `release.toml` to the page's address, ending in `/j/`, and build the installer. Builds without it share `couchco://` links, as before.

## Trying it on this PC

1. Serve the page: `npx --yes http-server deploy/invite-page -p 8080`.
2. Make a test build that links to it. In a test copy of `release.toml` (`test = true`), set `invite_page = "http://localhost:8080/j/"`. For a development build, set `COUCH_BUILD_INVITE_PAGE=http://localhost:8080/j/` in the terminal before `npm run tauri dev`.
3. Copy an invite in the app and open it in the browser. The page shows the room's name, and its button opens the app's join screen.
