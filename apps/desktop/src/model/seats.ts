// The seats an input setup makes, labelled as the engine labels them
// (`couch_proto::session::seat_labels`): "Controller" (numbered when there
// are several), then "Keyboard and mouse"; none for a session to watch
// together (M24). The start dialog previews them; the preview's fake engine
// makes them.
import type { InputSetup } from "../bindings/InputSetup";
import type { SeatView } from "../bindings/SeatView";

export function seatsFor(input: InputSetup): SeatView[] {
  const kinds: ("pad" | "kbm")[] = Array.from({ length: input.controllers }, () => "pad");
  if (input.keyboardMouse === "ownSeat") kinds.push("kbm");
  const pads = input.controllers;
  let pad = 0;
  return kinds.map((kind) => ({
    label: kind === "kbm" ? "Keyboard and mouse" : pads > 1 ? `Controller ${++pad}` : "Controller",
    pad: kind === "pad",
    keyboardMouse: kind === "kbm",
    holder: null,
    host: false,
    hostController: null,
    guestController: null,
  }));
}
