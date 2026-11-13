# ADR 0014: Public builds, and relays you register

**Status:** accepted (2026-11-13), the author's decisions, for M26. It amends [ADR 0009](0009-source-only-releases.md) (public installers come back, unsigned, with a notice of new versions; no updater, no wire freeze) and [ADR 0007](0007-our-relay-and-release-configuration.md) (a relay's token is no longer only baked into a build: the user registers it).

The author's choices (2026-11-13):
- **public binaries, built by CI** and published as GitHub Releases;
- **unsigned**, with a note on getting past SmartScreen;
- **a notice of new versions**, from GitHub's latest release, with a switch to turn it off;
- **several relays registered, all used at once**, each with its own token, so moving between rooms on different relays needs no switching (revised the same day, after the check below showed it works: the first draft had one relay in use at a time, which needed a restart to move between rooms);
- **the app says what went wrong and what to do**, whatever it is ("users must have the absolute best experience and understand when something goes wrong");
- **relay codes travel by themselves** between members of a room, and in invites (by default, with a switch per link), so nobody copies a code to play with their room;
- **the wizard comes when it matters,** on creating a room or an invite while on the public relays only, not at every start.

**Settled while building M26a-1** (2026-11-13; [results](../m26a-results.md)):
- **A relay added, removed or given a new code applies while running,** with one change to iroh 1.3: a relay's connection reads its token once at start, so a new code for a refusing relay, a relay added after a refused dial, and a removed relay kept the old token (the home relay for good). iroh is vendored with ~50 lines that restart a relay's connection when its map entry changes (`third_party/iroh/PATCHED.md`, the author's choice); no restart is ever needed.
- **iroh reports on the home relay only,** so the others' states come from its net report, the relays our connections run through, and checking each code now and then with a throwaway endpoint of a random key (our own key twice on one relay would have it replace our real connection).
- **A relay refusing our code is benched** out of the endpoint's list until a check lets us in or its code changes: iroh picks its home relay by speed and keeps one that refuses us.
- **The public relays are iroh's default relay map entries,** never its DNS address lookup (`presets::Minimal` everywhere): records and invites already carry addresses.
- **They may stand in** while none of ours works (refusing or not answering, once lasting), as a setting off by default (the author's choice); they go as soon as one of ours answers.
- **Diagnostics are anonymous** (the author's choice): versions, relays by server name with states and errors (IP addresses scrubbed), the network, rooms and members numbered.
- **Records don't follow a member to a relay nobody else holds:** with no connection left to send its new record on, the others keep dialing its old relay. M26a-1 warns before removing a relay others reach us through; M26a-2's reasons and M26b's codes must account for it.

## Context

**Only the builder can make a release today.** A release build carries its builder's relay and token (`release.toml`, ADR 0007), so every update is an installer built by hand on the maintainer's PC and handed out the same way. The token in it was never secret anyway: anyone with the installer can read it.

**What iroh 1.3 allows** (read in its source, `iroh` and `iroh-relay` 1.3.0):
- **a token per relay.** `RelayConfig::auth_token` belongs to each entry of the relay map, and the relay actor looks it up by URL when it connects (`socket/transports/relay/actor.rs`, `start_active_relay`). Only `RelayMap::with_auth_token` puts one token on every entry. Ours (`RelayChoice::Custom { urls, token }`) has the same one-token shape, which is easy to change;
- **one home relay at a time**, the fastest in the map (it moves only for one much faster: under two thirds of the current one's latency), with no way to pin it, and **peers learn only that one** (our signed member record's `PeerAddr.relay`; an invite's address). To reach a member through its relay, you connect to that relay as a client, so on a relay with a token **you need its token**. iroh connects to a relay that isn't home only when it dials someone there, and lets it go after 60 s without traffic;
- **in a room, both members of a pair dial** (`couch_room::room`: crossed dials keep the lower id's), so a pair connects when **either** holds the other's home relay;
- **a connection says which path it uses** (`Connection::paths`), so whether two members are connected through a given relay is known, not claimed;
- **the server's access control:** everyone, an allow or deny list of endpoint ids, an HTTP callback per connection, or `access.shared_token`, a list of tokens. Our deploy sets `IROH_RELAY_ACCESS_TOKEN`, which replaces the list with a single token. A token per friend is the list in the root-only config instead; taking one back is editing the list and restarting the relay.

**Checked (2026-11-13, a throwaway test on two in-process relays with different tokens, relay only, no direct paths):**
- an endpoint holding both relays, each with its own token, dialed a member holding only the first and one holding only the second: both connected, a stream went each way, and neither relay refused anything (each got its own token). It worked whichever relay was its home;
- held 75 s on the relay that wasn't home (past iroh's 60 s cleanup, the QUIC keep-alive counting as traffic), the connection still carried a stream back;
- a member holding only one relay dialing that endpoint at the relay that **wasn't** its home timed out: it isn't there until it dials someone there. At its home it connected;
- the home relay came out differently for two endpoints with the same two relays.

**iroh's public relays** are for testing and hobby use, with limits n0 doesn't publish. A public build can start on them, but nobody should rely on them for long sessions.

## Decision

### Public builds

- **A tag `v<version>` builds the release in GitHub Actions**: the NSIS installer on a Windows runner and the AppImage on an Ubuntu 24.04 runner, from the tagged commit, through the same `xtask` steps as today (`build-installer.ps1`'s checks, `cargo xtask notices`, `cargo xtask appimage`). The workflow checks that the tag names the version in `Cargo.toml`, `tauri.conf.json` and `package.json`, and attaches to a **draft** release:
  - the installer and the AppImage, with their SHA-256 sums;
  - `THIRD-PARTY-NOTICES.txt` (also inside each package);
  - FFmpeg's pinned source and build scripts, and `SOURCES.txt`.

  The author reads the draft and publishes it. The release notes link the source at the tag.
- **A public build carries no secret.** No relay URL, no token: its default relay is iroh's public relays, said so in Settings and the wizard (below). It carries the invite page's address (public anyway) and the releases' address for the update notice. A new build kind, neither test nor release-with-relay: named without `-test`, refusing a dirty tree like a release.
- **The private route stays.** `release.toml` with a relay still builds a friends' installer by hand (`build-installer.ps1`, `build-appimage.sh`); its relay is a registered relay from the start (below), so nothing changes for friends on it. Self-builders keep both routes.
- **Unsigned.** The release page and the README say how to get past SmartScreen ("More info", "Run anyway") and how to check the SHA-256. Signing can come later, in its own ADR (SignPath's free plan for open source needs CI builds, which this gives).
- **No wire freeze.** Builds of different wire revisions keep refusing each other, and the member list says who needs to update (ADR 0009 stands). The update notice makes that cheap.
- **The ALPN stays `couch-coapp/0`.**

### The update notice

- **Builds that name a releases address check it** (a build value beside the invite page's, set by the CI workflow to this repository; private builds may set it in `release.toml`; development builds and builds without one never check): GitHub's latest-release API, at most once a day, a few minutes after start, never while hosting or watching.
- A newer version shows a notice in the app, "Couch Co-App X is out", with a link to its release page, until dismissed for that version. Nothing is downloaded or installed.
- **Settings → App → "Tell me about new versions"**, on by default, says what it contacts (GitHub, which sees the PC's address). The request names a generic user agent, nothing of the user's.

### Registering relays

- **A registered relay is an address and its token** (or none, for a relay without one), kept in the profile sealed like the room files (DPAPI on Windows, 0600 on Unix), never logged (`..`), never sent through remote play, never in a member record; it travels only as below.
- **Every registered relay is used at once**, each entry of iroh's relay map with its own token (never `RelayMap::with_auth_token`, which would send one relay's token to all). Nothing to choose: whoever holds the relays of the people in their rooms reaches them all, wherever iroh puts its home. A relay added or removed applies while running if `Endpoint::insert_relay` and `remove_relay` hold up in M26a's tests, else at the next start like today's setting (`restart_app`).
- **A relay without a token is open to everyone**, registered or not: iroh dials a member at any relay URL, its map deciding only the tokens and the home relay. So members on iroh's public relays stay reachable whatever we register; only relays with a token need registering.
- **iroh's public relays** are used while nothing is registered (a public build's start). Once a relay is registered they're off, unless the user keeps them on beside it (Settings → Network): mixed in, the home relay could land on a public one, and our sessions' traffic run through relays with limits n0 doesn't publish, rather than the one registered for it. Turning them off breaks no room (above).
- **Today's setting carries over:** a saved `network.json` becomes registered relays at the first start of M26a (a custom address, a relay without a token; `public`, the public relays kept on), and the file stays readable by the version before, so going back to it keeps working.
- **Addresses from others follow the release rules:** a relay address that came from anyone but the user (a member's record, a received code, an invite) is dialed only as https to a name, never to `localhost`, a private or link-local address or a bare IP, in release builds (development and test builds keep LAN relays). Today any address in a member's record is dialed, so a member can make others' apps send requests into their own network; this closes it for records too.
- **A build's own relay** (`release.toml`) is written into the profile's registered relays at its first start, and can't be removed. So installing a public build over a friends' build keeps it, and the rooms on it keep working; a profile with a relay registered never sees the wizard.
- **A relay's first holders get it as a code** (an address and token on one line, with a versioned prefix and a checksum, so a truncated paste is caught and said), from whoever runs the relay, made by the app's Share or by the deploy kit. Pasting it in Settings → Network or the wizard fills the form for the user to confirm; the app **connects with it before saving** (a refused token, or no relay there, is said plainly, with "save anyway" for a relay that's down for now). Never a link: any web page can open a link, and a relay the user adds by hand must come from someone they trust. Everyone after them gets it by itself (below).
- **A code can be personal:** whoever makes it can mark it so (the deploy guide's per-friend tokens are), and a personal code is never passed on, by asking, an invite or Share.
- **"Share this relay"** copies a code, on a click, from any registered relay that may be passed on (below), the build's own included (its token is in the installer anyway), marked for Windows not to keep in its clipboard history or sync it (and for KDE's clipboard history, as a secret, on Linux). Nothing else reads a token back.
- **A token can't be taken back from one person**, only changed for everyone (rotation, `deploy/relay/README.md`), unless the relay's owner gives each friend a personal code of their own (the server's token list) and takes theirs back: the deploy guide says how, and that banning them from the rooms on the relay goes with it.
- **Backups carry the registered relays** (sealed with the backup's passphrase): a token is no more secret than the identity beside it, and a rotated one simply fails.
- **At most 16 relays** are held at once (each one is probed for its latency); a relay received for rooms (below) that none of our rooms has needed for 30 days is forgotten.
- **The wizard comes when it matters, not at start:** on creating a room or making an invite while the only relays are iroh's public ones (a member who joins a friend's room gets its relay by itself and never needs it). It says what a relay is for in a sentence, offers "Paste a relay code" and a link to running your own, "Remind me later" (in 7 days, or at the next version, whichever is first) and "Stay on the public relays" (never again; Settings still offers it).
- **Plain words:** the first time "relay" appears, one line says what it is ("A relay passes your connection along when your PCs can't connect directly"); the app always says "relay" and "relay code", never iroh's terms.

### Relay codes passed on by themselves

The author's choice: nobody should copy a code to play with their own room. So members of a room pass each other the codes its members' relays need, without anyone asking for it by hand.

- **Pulled, never pushed.** A member asks for a code when a member of a room it shares is on a relay it needs and lacks (from that member's signed record: not ours, not iroh's public ones), or when the relay refuses the code it has (a rotation). It asks the members of that room it is connected to, at most once a minute per relay.
- **An answer only proves what the room really uses.** A member answers with the code of relay R only to a member of a room they share, on that room's current connection, while the asker is on its roster, and only if **R is its own home relay** (its own signed record) **or it is connected through R, right now, to another member of that room** (`Connection::paths`). A member who claims a home relay it isn't on gets nothing for it: asking can't fish for relays a room doesn't use.
- **What may be passed on:** a relay the user registered themselves (pasted, or the build's own), unless personal or its switch "Pass this relay on to my rooms" is off; and a relay received for a room, **within that room only**. So a code spreads through the rooms of whoever registered it, never on from one room to another by itself.
- **Checked before kept:** a received code counts only for the address asked for, and is kept only after connecting with it. One the relay refuses is dropped; one for a relay that doesn't answer now is kept as unchecked and tried again later, never dropped and asked for in a loop. Two answers with different codes (one from before a rotation) are each tried, and the one that works is kept. It's kept as received, with where it came from ("from Bia, for Friday games"); removing it in Settings stops asking for it again.
- **Leaving a room drops the relays received only for it**, at once.
- **The answerer counts too:** a member answers each other member at most a few asks a minute, whatever they ask, so asking can't be used to load it.
- **Settings → Network → "Use relays my rooms pass on"**, on by default: off, nothing is asked for or kept, for someone who'd rather not connect to relays they didn't choose (and the reasons above then say "ask for its code" as before).
- **Rotation travels too:** a member whose code is refused asks again; whoever has the new one passes it on. So changing a relay's code no longer cuts anyone off who is still in a room on it: cutting someone off is a personal code taken back, plus a ban.
- **Joining:** an invite carries the owner's home relay's code when the owner may pass it on (the author's choice: by default). The newcomer uses it for the join alone, and keeps it (received for that room) only once admitted. **This needs a relay added while running** (`Endpoint::insert_relay`): M26a's test of it decides whether M26b can be built this way; if it fails, M26b's plan finds another way before anything else (a restart in the middle of a join is not acceptable). The invite gets a new version, the invite page still reading the name at its fixed offset (`deploy/invite-page/check.js`). **Whoever has the link can use the relay**, password or not, until its code changes: the invite dialog says so, with a switch to leave the code out of that link.
- **A wire change:** the ask and the answer on the control stream, never relayed or stored by anyone but the asker, the token printing `..`; the code in the invite. A wire revision and a minor version; the `protocol-change` and `room-security-checks` skills.

**What it opens, and why it's acceptable:**
- **anyone admitted to a room can use the relays its members are on,** as anyone they handed the code to could before. A relay's owner who wants to choose who uses it gives personal codes;
- **a member can make us hold a relay of its choosing** (its own home), which may then become our home relay (iroh can't keep a relay out of that choice): its operator sees our address and when we're online, and can drop what's relayed through it, never read it (end-to-end encrypted). Bounded by the cap, the 30 days, Settings showing every received relay and where it came from, and removing it.

### Showing what's wrong, and how to fix it

Public builds reach people with no maintainer to ask, and relays add ways for a room to stop working. Whatever goes wrong, the app says **what happened, on whose side, and the one thing to do**, in plain words, with the technical text (iroh's error, never a token) behind Details. Rules for all of it:
- **lasting trouble only:** shown after about 10 s, gone as soon as it clears, so a blip doesn't alarm;
- **in reserved space,** below what's in use (nothing jumps), each state a preview scenario and in `npm run shots`;
- **a hint, not a verdict,** where a direct path may still connect: nothing is refused for it.

The hints need no wire change of their own: a member's home relay is in its signed record (signed again when its address changes), the owner's in the invite, and iroh gives the rest.

**Our own connection** (Settings → Network, and the shell when it matters):
- **each registered relay's state:** connected (the one others reach us through: home), connected, idle (not needed now), unreachable (iroh's error under Details: no such name, a certificate gone bad, no answer), or **refused our code** (`RelayStatus::auth_denied_reason`: a rotated or revoked token, which iroh retries in vain, so it's said at once: "relay.example.com no longer accepts your code: ask its owner for a new one", with Paste a new code on that relay);
- **this network,** from iroh's net report: not online at all ("You're not connected to the internet"), behind a sign-in page (its captive-portal check: "This network wants you to sign in first"), or no UDP ("This network blocks direct connections: everything goes through a relay, with more delay and a lower picture");
- **iroh's public relays have a row too,** with the same states, while they're in use;
- **a strip in the shell only for what silently breaks every room:** no relay reachable, a relay refusing our code, or no internet; one line, and the button that fixes it, read out by screen readers when it appears (a polite live region);
- **the notification area icon says it too,** in its picture and its tooltip ("Can't reach your relay"), since a PC whose window is closed to the tray is exactly one whose friends can't reach it without anyone seeing the strip;
- **Copy diagnostics:** the versions and build kind, each relay by address with its state and error, the net report, and each room's member links, never a token, password or invite, for asking someone for help; beside it **Open the log folder**, and in public builds **Report a problem**, opening a GitHub issue with a template that asks for the diagnostics.

**Rooms: why a member can't be reached.** A member offline, or online but not connected to us, gets the reason we can tell:
- **their relay is ours and failing:** that relay's state ("relay.example.com isn't answering");
- **their relay needs a code we lack:** while it's being asked for, "Getting relay.example.com's code from the room…"; when nobody we reach may pass it on (personal, switched off, or nobody connected holds it), "Bia is on relay.example.com, which needs a code nobody here can pass on: ask Bia for it", with Paste a code; and since either of a pair holding the other's relay is enough, that Bia getting ours would do too;
- **our own connection** (above), when that is the cause, rather than a reason per member;
- otherwise nothing: they may simply be away.

A room where nobody can be reached says why in one line at the top, when the members share a cause. In a session, a stream window whose stream stalls says why ("Reconnecting: relay.example.com stopped answering") and so does the host's console for that viewer, rather than a frozen picture alone; and a viewer reached only through a relay sees why its picture is limited (the stream window's stats and the console name the relay), so a lower picture is explained, not a mystery.

**Joining**, the one dial in one direction (the newcomer alone dials, at the relay the invite names: the owner's home when it was made):
- **before the password,** an invite that names a relay needing a code we lack, and carries none (left out by the owner, or personal), says so ("This room's owner uses relay.example.com: ask them for its code"), with Paste a code right there; joining anyway stays possible;
- **a join that fails says which step did:** the relay couldn't be reached (its state), the relay refused us, **the relay answered but the owner isn't on it** ("They may be offline, or the link may be old: ask for a new one"; told apart by connecting to the relay first), or, as today, the password, the link's expiry or a ban;
- **links that go stale are marked at the owner:** an invite names the owner's home relay; when it moves (rare: only for one much faster), the owner's app marks its open links as pointing at the old one, to make new ones.

**Changing relays never breaks a room silently:**
- **removing a relay,** or one refusing our code, lists the rooms and members on it first: "Without it you'll reach Bia and Caio in Friday games only over a direct path";
- **at a home PC run from afar** (ADR 0010; nobody is there to paste a new code): registering or removing a relay while remote play is on warns that its devices may be cut off; a device that can't reach its home PC says why when it can tell (the PC's relay unreachable or refusing us), and that it must be fixed at the PC. A relay's new code reaches an unattended PC by itself (M26b) as long as someone in its room has it, so the deploy guide's rotation keeps the old token beside the new for a while, until every PC has heard it.

**Versions** (M26b):
- **a member on another wire revision** says which side must update; when it's us, with a link to the release ("Bia is on 0.36: update to play together"). It needs no request (the member told us its version), so it shows with the update check off too;
- **an update that changes how members talk says so before it's installed:** the release notes name the wire revision (the workflow writes it), and the notice reads it: "Everyone in your rooms needs this version to play together: update together". An update that doesn't is just "X is out";
- **the invite page** offers the download to someone without the app (the one for their system first; on a Mac, that it isn't there yet), and says when the owner uses a relay of their own whose code the link doesn't carry: "ask them for its code too". It never shows or sends a code: it reads the link's fragment in the browser alone, as today.

**Public builds' own troubles** (M26b):
- **antivirus:** an unsigned app with a keyboard hook and input injection can be taken for malware. Each release is sent to Microsoft's file submission portal before it's published, and the release notes say what to do if a scanner still flags it;
- **SmartScreen and the checksum:** the release page shows the two clicks past SmartScreen with pictures, and how to check the SHA-256.

The room's relay in the owner's signed policy stays out (a wire revision for little: the members' records already say where each is).

## Consequences

- **Anyone can install the app** without a toolchain, and the maintainer's releases need no hand-built installers; friends move to public builds and register the author's relay with a code, or keep the private route.
- **Moving between rooms on different relays is seamless:** the codes a room needs reach its members by themselves (relays without a token, iroh's public ones among them, need none), and a link carries the owner's. What's left needs a personal code or a code left out of a link, or two people with no member between them; and when any of it fails, the app says which and what to do.
- **A relay's owner carries more than their friends:** whoever joins a room on it can use it. The deploy guide says how to watch its load (its metrics), and how to give personal codes instead.
- **A public build on iroh's public relays depends on their limits** until it holds a relay: the wizard asks when it matters, gently.
- **Every public release splits users by wire revision until they update;** the notice is the remedy, not a freeze.
- **SmartScreen warns harder on a new, unsigned publisher** than on friends' installers today.
- **Licences:** a public release is a distribution like friends' installers, with the same obligations (ADR 0009): notices in every package and on the release, the source at the tag, FFmpeg's source beside it, and for the AppImage the Ubuntu source packages it names. CI builds them the same way, so the checks in `xtask` keep guarding it.
- **The app now contacts GitHub** in builds with a releases address, unless turned off.
- **The outside review of admission (M15)** matters more once strangers use public builds; still optional.

## In parts (for M26's plan)

- **M26a, relays you register:** the store (registered and received relays, sealed, capped, pruned), `RelayChoice` with a token per relay, the code (prefix, checksum, personal) and its check, Settings → Network with each relay's state and the network's, the shell's strip, Copy diagnostics and the log folder, the wizard when it matters, the rooms', sessions' and join screen's reasons, the warnings before a relay goes, the build's own relay kept in the profile, today's setting carried over, the release rules for others' addresses (records included), the tray icon's state, backups, the deploy guide's personal codes, rotation and load. Its first tests, in `couch-engine`'s `tests/relay.rs`: the check above made lasting (two relays with their own tokens, rooms across them, the join that can't reach an owner on a relay the newcomer lacks), a member reached at an open relay outside our list, a relay refusing our code reported as such, and adding and removing a relay while running. A minor version; no wire change.
- **M26b, relay codes passed on by themselves:** the ask and the answer, the path rule, received relays per room, the code in invites and the invite dialog's switch, rotation passed on. Its tests: a member who can't reach another gets the code through a third and connects; a claimed home relay nobody is on gets nothing; a received code never leaves its room; a personal code never leaves its holder; a join with the code in the link, kept only once admitted. A wire revision and a minor version.
- **M26c, public releases:** the public build kind, the CI release workflow on a tag, the update notice and its switch, the version mismatch's link to the release, the invite page's download and relay note, Report a problem, the antivirus submission, the README and docs (`self-hosting.md` rewritten as "run a relay, add it in your app, your rooms get it", `development.md`, the `release-installer` skill).

## Limits, for later

- **iroh can't keep a relay from becoming our home relay**, nor keep us listening on more than one. Either would close the last gaps (a received relay as our home; a newcomer whose link names a relay we moved from). Worth asking upstream.
