# ADR 0009: Source only, no public installers

**Status:** accepted (2026-10-08), the user's decision. It replaces the plan for a public binary release (M10b as first planned) and the updater (M11).

## Context

M10b was planned as a public binary release, with:
- installers for anyone, code-signed so Windows SmartScreen trusts them;
- a frozen wire format, so old and new public builds keep talking;
- our relay serving every user;
- later, an auto-updater (M11).

Each of those costs effort and money, and keeps costing:
- signing needs a certificate or a service, and an identity check;
- a frozen wire format slows every protocol change;
- a public relay carries strangers' traffic.

The app is GPL-3.0-only ([ADR 0008](0008-gpl-3.0.md)). The source can be public without the binaries being.

## Decision

- **No official installers for the public.** Installers built by the maintainer go to friends only, by hand.
- **Anyone else builds from source and hosts their own relay.** The repository is public on GitHub. The README and `deploy/relay/README.md` are written for a stranger to follow. A builder's own `release.toml` points their installers at their own relay; builds without one use iroh's public relays, for testing only.
- **Dropped:**
  - **the updater (M11);**
  - **code signing:** friends click past SmartScreen once, and builders sign their own if they want;
  - **the wire-format freeze:** `PROTOCOL_MINOR` stays a wire revision for good. It is bumped with every change to what members exchange, and builds of different revisions refuse each other with "update needed". Payloads may keep changing in place. Different people's builds never need to talk: each group runs one builder's installers, on that builder's relay.
  - **our relay serving the public:** it serves the maintainer's friends. Other builds can't use it anyway: they don't have its token.
- **Optional now:** the fallback encoder (a host without a GPU encoder), and the outside review of the admission composition. The review is still worth having before encouraging strangers to rely on the app.
- **Still required, because friends' installers are a distribution:**
  - the third-party notices in every installer;
  - the app's source: a link to the public repository at the installer's commit;
  - FFmpeg's source and build scripts, next to the installer.

## Consequences

- **SmartScreen keeps warning** on friends' installers ("More info", then "Run anyway").
- **Updates are a new installer,** handed out by the maintainer. The "update needed" message tells friends when they need it.
- **Self-builders need the full toolchain:** Visual Studio's C++ tools, Rust, Node.js and CMake. [docs/self-hosting.md](../self-hosting.md) lists it, step by step.
- **The ALPN stays `couch-coapp/0`.** There is no public release to move it to `/1`.
- **Reversible.** Signing and an updater can still be added later, in a new ADR.
