# vigem-rust 0.2.0, patched for Couch Co-App

This is vigem-rust 0.2.0 as published on crates.io (https://github.com/arounre/vigem-rust, MIT OR Apache-2.0; its licence texts are beside this file), with two changes, wired in by `[patch.crates-io]` in the workspace's `Cargo.toml`, each marked "Couch Co-App" in the code:

- `ClientBuilder::first_serial(n)` (`src/client.rs`): every serial search of that client starts at `n` instead of a random serial.
- `Bus::plug` (`src/internal/bus.rs`, `is_optional_wait_device_ready_error`): a `VIGEM_WAIT_DEVICE_READY` that answers `ERROR_DEVICE_HARDWARE_ERROR`, or doesn't answer within the library's 5 s (`ERROR_TIMEOUT`), no longer unplugs the pad and fails the plug. The pad is kept, and `wait_for_ready`'s probes (neutral reports until several are accepted) decide when it takes input.

**Why.** Each ViGEmBus serial (1 to 64) is a device instance of its own to Windows, set up the first time a pad with that serial is plugged. vigem-rust starts each client's search at a random serial, so a fresh PC sets up a new device on most plugs, and a pad plugged while Windows sets it up can fail ViGEmBus's readiness check (`VIGEM_WAIT_DEVICE_READY`, 0x800701E3: seen on a friend's PC, 2026-10-23). vigem-rust then unplugged it, which didn't take: the pads came up a moment later, unowned, and stayed until removed by hand. The original ViGEm client starts from serial 1, so a PC sets up the first few once and reuses them; Couch Co-App does the same now (`couch_platform_win::vigem`).

Everything else is unchanged, but for line endings (LF) and the removed `.github` folder. To update: copy the new release over this folder, apply the change again, and compare with `cargo package --list` of the release.
