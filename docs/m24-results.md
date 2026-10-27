# M24 results: sessions to watch together

## Why

- **Your friends' run of 0.17.1** asked for a session just to show something or watch together, with nobody playing. Until now a session had at least one seat, so it always plugged a virtual controller, could hide the host's controller, and took the host's keys for the panic key.
- **Your own observation (2026-10-27):** keyboard and mouse "With Controller 1" is redundant since M18. That option gave one seat with a controller, keyboard and mouse together; since M18 one guest can hold Controller 1 and the keyboard seat at once. You agreed to drop it here, since both changes touch the wire.

A wire change: **revision 9, version 0.20.0**, which doesn't talk to 0.19.x or earlier. Neither 0.18.0 nor 0.19.x was handed out, so the next installer for your friends is 0.20.0.

## What changed

- **No seats is a choice:** 0 controllers and keyboard and mouse "Not for guests".
  - The start dialog used to refuse it ("Give the guests at least one seat"). It now says "Just watching: guests see and hear, nobody plays."
  - In such a session:
    - no virtual controller is plugged in (no Steam notification, no ViGEmBus needed);
    - none of the host's controllers is hidden: they play the game directly;
    - no hook takes the host's keys, so there is no panic key and no answer keys, since there is nothing to take back or answer.
  - The stream, sound, viewers, stream switching and ending work as in any session.
- **What everyone sees:**
  - **The host's console:** a "Watching together" panel lists who watches, in place of the requests strip and the patch bay. The session bar says "Watching together" where it shows the panic key.
  - **A guest's session card:** one tile, "No seats · Watching together". The card's menu has no ask entries.
  - **The stream window's bar:**
    - no seat chips;
    - "Watching together: nobody plays" as its status line;
    - no "ask or give back" key in its hints;
    - the ask shortcut does nothing;
    - between streams, its centre card no longer says "Your seat stays yours".
- **"With Controller 1" is gone.** Keyboard and mouse are either "Not for guests" or "A seat of their own". A setup saved with the old choice comes back as a seat of their own.
  - On the wire, `SeatKind::Both` is gone.
  - In the engine, `KeyboardMouseSeat::WithController1` is gone.
  - The patch bay no longer has its junction, where two of the host's cables met at one seat.
- **Underneath:**
  - `valid_seats` takes 0 to 5 seats.
  - `HostSession::new` takes none. Every request, pass and Play here then finds no seat (`NoSuchSeat`, or `HostNotOnASeat`).
  - The input task still runs, with an empty router, so a session with no seats ends like any other.

## Checked

- **couch-proto:**
  - an announcement with no seats is valid;
  - two keyboard seats are not;
  - revision 9's row is in `REVISIONS`.
- **couch-room:** `a_session_to_watch_together_has_nothing_to_hand_out`:
  - the host's controllers get no seat;
  - a request (with or without a controller) leaves nothing pending;
  - a pass and Play here are refused;
  - the status and announcement are valid and empty.

  The property test passes unchanged.
- **Session scenarios:**
  - **the new `a_session_to_watch_together_seats_nobody`:**
    - no pad is plugged;
    - the host's DualSense isn't hidden;
    - no hook is set up;
    - two guests watch and see no seats;
    - a guest's request and a host's pass reach nobody;
    - a guest's pad goes nowhere;
    - ending ends watching for both.
  - **Reworked from "with Controller 1":**
    - `between_streams_a_guests_input_is_released_and_held_off` now gives the guest the controller and the keyboard seat;
    - `the_input_setup_is_checked_and_makes_the_seats` checks the seats a controller pair and a keyboard seat make.

  All 67 pass.
- **The stream window:**
  - a HUD test: no chips and no seat to ask for, with no seats;
  - `viewer-shots.ps1`'s new `together` state.
- **Front end:**
  - vitest: the seats a setup makes, none to watch together, and a saved "withController1" coming back as `ownSeat`;
  - `npx tsc --noEmit`.
- **`npm run shots`:**
  - the scenarios `hosting-watch-only`, `hosting-watch-only-empty`, `watching-together` and `together-not-watching`, and the shot `dialog-start-together`;
  - the start dialog's no-jump check, now down to no seats.
- **The input-safety checks,** with times in their usual ranges:
  - `release-check`: 266 / 82 / 176 / 347 / 0 ms;
  - `input-sink`: everything arrived in order, and nothing blocked got through;
  - `lifecycle-check`: all six passed.

## The review

A fresh review agent found no safety problems. It checked these and found them fine:
- the seat router with no seats: every lookup goes through `seats.get`, and nothing indexes seat 0;
- the host pipeline: no pad, no hiding guard, and empty per-seat lists;
- the session machine;
- a session without the hook: `RevokeViewers` on an empty router does nothing, and with no requests there is nothing for the keys to answer;
- the guest's side: the request is refused locally, and nothing is forwarded;
- that nothing else uses `Both`.

Fixed:
- **The members panel offered "Pass a seat" with an empty submenu** for a viewer of a session with no seats. That viewer now gets no seat entries.
- **The preview's fake engine still refused a session with no seats.**
- **Wording that didn't fit a session with no seats:**
  - the start dialog's description;
  - the callout for a program running as administrator, which talks of guests' keyboard and the panic key: it is now shown only with seats.
- **The host's list of viewers could grow a row as people joined,** moving the panels below it. It now keeps two rows' height and scrolls.
- **The stream window's ask shortcut did nothing, silently.** It now notes "Nobody plays in this session: it's for watching together".
- **Dead code:** the patch bay's "+ keyboard and mouse" for the host on a seat with both.

Your local run: in the guest's card, "Watching together" overflowed its tile. The bold line sat outside the span that cuts text with "…", in a tile 118 px wide. The tile now spans the whole strip and reads "No seats · Watching together: nobody plays".

**Worth knowing:** a setup saved with "With Controller 1" comes back as keyboard and mouse on a seat of their own. That seat can then go to a guest other than the one on Controller 1. It is still yours to hand out, and the start dialog shows its danger callout.

## Still to try

- On this PC: start a session with 0 controllers and keyboard and mouse "Not for guests". A profile watches it. Check:
  - no Steam notification of a pad;
  - your controller still plays the game itself;
  - the panic key does nothing;
  - the guest sees no seats on the card or the bar.
- With friends, on 0.20.0: a movie or a game shown to the room, nobody playing.
