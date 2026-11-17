# The invite page

Chat apps such as Discord and WhatsApp don't make `couchco://` links clickable. With an invite page, the app shares invites as `https://<page>/j/#<invite>` instead:
- the page shows the room's name and an "Open in Couch Co-App" button, which is the `couchco://join#<invite>` link;
- it offers "Copy the invite" for pasting into the app's Join box;
- it offers the download, for the visitor's system first (M26c-3): a button to the repository's latest release, naming the file to pick there, and a link for the other system. A Mac or a phone is told the app runs on Windows and Linux PCs;
- it says when the room's owner uses a relay of their own whose code the link doesn't carry (an invite of version 1 or 2 naming a relay that isn't one of iroh's public ones): "if the app asks for one, ask them for it". It reads the relay's address only, never the code.

The app's Join box takes both kinds of link.

**What the page never does:**
- **Receive the invite.** The invite is the part after `#`, which browsers never send to a server. The page reads it in the browser.
- **Make requests,** other than for its own three files (its script, its styles and the app's icon). The download links are links the visitor follows, never fetched by the page.
- **Send a referrer, or use cookies or analytics.**

A strict Content-Security-Policy enforces all three.

Opening a `couchco://` link only fills in the app's join screen. Joining still needs the password and a click.

## Files

- `j/index.html`, `j/invite.js`, `j/invite.css`: the page, served at `/j/`, in the app's look (dark or light as the visitor's system is). `index.html`'s `couch-releases` meta tag names the repository whose releases the download opens (`owner/name`): empty in the repository, written by the workflow as it publishes the page. Left empty (served from here), the page asks the visitor to get the app from whoever invited them.
- `j/icon.svg`: the app's icon (a copy of `apps/desktop/src-tauri/icons/source/icon.svg`), for the page and its tab.
- `check.js`: run `node deploy/invite-page/check.js` after changing the page. It reads invites the app made, the payloads pinned in `couch_proto::invite`'s tests (`the_layout_is_pinned`, `the_handed_over_layout_is_pinned`, `the_invites_for_the_relay_note_are_pinned`), and checks the systems the download tells apart and the placeholder the workflow fills in. `couch_net::endpoint`'s `the_invite_page_knows_the_public_relays` checks the page's pattern for iroh's public relays against iroh's list.

## Hosting

Any static host works: it only needs to serve the four files over https.
- **GitHub Pages** (free; P4). The repository's workflow, `.github/workflows/invite-page.yml`, checks the page (`check.js`), writes the repository into it (`github.repository`) and publishes `j/` on every push to main that changes it. Switch it on once: the repository's Settings → Pages → Source: "GitHub Actions". Do it before merging the workflow, whose own push runs it; otherwise run it again by hand afterwards (Actions → Invite page → Run workflow). The page is then at `https://<user>.github.io/<repository>/j/`.
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
4. To see the download as published, put a repository in the served copy's `couch-releases` meta tag (`content="owner/name"`); never commit it there.
