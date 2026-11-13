# iroh 1.3.0, patched for Couch Co-App

This is iroh 1.3.0 as published on crates.io (https://github.com/n0-computer/iroh, commit 0072d7d84b233f9e7185eb676f049beaf557ac03; MIT OR Apache-2.0, whose texts are beside this file, taken from that commit since the crate ships only `LICENSE-BSD3`), with one change, wired in by `[patch.crates-io]` in the workspace's `Cargo.toml`, marked "Couch Co-App" in the code:

- **A relay connection starts again when its entry in the relay map changes** (`src/socket.rs`'s `handle_relay_map_change`, `src/socket/transports.rs` and `src/socket/transports/relay.rs`'s `relay_map_change`, and `RelayActorMessage::RelayMapChange` with `RelayActor::restart_changed_relays` in `src/socket/transports/relay/actor.rs`). `Endpoint::insert_relay` and `remove_relay` now also stop each relay connection whose token differs from what the map holds for its relay now (a new token, or the relay removed), and the home relay's connection starts again at once. A connection to a relay outside the map without a token (another member's) is left alone. `ActiveRelayHandle` keeps the token each connection started with, and its own stop token.

**Why.** A relay connection reads its token from the map once, when it starts, so iroh 1.3 kept the old token until the connection had been idle for 60 s, and the home relay's for good. Couch Co-App registers relays while running (M26, ADR 0014), and found in M26a's first tests (`crates/couch-engine/tests/relay.rs`, mod `registered`, 2026-11-13) that, unpatched:
- a relay added after a dial to it was refused (the usual moment: a member is on a relay we lack, and its code is pasted) kept being dialed without its token;
- a new code for a relay refusing ours (rotated or revoked) never reached it while it was the home relay;
- a removed relay kept receiving its token.

Those three tests fail unpatched and pass with this change. Everything else is unchanged.

To update: copy the new release over this folder (keeping `LICENSE-MIT` and `LICENSE-APACHE` if it still ships none), apply the change again, and run `cargo test -p couch-engine --test relay`. A release that restarts relay connections on a map change needs no patch: then remove this folder, its line in `.gitignore` and the `[patch.crates-io]` line.
