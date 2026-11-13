# M26a results, part 1: relays you register

## Why

Until now each release build carried its builder's relay and token, and a profile used one relay, chosen in Settings and applied at the next start. Friends whose rooms were on different relays couldn't be in both, and a public build (M26c) would have nobody's relay at all. [ADR 0014](adr/0014-public-builds-and-registered-relays.md) settled the way out: a profile holds several relays at once, each with its own token, added by a relay code; and the app says what went wrong and what to do.

**Your choices (2026-11-13):**
- M26a in two PRs: this one, relays you register and our own connection's state (0.35.0), then the reasons: why a member or a join fails, a session's stall (M26a-2, 0.36.0);
- iroh vendored and patched, so relays change while running (below);
- n0's DNS address lookup dropped in every build: iroh's public relays are entries of its default relay map;
- the public relays standing in while none of ours works: a setting, off by default;
- diagnostics anonymous: relay names, states and errors, versions, the network; rooms and members numbered; no names, no IP addresses.

Version **0.35.0**, no wire change: it talks to 0.34. Backups are format 3; format 2 and 1 still open.

## What we found

**iroh 1.3 keeps a token per relay** (each entry of its relay map has its own), so several relays at once needed nothing from iroh. Changing them while running did: a relay's connection reads its token once, when it starts. Ten tests in `couch-engine`'s `tests/relay.rs` (mod `registered`) showed:
- a relay added before its first dial, a relay removed, a token per relay, a relay open to everyone outside our list, and a refused code reported by the home relay's status all work unpatched;
- but a relay added after a refused dial, a new code for a refusing relay (the home relay's above all), and a removed relay kept the old token until 60 s idle, the home relay for good.

iroh is vendored (`third_party/iroh/`, `PATCHED.md`) with about 50 lines that restart a relay's connection when its map entry changes; the same three tests fail without it.

**iroh reports on the home relay only.** The others' states come from its net report (which relays answered its probes), from the relays our connections run through now, and from checking each code now and then with a throwaway endpoint of a random key: our own key twice on one relay would have the relay replace our real connection.

**iroh picks its home relay by speed and keeps one that refuses us.** Found by a test: with two relays, home landed on the one refusing our code, leaving us reachable nowhere while the other worked. A refusing relay is now benched out of the endpoint's list until a check lets us in or its code changes.

**Records don't follow a member to a relay nobody else holds.** A member whose only relay changes to one no other member holds has no connection left to send its new record on, and the others keep dialing its old relay. This part warns before removing a relay others reach us through; M26a-2's reasons and M26b's codes must account for it.

## What changed

- **Relays in the profile** (`couch_engine::relays`): each an address and its token (or none, for a relay open to everyone), sealed in `relays.sealed` like the room files, at most 16. The build's own relay is written in at start, first, and can't be removed; a newer build's token replaces only the build's, never a code the user pasted for it. A `network.json` from before is carried over once (a custom address becomes a relay without a token, `public` keeps the public relays on) and left as it was, so the version before still reads it.
- **All of them at once, changed while running** (`couch_net::endpoint::RelayChoice::Relays`, `change_relays`): one map entry each, its token sent to it alone. No restart, ever.
- **Relay codes** (`relays::code`): `couch-relay-1.<base64url of address, token, flags>.<checksum>`, checked whole (a code cut short when copied is caught and said), optionally personal. `deploy/relay/relay-code.sh` makes the same codes in plain bash (a test runs it where bash exists).
- **Checked before saved** (`couch_net::endpoint::check_relay`): adding a relay connects to it with its code first; refused, nothing is saved and the app says to ask its owner for a new one; no answer, "Save anyway".
- **Share its code** (`Command::ShareRelay`): copied natively, never through the webview, kept out of Windows' clipboard history and cloud sync (`couch_platform_win::clipboard`) and KDE's history (`couch_platform_linux::clipboard`, through `arboard`); never a personal code.
- **Our own connection** (`couch_engine::network`): each relay's state (connected and home, connected, answers, not answering, refusing our code), the public relays as one row, the network's (offline, a sign-in page, no UDP), and the trouble that silently breaks every room (no internet, a sign-in page, a relay refusing our code, none of ours answering), shown only once it has lasted 10 s and gone at once.
- **The public relays** (iroh's default relay map, never its DNS lookup): in use while nothing is registered or when kept on, standing in while none of ours works if the user allows it (off by default).
- **Others' relay addresses** (a member's record, an invite) are dialed in release builds only as https on port 443 to a dotted name (`couch_net::addr::DialRules`): never an IP literal, `localhost`, a single-label name, a LAN suffix or credentials. Before, a member could aim others' apps at any address and port in their own network; now only a public-looking name on 443, and one resolving to a private address isn't caught (iroh resolves it). An invite naming only such a relay is refused with why.
- **Backups carry the relays** (format 3, sealed with the passphrase); a restore writes them and freezes relay changes until the restart.
- **Settings → Network**, rewritten: each relay with its state and Details; adding by code (what it names shown first) or by address and token; Share; Remove…, which names the rooms and members reached through it (`Command::RelayUse`) and warns while remote play is on; the public relays' two switches; Check again; Copy diagnostics (`couch_engine::diagnostics`) and Open the log folder (`Backend::open_folder`: Explorer, or the desktop's OpenURI portal on Linux, so an AppImage's environment never reaches the file manager).
- **The strip** at the bottom of the main area (`shell/TroubleStrip.tsx`): one line and the button that fixes it (Check again, Paste a new code, Network settings), in a polite live region; nothing above it moves.
- **The tray** (Windows): its tooltip says the trouble, and an amber dot is drawn on its icon at runtime (no new icon files).
- **The wizard** (`settings/RelayWizard.tsx`): on creating a room or an invite while only the public relays are in use; Paste a relay code, the guide to running one, Remind me later (a week, or the next version, `Command::DeferRelay`, saved with the relays), Stay on the public relays. Whatever is chosen, what the user was doing goes on.
- **The sidebar** says "via the public relays" rather than an iroh server's name.
- **The deploy guide** (`deploy/relay/README.md`): relay codes, personal codes as one token per friend and taking one back, rotation keeping the old token beside the new until every PC (a remote-play PC above all) has the new code, what the app shows while the relay is down, watching its load. `docs/self-hosting.md` follows the new Settings.

## Checked

| What | How | Result |
|---|---|---|
| Lint, format | `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, Windows and Linux (WSL) | clean |
| Workspace tests, Windows | `cargo test --workspace` | pass (couch-engine: 163 unit, rooms 28, sessions 98, relay 12) |
| Linux (WSL) | clippy, fmt, couch-engine and couch-net tests | pass |
| Rooms, sessions, relay, three times (room-security-checks) | `cargo test -p couch-engine --test rooms`, `--test sessions`, `--test relay` | pass, 3 of 3 each; no flaky pass |
| Relays changed while running | relay `registered` (10 tests: a token per relay, an open relay outside the list, a refused code, added before and after a dial, a new code for a refusing relay, the home relay removed, a removed relay gets no token) | pass; the three listed above fail unpatched |
| Codes | code tests (round trip, the deploy kit's vector and script, cut or changed codes, a token never prints) | pass |
| Our connection | network unit tests (each relay's state, the public row, benching, a refusal from before a new code, the fallback, code checks off the home relay, the wizard's timing); relay `the_network_view_tells_each_relay_s_state` (home, a second relay refused and benched, ours home, the trouble, the right code clearing it) | pass |
| Who uses a relay | rooms `a_room_follows_relays_registered_while_running` (Bia listed for the relay, nobody for another) | pass |
| Diagnostics | `nothing_names_anyone`, `addresses_are_taken_out` | pass |
| Dial rules, backups | couch-net `a_release_build_dials_others_relays_only_as_https_to_a_name`; store `a_backup_carries_the_relays`, `relays_carry_over_once_and_stay_sealed` | pass |
| The UI | `npx tsc --noEmit`, `npx vitest run` (107, `model/network.test.ts` new), `npm run shots`: 9 new scenarios (`network-*`), 25 new shots (Settings in each state, codes read, refused, not answering, the menus, removal, the strip's fix, the wizard), two new no-jump views ("network add", "trouble strip"); dark, light and High Contrast at three sizes | no problems, nothing jumps; looked at by eye |
| Docs | `bash scripts/check-docs.sh` | nothing stale |

## Found while checking

- **The address form was taller than the code form,** so switching moved Help down: both forms now lie in one place, the hidden one hidden (found by the no-jump check).
- **Both forms showed the check's outcome** (a test found the text twice): only the form shown says it.
- **`folder.rs` didn't build on Linux** (a borrow of a temporary): found by the WSL run.
- **The preview's fake engine dropped a public home relay,** so the sidebar said "connecting…" behind the wizard; and an iroh server's name meant nothing there anyway, hence "via the public relays".
- **A 0.34.2 test raced** (`a_host_that_cant_draw_over_the_game_draws_in_its_own_window`: B drew before hearing drawing was on), 1 in 10 here: it now waits like the others (0 in 15).
- **`git worktree remove --force` deleted `third_party\ffmpeg` and `sdl3`** through junctions into them (told at the time); restored with `cargo xtask fetch-deps` and `stage-dlls`.

## The review

A fresh review agent read the whole branch (steps 1 to 8) against CLAUDE.md, the ADR and this page, and tried to break it. **Nothing serious:** no token reaches a log, a reply or event, a record, an invite, remote play or the diagnostics; each token goes to its own relay alone; the code parser can't panic; backups of format 1 and 2 still open and a failed restore is undone; no lock is held across a wait where it mustn't be; the Windows clipboard code frees and hands over its memory correctly. Its findings, each checked against the code; all held, and all were fixed but the last three:

1. **A check's answer about an old code was kept** (medium): a check started before the user pasted a new code could come back after it, bench a relay whose new code works (the strip back for two minutes), or hide a wrong one for ten; and the check under way held up one with the new code. Each answer now carries the code it checked and counts only if that code is still held (`Network::checked_with`, the add's own answer too), and a new code starts its own check at once. Test `an_answer_about_an_old_code_is_dropped`, and the scheduling test's new cases.
2. **The dial rule claimed more than it did** (medium): names with a LAN's suffix (`nas.local`, `router.lan`, `x.home.arpa`) and any port passed. Now refused, with others' relays on port 443 only (decimal and hex IP forms checked too). A public name resolving to a private address still isn't caught (iroh resolves it), and the docs (CLAUDE.md, protocol.md, the ADR, this page) now say "narrows" rather than "closes".
3. **A relay without a token that refused us stayed set aside for the run** (low-medium): only relays with a token were checked again. Any relay that refused is now.
4. **The diagnostics missed addresses after a word's colon or in brackets** (`host:10.0.0.5`, `[192.168.1.20]:3340`): now taken out, with tests.
5. **"Save anyway" in the wizard** turned the public relays off at once for a relay that doesn't answer, right before an invite that needs one: the wizard no longer offers it (Settings still does).
6. **Typing a personal relay's address again cleared its mark**: only a code decides it now; a typed address keeps it (engine test).
7. **A crafted backup's relays were half-checked**: now checked as typed ones (no credentials) and each kept once; the log names a relay's server, never its whole address.
8. **Our own relays reached the dial rules only after the room actor started**, so a first dial could strip one: filled before.
9. **"The webview never sees a token"**: true of every reply and event, but a script could read the clipboard after Share, and a pasted code passes through a field. The docs say so now.
10. **Smaller:** Explorer is started by its full path (a bare name is looked for beside our exe first); on Linux the log folder is opened by `desktop.log` or the folder itself, never another file of the profile's; the strip's dead "standing in" text is gone (while the public relays stand in nothing is wrong: no strip, Settings marks them), with a scenario of its own; a misplaced doc comment. **Left as they are:** the strip takes its line from the bottom of the room's area when it appears (nothing above it moves, which is the rule); the remote-play note in Add a relay appears a moment after the section on a home PC with remote play on; and our iroh patch's message uses iroh's own `try_send` (dropped only on a full channel of 256, as iroh's other relay messages).

After the fixes: clippy and fmt clean on Windows and Linux; couch-engine's unit tests (163) on both; couch-net's; rooms, sessions and relay; the front end's checks and the network and wizard shots.

## Still to come

- **By hand:** Share on Windows (Win+V's history must not list the code) and on KDE and GNOME; Open the log folder on both; the tray's dot and tooltip; the public relays standing in, with the internet; Settings with a real relay and a code made by `relay-code.sh` on it.
- **M26a-2:** why a member can't be reached, which step of a join failed, links gone stale at the owner, a session's stall and a relayed picture explained (the stream window's IPC).
- **M26b:** codes passed on by the rooms, and in invites (a wire revision). **M26c:** public releases.
- **The iroh upstream issue** for the token restart, parked by you until it's explained.
