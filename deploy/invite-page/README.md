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
- **GitHub Pages** (free; P4). The repository's workflow, `.github/workflows/invite-page.yml`, checks the page (`check.js`) and publishes `j/` on every push to main that changes it. Switch it on once: the repository's Settings → Pages → Source: "GitHub Actions". Do it before merging the workflow, whose own push runs it; otherwise run it again by hand afterwards (Actions → Invite page → Run workflow). The page is then at `https://<user>.github.io/<repository>/j/`; ours is `https://marcusvilete.github.io/couch-coapp/j/`.
- **The relay's server.** iroh-relay serves only the relay, so this would need a separate web server there (not set up).

**Then point builds at it:**
- **Release builds:** set `invite_page` in `release.toml` to the page's address, ending in `/j/`.
- **One build only:** `scripts\build-installer.ps1 -InvitePage <address>` overrides that. It works for `-TestRelay` builds too, which otherwise have none. `-NoInvitePage` builds with `couchco://` links.
- **Every build prints which it uses.** Builds without a page share `couchco://` links, as before.

**Using someone else's page.** The page doesn't depend on the relay, so a build of your own can link to ours, or to a copy you host.
- **Whoever runs the page could see the invites opened with it.** The page runs in the reader's browser and reads the invite there. Ours sends nothing anywhere, but a changed page could.
- **The room's password still guards it,** since joining needs the invite and the password together.
- **Hosting your own copy** (a fork's Pages) keeps that trust with you.

## Trying it on this PC

1. Serve the page: `npx --yes http-server deploy/invite-page -p 8080`.
2. Make a test build that links to it. In a test copy of `release.toml` (`test = true`), set `invite_page = "http://localhost:8080/j/"`. For a development build, set `COUCH_BUILD_INVITE_PAGE=http://localhost:8080/j/` in the terminal before `npm run tauri dev`.
3. Copy an invite in the app and open it in the browser. The page shows the room's name, and its button opens the app's join screen.
