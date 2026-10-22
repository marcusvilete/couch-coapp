// The seats an input setup makes, labelled as the engine labels them
// (`couch_proto::session::seat_labels`): "Controller" (numbered when there
// are several), "Keyboard and mouse", "Controller, keyboard and mouse".
// The start dialog previews them; the preview's fake engine makes them.
import type { InputSetup } from "../bindings/InputSetup";
import type { SeatView } from "../bindings/SeatView";

export function seatsFor(input: InputSetup): SeatView[] {
  const kinds: ("pad" | "both" | "kbm")[] = [];
  for (let i = 0; i < input.controllers; i++) {
    kinds.push(i === 0 && input.keyboardMouse === "withController1" ? "both" : "pad");
  }
  if (input.keyboardMouse === "ownSeat") kinds.push("kbm");
  const pads = kinds.filter((k) => k !== "kbm").length;
  let pad = 0;
  return kinds.map((kind) => ({
    label:
      kind === "kbm"
        ? "Keyboard and mouse"
        : kind === "both"
          ? "Controller, keyboard and mouse"
          : pads > 1
            ? `Controller ${++pad}`
            : "Controller",
    pad: kind !== "kbm",
    keyboardMouse: kind !== "pad",
    holder: null,
    host: false,
    hostController: null,
  }));
}
