# ADR 0002: The stream renders in a separate native viewer process

**Status:** accepted (2026-10-02).

## Context

The project owner chose Tauri for the app UI. A webview is a poor place for a low-latency game stream:
- Browser WebRTC jitter buffers add about 100 ms.
- WebView2 and WKWebView lack the Keyboard Lock API.
- Gamepad and pointer-lock support is unreliable across webviews.
- Linux's WebKitGTK ships without WebRTC.
- Tauri has no zero-copy path for frames from Rust.

## Decision

- **Where the stream lives.** `couch-viewer` is a separate binary, started by the main app as a child process. It handles decoding, rendering (wgpu), audio playback, input capture (SDL3) and the in-stream HUD (egui).
- **What it doesn't do.** It does no networking. The main process forwards encoded frames over its stdin and reads input and timing from its stdout, framed as described in [protocol.md](../protocol.md).
- **What stays in Tauri.** The lobby and room UI.

## Consequences

- **Crash isolation.** A GPU driver or decoder crash kills the viewer, not the app; the main process respawns it once.
- **macOS works the same way.** It needs SDL and Tauri each on their process's main thread, which separate processes give us.
- **stdout is reserved.** It belongs to the IPC protocol, so the viewer must never print to it. Logs go to stderr, and `clippy::print_stdout` is denied in the crate.
- **Two UI stacks:** React in the lobby, egui in the HUD. The HUD stays small: who is watching, who has control, request control.

## Fallback

If SDL3, wgpu and egui together don't work out, the window moves to winit with egui-winit. SDL3 then handles only gamepads.
