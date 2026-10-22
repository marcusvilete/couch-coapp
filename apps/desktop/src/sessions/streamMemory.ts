// What the console remembers about the stream it started: whether the window
// runs as administrator (the engine's stream view doesn't say), so the
// console can keep warning while it is streamed.
let elevated: { name: string } | null = null;

export function rememberStream(name: string, isElevated: boolean) {
  elevated = isElevated ? { name } : null;
}

export function streamsElevated(name: string | undefined): boolean {
  return name != null && elevated?.name === name;
}
