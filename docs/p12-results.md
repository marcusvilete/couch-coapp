# P12 results: a guest's controllers in the patch bay

## Why

Your friends' 0.17.1 run (2026-10-26): a guest with several controllers played several seats (M18), and it worked, but it was confusing. The host's patch bay showed that guest as one player, so nobody could see which controller drove which seat. You chose that the host should also be able to pick ("Option 1"): dragging one of a guest's controllers onto a seat puts *that* controller on it.

A wire change: **revision 7, version 0.18.0**, which doesn't talk to 0.17.x or earlier. Everyone needs the new installer.

## What changed

- **In the patch bay,** each guest's controllers sit under their name, a plug each, named as their system names them and numbered when two are of one model ("DualSense Wireless Controller 1", "… 2"; the number stays when a long name is cut). A seat's cable runs from the controller driving it, and the seat names that controller where it said "guest". A guest who hasn't said yet, and the keyboard-and-mouse seat, still go to the guest's own plug.
- **Dragging a guest's controller onto a seat** puts that controller on it:
  - a seat they don't hold: added or a move, as before (M18), and "Hands it over" / "Added" / "Moves them" say which;
  - a seat they hold: only the controller changes, and "Swaps with Controller 3" says when another of their seats takes the one it had ("Only this one plays it" keeps their one seat to the controller on it). That works even while control is paused, since nothing changes hands.

  Dragging the guest themselves still lets their side pick, and a cable picked up at a seat drops as the controller at its other end.
- **The menus** do the same, for the keyboard. Pass a seat → a seat → "Whichever they pick up" or one of their controllers. A seat they hold: "Play Controller 3 with" → another of their controllers.
- **Underneath:**
  - The stream window names its controllers to the app (IPC `CONTROLLER_NAMES`, v13).
  - The guest's forwarder reports to the host its controllers, named, and which drives each seat (`ViewerControllers`, changed in place). The host checks the report and drops a malformed one whole: at most 8 controllers on distinct slots, at most 8 seats, each driven by one of those, names cut to 40 characters without control characters.
  - The host's choice goes with the grant (`SeatAssigned.controller`) or alone for a seat held (`SeatController`, optional).
  - The forwarder follows the choice while that controller is connected. That includes a seat on its own, which then no longer follows the controller touched last. A seat it drove takes the other's controller. A controller that isn't connected is no choice.
  - **The seat router is untouched:** no seat changes hands by any of this.
- `Command::PassControl` takes an optional `controller`. `SessionView.viewerPads` and `SeatView.guestController` carry what the app shows.

## Checked

- Forwarder tests:
  - a choice drives the seat, a seat on its own too;
  - a second seat chosen for the controller already playing swaps;
  - the host moves a held seat to another controller (and to a free one);
  - a controller not connected, or a seat not held, changes nothing;
  - a chosen controller that goes gives its seat back, as before;
  - the report names the controllers and their seats;
  - names wait for their controllers and go with them, and a seat granted again keeps its pick.
- couch-proto: the payloads roundtrip, the report's checks refuse each malformed shape, names are cleaned, and the fullest report fits a control frame.
- A session scenario, `the_host_picks_which_of_a_guests_controllers_plays`:
  - the host sees the guest's controllers by name;
  - passes a seat with the second controller, which then drives it alone;
  - passes another seat with the same controller, and the two swap;
  - moves a held seat to the other controller, and the fake game's pads follow each time.

  All 65 scenarios pass, the M18 ones unchanged.
- The patch bay's drop hints (vitest): names numbered, cables from the controller, added/moves, swaps, already here, paused, keyboard seat refused, a cable picked up.
- `npm run shots`:
  - the new scenario `hosting-guest-controllers`;
  - the shots `patch-guest-pad-drag` (held over a seat: "Swaps with Controller 3") and `patch-guest-pad-drop`;
  - a no-jump check ("guest controllers") that swaps by dragging and back, and finds neither the plugs nor the seats moving.
- The input-safety checks: `release-check`, `input-sink`, `lifecycle-check`.

## The review

A fresh review agent found no safety-rule violations. It checked three things:
- the seat router decides who plays as before;
- `SeatController` is followed only from our host, for a seat we hold;
- a report is taken only from the viewer, on the connection it joined on, and checked first.

Fixed:
- **The host's pick could go stale or be lost.** It was one slot for the latest pass. A pass that never became a grant left its pick for a later grant of that seat to that guest (a request accepted minutes later, say). Any other pass dropped a pick still on its way, and two quick drags on one seat granted it twice, the second without the pick. Now each pick is kept by guest and seat until its grant goes out with it. It is dropped when the seat is revoked from them or taken back, or when they go. A seat granted again while held keeps its controller on the guest's side.
- **A new controller first reached the host as "Controller",** then by name a moment later, in two messages. The stream window now sends the names just before the slots, and a name stays until its controller goes.
- A guest's one pad seat follows the controller touched last, so "Already here" refused to keep it to the one on it now. It now offers "Only this one plays it".
- "Added" now says when their other seat takes another controller ("Controller 3 takes another of theirs").
- A report that wasn't taken no longer refreshes the app's view. The held-seat branch allowed while paused now says so in a comment.

## Your run (0.19.0 and 0.19.1, 2026-10-27)

Dragging a guest's controllers works and "makes sense now". Moving one of two seated controllers took the other's seat too, and so did a guest asking for a third seat: P13 changed that. Moving a seated controller while another was spare added a seat: 0.19.1 fixed it (`p13-results.md`).

## Still to try

- With friends, on 0.19.1.
