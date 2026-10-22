# M21: what the old UI does, and where it goes

The checklist for M21's new UI. Every action, state, warning and remembered choice of the UI before M21 (0.16.0), with where it lives in the new one. The review (2026-10-21) checked each row against the new code; the one it found missing (a development build's restore note outside a room) was fixed ([results](m21-results.md#the-review)). "Same text" means the wording is kept (it may lose a sentence that moves behind **Why?**, never a warning).

## App frame

| Old | New | ✓ |
|---|---|---|
| Fatal screen "Couch Co-App couldn't start" with the engine's error | `shell/Fatal`, restyled | |
| Splash: two pads slide in, P1/P2 lights, couch, name typed; skip by key or click; at most 5 s; plays even with animations off | `shell/Splash`, same story and timing, new colours | |
| Notices (toasts): info and warn, 8 s, dismiss ×, `aria-live` | `ui/Toasts`, bottom right, pause on hover | |
| Window 960×640, minimum 720×480 | 1200×760 fitted to the work area, minimum 760×520, remembered per profile | |
| Events missed while away: rooms refreshed when the window gets the focus | kept (`state/lobby.ts`) | |
| Room views: the higher revision wins (`putRoom`, `refreshRooms`) | kept unchanged | |

## Rooms rail and order

| Old | New | ✓ |
|---|---|---|
| Rooms as round initials, tooltip with the name; selected marked | sidebar rows: tile, name, status line; icon rail below 900 px | |
| Drag to reorder (drop marker before/after); Alt+↑/↓ moves the focused room | kept, plus Move up/down in the room's context menu | |
| Order and selected room remembered (`roomOrder`, `selectedRoom` in web storage); new rooms last, by name | kept, same keys | |
| "+" (Create or join a room) → dialog with two big choices | sidebar "+" → same two choices | |
| Welcome (no rooms): "Play together, from your own couches", Create / Join | first-run empty state, same two choices | |

## Room column

| Old | New | ✓ |
|---|---|---|
| Room name as the column title | room header | |
| Owner: **Invite people** button | header's main button | |
| Owner, no password (restored or handed over): banner "This room has no password yet…" + **Set a new password** | callout + header's main button "Set a password" | |
| Catching up banner: owner "Until A and B have been online, nobody can be invited, let in or banned", member "until another member is online"; owner **Stop waiting…** | callout, same texts; Stop waiting for the owner only | |
| Handover, owner offering: "You offered the room to X. Waiting for them to accept…" + **Withdraw the offer** | callout | |
| Handover, offered: "X offers you the room: you'd let people in, ban, and close it." + **Accept…** / **Decline** | callout | |
| Handover, accepted: "You accepted X's offer… until then you can't leave it." | callout; Leave disabled with that reason | |
| Joins paused banner with a live countdown | callout with countdown | |
| Room menu "⋯": owner Invite people, Change password, Revoke all invites; Close room for everyone / Leave room | header "⋯" and the room's context menu; items disabled with reasons (catching up, accepted offer) | |
| Members list, "Members — N"; you first | members panel, Online / Offline groups | |
| Member row: avatar with link dot; tooltips direct / relayed / online not connected / offline / you | presence on the avatar, same tooltips | |
| Owner crown ♛ ("Owner: the one who lets people in"); playing 🎮 | crown, playing / watching / streaming icons | |
| LIVE / SESSION badge on a host ("X is streaming" / "X has a session; nothing is streamed now") | streaming icon with the same tooltip | |
| Version: "Update yours" / "Needs update", tooltips naming the other's version | same chips and tooltips | |
| **Make owner** for connected members when `canOffer` | member context menu and row "…" menu, same condition | |
| **Ban** for every other member (owner) | member context menu and row "…" menu | |
| Hint when alone: "Nobody else yet. Invite people to get started." / "Waiting for the others." | members panel empty line | |
| Your panel: avatar, name, "profile X" when not default, ⚙ Settings, ✎ name | your card in the sidebar, plus the relay in use | |

## Confirm dialogs (same texts)

| Old | ✓ |
|---|---|
| Revoke all invites? "Every invite link to “X” you've sent stops working. Members stay in the room." → notice "Invites to “X” revoked." | |
| Close “X”? (danger) "The room is gone for good, for everyone. Members who are offline find out when they come back." | |
| Leave “X”? (danger) "To come back you'll need a new invite from the owner." | |
| Ban X? (danger) "X is removed from “room” and can't come back, even with an invite. Every invite is revoked too, in case they hold one." | |
| Hand “X” over to Y? "If Y accepts, they own the room… and can't take it back." → Offer the room | |
| Stop waiting for the others? (danger) "Your room came back from a backup… Wait if they'll be back soon." | |
| Busy while running; an error shows in the dialog; Escape and the backdrop don't close it while busy | |

## Room dialogs

| Old | New | ✓ |
|---|---|---|
| Create: name (placeholder "Friday games"), password twice; checks: name, password, match, name length (96 bytes); hint on owner-only admission and password not saved | same fields and checks, hint shortened, Why? for the rest | |
| Join: invite link (textarea, mono), preview after 150 ms: name, "Works for about N more", expired, already member (→ **Open room**), own room; password; "Asking the owner…"; "The room's owner lets you in, so they need to be online." | same; password field always present, disabled until the invite checks out | |
| Join: "Another invite link was just opened." + **Use it instead**; a link taken by itself only while no password is typed and nothing is under way; the password clears when the invite changes | kept (`state/openedLink.ts`, tested) | |
| Opened link waits for other dialogs; notice "An invite link was opened. It will show when you close this window." | kept; Settings, the picker, a shortcut capture and the restart cover count as dialogs | |
| Invite: "Making a link…", link (mono, selects on focus), **Copy link** → "Copied" 2 s, copy failure text, hint on sending the password separately | same | |
| Change password: twice, match; hint "Members stay in the room…"; notice on change | same | |
| Accept the room: "Take over X?", password for newcomers twice; hint on what owning means; **Take over the room** | same | |
| Name: display name, 96 bytes; "Everyone in your rooms sees this name." | Settings → Profile, and ✎ on your card | |

## Stage

| Old | New | ✓ |
|---|---|---|
| Empty: "Nobody has a session yet" + explanation; **Start a session**; owner alone **Invite people**; blocked reasons "You have a session already, in another room." / "Stop watching first." | empty state, same buttons and reasons | |
| "Sessions in X" header + Start a session when we don't host | stage header | |
| Session card: host avatar, LIVE/SESSION, title, "X's session", who plays ("Controller 1: Ana · Controller 2: free", "Ana: own controller"), "N of M watching", "keyboard and mouse allowed", idle text by stream state and stop reason | fixed-height card: seat strip, counts, badges, same idle texts | |
| Watch (blocked: "End your session first." / "You're watching another session." / "Full: the host chose how many can watch.") → "Opening…" | same | |
| States: "Waiting for X…", "Lost the connection to X. Reconnecting…", "You're watching.", "X paused control for now.", "You're playing (on A and B).", "You asked X for Y.", "You're playing on A, and asked X for B." | same status line | |
| Ask for each seat not ours (disabled while paused, or the one asked) → "Asked for X" | seat tiles on the card | |
| Give back each seat (with several); Withdraw the request; Give it back / Give all back; Stop watching | same | |

## Host panel

| Old | New | ✓ |
|---|---|---|
| Head: badge, title, "N of M watching · keyboard and mouse allowed", **End session** → "Ending…" | console top bar; End session confirmed when anyone watches | |
| Banner: your controllers hidden for the whole session; start the game now; reconnect once; Steam Input | console banner, one line + Why? | |
| Banner: "Your controllers can't play through seats: …. You play on them directly, beside the seats." | console banner | |
| Control paused: "You took the controller back with the panic key…" + **Allow control** | console banner and the session bar | |
| Requests: "X asks for Y", Accept / Decline | request strip (reserved space), Accept waits 1 s, disabled while paused, keyboard warning on a keyboard-seat request; session bar shows the count everywhere | |
| Seats list (several seats, or our controllers on seats): "Your keyboard and mouse" / "Your own controller, keyboard and mouse · you, beside the seats"; each seat: label, holder ("you (DualSense)", name, free) | seats board tiles; "You · beside the seats" tile | |
| Seat actions: **Take back** / **Leave** (tooltips "It stays empty", "Your X stops playing it; it stays empty"); **Play here** (one controller); "Play here with…" (several: back on its seat / swaps seats with your X / leaves Y; "(Z loses it)") | tile actions and its context menu, same wording | |
| Hint on how games number players (with and without our controllers on seats) | Why? on the seats board | |
| Watching — N: "Nobody yet…"; each viewer: avatar, name, 🎮 seats, "N controllers"; one seat: **Pass control** / **Take back**; several: "Pass a seat…" / "Pass another seat…" with "(yours)", "(X's)", "— added" / "— moves them"; disabled "Allow control first" | Watching list, same; mirrored in the members panel's menu | |
| Footer: what you play ("You play the seat with your X…", "Your X plays no seat now: use Play here…", "You're playing.", "X is playing; your own controller, keyboard and mouse still work too.") and the panic key "<keys> takes the controller back from anyone at once, even from inside the game." / "The panic key couldn't be set up this time: use Take back." | session bar (keys or warning) + console summary line | |

## Stream (inside the host panel)

| Old | New | ✓ |
|---|---|---|
| Live: "Streaming X at N fps, up to M Mbit/s for each viewer." + **Stop streaming** → "Stopping…" | console top bar | |
| Starting: "Starting the stream…"; idle texts by stop reason; "Viewers stay, and keep their seats; their input waits for the stream, since they can't see." | top bar status + Why? | |
| Sources: windows (title, app, "admin"), screens ("Whole screen N (main)", W×H, Hz); refresh ⟳; "Looking for windows…"; "No windows found. Start the game, then refresh."; load errors | stream picker, Windows / Screens tabs, search | |
| "A window is best: only the game is shown, and only its sound is sent; keyboard and mouse need one." | picker hint | |
| Admin window warning (keys and panic key don't reach it; use Take back) | picker, and the console while it's streamed | |
| Screen with a keyboard seat: warning + refused at start | picker warning + refusal | |
| Frame rate 120/60/30 (remembered `couch.stream.fps`); quality 5/10/20/30/50 with descriptions (remembered `couch.stream.quality`) | segmented frame rate; quality select with descriptions; same keys | |
| Refresh too low warning (Hz + 1); 120 fps note; per-viewer upload × viewers; screen sound note | picker, same | |
| Limit my total upload + Mbit/s (2–10000, whole numbers) | picker Advanced, same check | |
| "Stream this instead" / "Start streaming" / "Starting…" | picker's main button | |
| Stream volume 0–400 % step 5, ×N above 100, sent at most every 100 ms, a waiting change sent on close, failure notice and revert; meter (average bar, peak line, stale after 1 s): silent / loud peaks rounded off / sending sound; long hint | console sound section, compact; hint behind Why? | |

## Start a session

| Old | New | ✓ |
|---|---|---|
| Hint: seats first, controllers hidden, then stream | one line | |
| Title (remembered `couch.session.title`, cut to 96 bytes; checks: empty, too long) | same | |
| How many can watch 1–7 (4 recommended); "Nobody else is / 1 other member is / N other members are in the room now." | same | |
| Controllers 0–4; keyboard and mouse: Not for guests / A seat of their own / With Controller 1 (disabled with 0); remembered `couch.session.setup` | seat builder, same choices and key | |
| "Give the guests at least one seat" | same | |
| Your controllers, one seat each: checkboxes, "you start on Controller N", full when every seat has one of yours; two of one model in order; remembered `couch.session.controllers` (and the older `couch.goLive.controller`), only when hiding is ready; absent ones kept | same rules and keys (`model/sessions`) | |
| Hiding states: not installed (Settings → Drivers), busy, ready (long explanation), none chosen | same states, explanation behind Why? | |
| Remapper: "(a remapper's virtual pad)", "Games still see X… Turn on the remapper's own hiding", "each press drives two seats" | same | |
| "A controller is plugged into this PC…" | same | |
| No ViGEmBus: guests' controllers won't reach the game | same | |
| Keyboard-and-mouse trust warning / "Guests play with their gamepad only." | next to the keyboard choice | |
| "Windows' volume mixer sets how loud the game is…" | Why? | |
| Start → "Starting…"; prepares the chime | same | |

## Settings

| Old | New | ✓ |
|---|---|---|
| Modal with tabs; Close; Escape blocked while capturing a key or restarting | full page with side navigation; same blocks | |
| Shortcuts: keys, action, scope ("In the stream window", "Anywhere, while you stream"), condition; Change (capture: chord or F-key, Esc cancels, modifiers ignored, refused reason); Default; Reset all; hint on when new keys apply | same | |
| Overlay: show requests and events; chime; texts on exclusive full screen and admin games | Overlay and sounds, same | |
| Network: built-in / iroh's public relays (for testing) / my relay (URL, LAN hint in test builds); "Everyone in a room should use the same choice"; connected through X; in use; Save; restart needed: dev "close it and start it again", release **Restart now** / "Restarting ends what you're streaming or watching." **Restart anyway**; polls every 2 s | same; polls only while the page is shown | |
| Drivers: ViGEmBus and HidHide, why, state (Installed, restart Windows, busy, Not installed), Install → "Installing…", "Its setup is open…", installer missing in this build | same | |
| Backup: passphrase twice (12+ characters after trim and NFKC), "Back up to a file…", "Saved to X."; safety text; restore: refused with rooms, passphrase, one-identity warning, refused while hosting, "Choose the backup and restore…"; dev: "Restored. Close the app and start it again."; release: restart | same; dev restore also shows an app-level callout | |
| About: version and commit or "a development build", GPL text, source link (commit unless dirty), FFmpeg source note, third-party licences | same | |
| Restarting cover: "Restarting Couch Co-App…" over everything | same, over the whole app | |

## Notices from engine events (same texts)

| Event | ✓ |
|---|---|
| Room removed: closed / left / banned / removed by the owner | |
| Joins paused: "Too many wrong passwords: nobody can join “X” for N minutes." | |
| Owner changed: "You now own “X”." / "Y now owns “X”." | |
| Room offered: "X offers you “Y”. Answer in the room." + taskbar flash | |
| Control requested: "X asks for Y. Answer in “room”." + taskbar flash + chime | |
| Session ended after an error | |
| Stream ended: game closed / source gone / error (the session goes on) | |
| Watch ended: ended / connection lost / unavailable / stream window failed | |

## Host overlay (behaviour unchanged)

| Old | ✓ |
|---|---|
| Requests (oldest marked, its keys "accept · decline", others "Waiting", "Answer in the app" without keys) and session notes 4.5 s | |
| Only while hosting, the app not in front, and Settings → Overlay on; on the monitor streamed, top right | |
| Never focused, unfocusable, click-through, excluded from capture, closed not hidden, off screen until drawn; taskbar flash stops when requests are answered | |
