# P13 results: one controller moves at a time

## Why

Your run of 0.18.0 (2026-10-27) turned up two problems:
- **Dragging moved too much.** With a guest's two controllers each on a seat, dragging one of them to a third seat also took the other off its seat.
- **Asking moved too much.** A guest on two seats asking for a third lost one seat too many. You expected only one controller to move.

Both came from M18's rule. A guest on as many seats as controllers, given one more, "moved": they lost *every* other seat with a controller. You also asked for a clearer way, for guests, to see which controller a request adds or moves. Your choice: whoever moves a controller always says which ("always choose").

A wire change: **revision 8, version 0.19.0**, which doesn't talk to 0.18.0 or earlier. The 0.18.0 installer was built but not handed out, so the next one for your friends is 0.19.0.

## What changed

- **Only the moving controller leaves its seat** (`HostSession::frees`).
  - A grant beyond a guest's controllers first frees only as many of their seats as it must, normally one: the seat of the controller that moves, as their side last said (`set_drivers`, from `ViewerControllers`).
  - The keyboard-and-mouse seat stays, as before.
  - The revokes still go first, on the same ordered channel, so the router never sees a guest on more seats than they have controllers.
- **Whoever moves it says which:**
  - **The guest asks with a controller** (`ControlRequest.controller`). With two controllers or more:
    - in the stream window, clicking a seat (or pressing the ask shortcut) puts a choice below the seats: "Which controller plays Controller 1?", each controller with the seat it leaves ("leaves Controller 2"), and Cancel;
    - in the app, the seat's tile opens the same choice as a menu, and the card's right-click menu has "Ask for Controller 3 with".

    The choice shows only when a controller must leave its seat: a seat with a controller, and each of the guest's controllers playing one already. Otherwise (one controller, a spare one, the keyboard seat) asking works as before.
  - **The host drags one of the guest's controllers** (P12's plugs). Dragging the guest themselves still works while nothing has to be freed, or only one seat could be. When a seat must be freed and nothing says which, the drop is refused with "Drag one of Bia's controllers", and the menus offer only their controllers. The engine refuses such a pass too (`SessionError::ChooseController`).
  - An accepted request without a controller (one that became ambiguous after it was made) frees from the last seat down, rather than failing.
- **What everyone sees:**
  - The host's request line names the controller and what it leaves: "Bia asks for Controller 1", then "With their Xbox Wireless Controller · leaves Controller 3".
  - Hints say "Leaves Controller 3" where they said "Moves them".
  - The guest's note after a grant names the controller: "Your Xbox Wireless Controller now plays Controller 1".
- The preview has new scenarios: `hosting-guest-asks`, `playing-two` with our controllers, and the shot `ask-with`. The stream window has the new preview state `choosing`.

## Your run of 0.19.0 (2026-10-27) and 0.19.1

It worked, but with one gap. A guest had two controllers, only one of them on a seat. When you moved that controller to another seat, the guest ended up on both seats: the session added the new one because a controller was spare, and the spare took the old seat. You expected only the move.

0.19.1 fixes it: **a named controller that plays one of the guest's seats always moves**, freeing its seat whether or not another is spare (`HostSession::frees`, and the app's copy of it). A free controller named, or none named, still adds the seat while one is spare. Only the host's session changed, not the wire: 0.19.1 still talks to 0.19.0.
- New unit test `a_seated_controller_moves_even_with_one_to_spare`.
- P12's scenario now moves the Xbox pad to seat 2, freeing seat 1, where it had added and swapped. Then the spare DualSense is added to seat 1.
- The drag hints say "Leaves Controller 3" where they said "Added · Controller 3 takes another of theirs".
- The input-safety checks pass again: `release-check`, `input-sink`, `lifecycle-check`.

## Your P10 run

On the PC that froze before, Windows took a few seconds and showed its "installing device" notification, with no freeze and no warning in the app. That is as designed (see `p10-results.md`).

## Checked

- **couch-room:**
  - `only_the_moving_controllers_seat_is_freed`: the host's pass with a controller frees only its seat, revokes before the grant, and names the controller; without one, or with one that plays no seat, it's refused; a request's controller frees only its seat when accepted;
  - `one_seat_moves_without_a_choice`;
  - the property test, with new ops (a pass and a request naming a controller, the guest saying which drives what), still never sees a guest on more seats than controllers, and still reaches a guest on two seats (a temporary assertion against it failed, as it should).
- **The session scenario `only_the_controller_that_moves_leaves_its_seat`:**
  - a guest on two of three seats, passed the third without a controller: refused;
  - passed it with their Xbox: only its seat goes, and the DualSense plays on;
  - then asking for the freed seat with the DualSense: only its seat goes, and the pads follow.

  The P12 and M18 scenarios pass unchanged.
- **Front-end tests:** `frees` and the drop hints (only the moving controller's seat, the refusal, the request's detail).
- **`npm run shots`:** the new scenarios and shots. **`viewer-shots.ps1`:** the stream window's `choosing` state.
- **The input-safety checks,** all at their usual timings, before and after the review's fixes: `release-check`, `input-sink`, `lifecycle-check`.

## The review

A fresh review agent found no safety-rule violations. It checked three things:
- seats are freed only from the guest's own, held or on their way, always aimed at them, before the grant;
- a lying or stale report of which controller drives what can only change which of the guest's own seats goes;
- the choice in the stream window sits inside the bar, so its clicks never reach the game.

Fixed:
- **A grant on its way, freed by a later one, could free one seat too many when reported.** For example: a seat passed and not yet taken, then a request accepted that freed it. The router's late report of the first grant matched nothing, and the fallback freed another of the guest's seats. Such a grant now stays pending, marked cancelled, so its report frees nothing (`a_freed_grant_on_its_way_frees_nothing_when_reported`).
- **The stream window offered the choice for the keyboard-and-mouse seat, and when nothing had to move.** It now shows only for a seat with a controller while each of the guest's controllers plays one already. Otherwise a spare controller takes it, as before. The app's tile menu does the same. The choice also closes when the mouse is captured for the game, so it can't hold the bar up out of reach.
- **The host took any controller number in a request.** It now takes only one the guest said it has.
- **A request's controller could change after the host had seen it,** by asking again for the same seat. That would slip a new controller under the host's Accept. A request now stays as first asked; to change it, withdraw it.
- **A request without a controller showed nothing,** though accepting it frees a seat by guess. The host now sees which ("They leave Controller 3").
- Old stored choices are cleared on withdraw, the panic key and the end. Stale doc comments are fixed.
