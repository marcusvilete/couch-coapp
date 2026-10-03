// The only place the UI talks to the Rust side: one command and one event
// channel, typed with the bindings ts-rs generates from couch-engine.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Command } from "./bindings/Command";
import type { Event as EngineEvent } from "./bindings/Event";
import type { Response } from "./bindings/Response";

export function engineCommand(command: Command): Promise<Response> {
  return invoke<Response>("engine_command", { command });
}

export function onEngineEvent(handler: (event: EngineEvent) => void): Promise<UnlistenFn> {
  return listen<EngineEvent>("engine-event", (e) => handler(e.payload));
}
