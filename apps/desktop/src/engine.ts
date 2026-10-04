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

/** Sends `command` and checks the reply is the `expected` kind. Rejects with
 * the engine's error text, ready to show. */
export async function request<K extends Response["type"]>(
  command: Command,
  expected: K,
): Promise<Extract<Response, { type: K }>> {
  let reply: Response;
  try {
    reply = await engineCommand(command);
  } catch (e) {
    throw new Error(String(e));
  }
  if (reply.type !== expected) {
    throw new Error(`The engine answered ${reply.type}, not ${expected}.`);
  }
  return reply as Extract<Response, { type: K }>;
}

export function onEngineEvent(handler: (event: EngineEvent) => void): Promise<UnlistenFn> {
  return listen<EngineEvent>("engine-event", (e) => handler(e.payload));
}

export function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
