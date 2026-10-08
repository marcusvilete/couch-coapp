# M7 results: our own relay, built and tested before there's a server

Production must use our own relay: iroh's public relays are for development only, and the public release (M10b) needs one. There's no VPS or domain yet, so M7 builds and tests everything that doesn't need them. Putting it on a server waits ([deploy/relay/README.md](../deploy/relay/README.md)).

Your decisions:
- **the https invite page:** built now;
- **Settings → Network:** "Couch Co-App relay", the default once a build has one; iroh's public relays, "for testing"; or your own URL. A profile that already saved a choice keeps it.
- **LAN tests:** debug builds and test installers accept `http://` relays at private addresses. Release installers stay https-only.

Your question before the plan: how do secrets stay out of git? The design is in [ADR 0007](adr/0007-our-relay-and-release-configuration.md):
- **The relay's token isn't a real secret:** it ships inside every installer. It keeps other apps off our relay, and the server's limits and the bandwidth cap protect it. It still stays out of git, as every release value does, so the repository can be made public one day.
- **The rule for real secrets:** a private file or hardware outside the repository, handed to the build by the build script. That covers signing keys later too (M10b, M11).

## What it does

**Our relay** (iroh-relay 1.3.0, iroh's own server, the version of our iroh pin; MIT/Apache)
- **Who it admits:** only clients with its token (`access.shared_token`).
- **Its limits:**
  - each client may send 2 MB/s, bursting to 4 MB (a host relaying to 7 viewers at the app's 2 Mbit/s relay cap needs about 1.75 MB/s);
  - 20 new connections a second across all clients, bursting to 100.
- **Metrics:** on localhost only.
- **On this PC:** `cargo xtask relay-tool` builds the official binary into `target\tools` (51 s). `scripts\relay.ps1` runs it in `--dev` mode: plain HTTP on port 3340, with production's limits and the token from `release.toml`. `-Lan` opens it to your other PC; `-Open` drops the token.
- **The deploy kit** (`deploy/relay/`, untested until there's a server):
  - `iroh-relay.toml`: Let's Encrypt, QUIC address discovery on UDP 7842, the limits, a token placeholder;
  - a sandboxed systemd unit;
  - `install.sh`: builds the pinned version, installs the files, asks for the token, adds firewall rules;
  - `README.md`: choosing a server, with about 1.8 GB per relayed viewer per hour and free options and their catches; a name; the first run; rotating the token; metrics; updating.

**The app's side**
- **Sending the token:** `RelayChoice::Custom { urls, token }` attaches it to our relay's entries only. iroh sends it nowhere else, so invites and member records never carry it, and it prints as `..`.
- **Built into release builds:** the relay's URL and token, and the invite page's address, come from `COUCH_BUILD_*` at build time (`couch_engine::build_config`). Development builds get none.
- **Settings → Network:**
  - **The choices:** the build's relay ("Couch Co-App relay (host)"; iroh's public relays in a development build), iroh's public relays (for testing), or My relay.
  - **Defaults:** a profile without a saved choice follows the build. Saved `public` or URL choices stay.
  - **The note:** "Everyone in a room should use the same choice here". A member set to another relay can't use ours, so a connection between such members needs a direct path.
- **Plain http relays:** always on this PC; at private LAN addresses (10/8, 172.16/12, 192.168/16) in development and test builds only.
- **`couch-cli`:** `--relay-token` goes with `--relay <url>`.

**The release configuration** (`release.toml`, outside the repository)
- **Creating it:** `cargo xtask release-config` writes `%USERPROFILE%\.couch-coapp\release.toml` with a fresh 256-bit token and prints the server's environment line. It refuses to overwrite an existing file, and refuses a path inside the repository.
- **What it holds:** `relay_url`, `relay_token`, `invite_page`, and `test` (allows http, names the installer `-test`). `deploy/release.example.toml` shows the shape with placeholders.
- **`build-installer.ps1` reads it** (or `-Config <file>`):
  - it refuses one inside the repository, an http URL outside a test file, or a missing token;
  - it passes the values to that build only, and unsets them afterwards.
- **Without the file,** only `-TestRelay` builds: iroh's public relays, like today's installers, named `-test`.
- **`doctor`** reports the relay tool's version against the iroh pin, and whether a release configuration exists.

**The invite page** (`deploy/invite-page/j/`)
- **What it shows:** "You're invited to <room>", an "Open in Couch Co-App" button (the `couchco://` link), and "Copy the invite". If you don't have the app, it says to ask whoever invited you for the installer.
- **How it treats the link:**
  - it reads only the part after `#`, which browsers never send to the server;
  - it makes no requests other than for its own two files;
  - it sends no referrer;
  - a strict Content-Security-Policy enforces the above;
  - a link that isn't an invite says so.
- **The app** shares `<invite_page>#<payload>` when the build has a page; otherwise `couchco://` links, as before. The Join box already took both.
- **Hosting** is for later, on GitHub Pages or the server (its README).

## Review

A review subagent read the change. It found nothing high-severity. Each finding was checked against the code:
1. **The committed placeholder token worked as a real token** (medium). Without `IROH_RELAY_ACCESS_TOKEN` (for example, mid-rotation after `install.sh` was run again), the relay would admit anyone who had read the repository. The configs now hold an empty list, `[""]`, which iroh-relay refuses to start with: it fails closed, which was checked with the real binary. The README warns not to run `install.sh` during a rotation.
2. **A malformed `relay_url` made a "release" without our relay** (medium). The script's pattern let `https://[bad` through, and the app reads an unparseable URL as none. The script now parses each address (an absolute URL, the right scheme, a host, no user info), and the app logs an error if a build carries a relay URL it can't read.
3. **A saved setting the build can't take fell back to iroh's public relays** (medium), for example a LAN http relay saved by a test build, then a release installed. It now falls back to the build's own relay, and "applies when the app restarts" agrees (new test lines in `the_default_follows_the_build`). CLAUDE.md said public relays run "in development only"; it now says when else they can.
4. **`test = "false"` in quotes read as true** (medium). Each key now takes only its own type: text in quotes, or `true`/`false` without.
5. **`install.sh`** (low) aborted when the distribution's cargo was present, and passed its arguments to `sed` unchecked. It now always uses rustup's toolchain, and checks the host name and email.
6. **The xtask "inside the repository" check was case-sensitive** (low). It now ignores case, as the build script does.
7. **`couch-cli` kept the token as plain text** (low), so `Debug` would print it. It's now a `RelayToken`; `--help` says the command line is visible to other users.
8. **iroh-relay was pinned apart from iroh** (low). It's now a workspace pin next to iroh's.
9. **Not changed:**
   - the in-process test relay checks tokens itself, not through iroh-relay's config; the real binary's config was checked by hand;
   - CGNAT addresses (100.64/10, such as Tailscale) count as not on the LAN, as documented.

## Checks

| Check | What | Result |
|---|---|---|
| Relay choice (couch-net) | The token goes only to our relay's entries, and plain http skips QUIC address discovery; the token never prints | pass |
| Engine unit tests | The default follows the build (dev: public; release: ours with its token; saved choices kept; our URL typed as your own isn't ours); http at LAN addresses only in test builds; an old `network.json` keeps its choice; invite links on the page parse back | pass (68) |
| Our relay, in-process (`couch-engine --test relay`) | Members with the token meet through the relay alone, knowing only its URL; a client without the token, or with another, is turned away | pass, 3 runs |
| Rooms through our relay | Relay-only members: the invite names only the relay; a join; a relayed link; finding each other again after a restart | pass, 3 runs (rooms 22) |
| Sessions through our relay | Relay-only: watching, then getting the seat | pass, 3 runs (sessions 32) |
| The real relay binary (`relay.ps1`) | Starts with `dev.toml` and answers on localhost:3340. With a token: `couch-cli host --relay-token` connects; without it, the relay logs "denied our authentication (not authorized)" | pass |
| `xtask release-config` | Creates the file (tried with a scratch path); refuses to overwrite it, and refuses a path inside the repository | pass |
| The invite page (`node deploy/invite-page/check.js`) | Reads the name "Sofá da Ana" from the invite the app made (now pinned in couch-proto, `the_layout_is_pinned`); takes wrapped, upper-cased and encoded links; refuses junk | pass |
| Workspace, after the review fixes | All tests; clippy (all targets), fmt, `cargo deny` (the relay server's crates are test-only), `tsc`; rooms, sessions and relay scenarios 3 runs each (22, 32, 2); the build script's address checks and the config reader's type checks with sample files; the relay refusing to start without its token | pass |
| G6 through our relay (release build, `latency-probe --relay http://localhost:3340 --relay-token … --relay-only`, 20 s, the real relay binary with a token) | 1165 frames at 59.9 fps, all relayed; 2.0 Mbit/s, the app's relay cap, held; clock to present p50 13.1, p95 14.1, max 28.2 ms; no lost packets, decode errors, catch-ups or audio concealment. On loopback, so this shows the path and the cap, not real-network latency | pass |
| Your run (one PC) | Your own `release.toml`; the relay with your token; a development build with it built in: Settings showed the Couch Co-App relay, the profiles met and watched; the invite page: Copy invite gave the page's link, which showed the room and opened the join screen | pass |

**Seen along the way:**
- **The disk filled up.** `target\debug` had grown to 184 GB, and a link failed for lack of space (LNK1140). The incremental cache (70 GB) and the leftover object files were deleted, which only makes the next build of each crate a full one.
- **The rest was stale too.** `target\debug\deps` still held 109 GB, mostly old builds. With your go-ahead, `cargo clean --profile dev` removed `target\debug` (128 GB in all; the release build, the installer and the relay tool stayed), and `stage-dlls` put its DLLs back.
- **"Direct" in your run is expected.** On one PC a direct path always exists, so iroh moves each connection off the relay once it's up. The relay started it: the invite named only the relay, and Settings showed it connected with your token. The relay carrying traffic is covered by the relay-only scenarios and the G6 run above. The app showing "Relay" on a real network waits for a LAN test or the deployed relay.

## Your run

**On this PC** (about 10 minutes):
1. **The release configuration** (yours, private): `cargo xtask release-config`. Keep the token it prints in a password manager.
2. **Our relay:** run `cargo xtask relay-tool` once (already built here), then `.\scripts\relay.ps1` in its own terminal.
3. **A development build with the relay built in.** In another terminal, set `$env:COUCH_BUILD_RELAY_URL='http://localhost:3340'` and `$env:COUCH_BUILD_RELAY_TOKEN='<the token>'`, then start `npm run tauri dev` in `apps\desktop`, and `.\scripts\profiles.ps1` (alice) from the same terminal.
   - Settings → Network shows "Couch Co-App relay (localhost:3340)", connected through localhost.
   - alice and the default profile meet, and alice watches a stream.
4. **The invite page.** Stop the app. Also set `$env:COUCH_BUILD_INVITE_PAGE='http://localhost:8080/j/'`, start the app again, and serve the page with `npx --yes http-server deploy/invite-page -p 8080`.
   - Copy invite gives `http://localhost:8080/j/#…`. Opening it in the browser shows the room's name; its button opens the app's join screen, which still needs the password.
5. **Afterwards:** close that terminal, or remove the three variables, so later builds don't carry them.

**With your other PC on the LAN** (optional):
1. Copy `release.toml` to a test file with `test = true` and `relay_url = "http://<this PC's LAN address>:3340"`.
2. Run `.\scripts\relay.ps1 -Lan`, and allow it through Windows Firewall when asked.
3. Build `build-installer.ps1 -Config <test file>` and install it on both PCs (or set the same LAN URL in this PC's terminal).
4. Watch and play across the two PCs. With `RUST_LOG=iroh_relay=debug`, the relay's window shows both connecting.
