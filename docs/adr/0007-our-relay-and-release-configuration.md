# ADR 0007: Our relay admits only our app with a shared token; release values stay out of git

**Status:** accepted (2026-10-08), M7.

## Context

**Why a relay of our own.** Production must not depend on iroh's public relays, whose terms cover hobby use. Our own `iroh-relay` on a small VPS is the plan (M0).

**What it costs, and why that needs a gate.** The relay forwards traffic for any iroh client that connects, and bandwidth is what costs money. iroh-relay 1.3 offers four access controls:
- everyone;
- an allow or deny list of endpoint ids;
- an HTTP callback per connection;
- `shared_token`, a list of bearer tokens.

An allowlist can't work for us: anyone can install the app, and each install makes its own key. A callback needs a service of its own to answer it.

**What a token can and can't do.** Every installer must send the token, so anyone who looks inside one finds it. It can't prove a client is ours. It does keep other iroh apps, and anyone scanning for open relays, from using our bandwidth by default. The server's limits are what bound the cost:
- the per-client rate limit;
- the connection-rate limit;
- the provider's bandwidth cap.

**How iroh sends it** (`RelayConfig::with_auth_token`): only to the relay URLs in the client's own relay map. A member whose home relay is somewhere else gets no token from us, and invites don't carry it.

**The other release values.** The relay's URL and the invite page's address must reach the release build without living in the source. So must the token, and later the signing keys (M10b, M11). The repository may become public one day.

## Decision

- **The relay.** It runs `access.shared_token`. The real token comes from `IROH_RELAY_ACCESS_TOKEN` in a root-only environment file on the server; the config in git holds a placeholder. Rotation uses the config's token list: old and new together until old builds are gone (`deploy/relay/README.md`).
- **The client.**
  - `RelayChoice::Custom { urls, token }` attaches the token to our relay's entries only.
  - `RelayToken` prints as `..`.
  - Plain http relays skip QUIC address discovery.
- **Release values come from a private file:** `release.toml`, at `%USERPROFILE%\.couch-coapp\` (or `COUCH_RELEASE_CONFIG`), outside the repository.
  - `cargo xtask release-config` creates it with a random 256-bit token.
  - `build-installer.ps1` reads it, refuses one inside the repository, checks its values, and passes them to the build as `COUCH_BUILD_*` environment variables for that build only.
  - `couch_engine::build_config` reads them with `option_env!`. rustc records them, so cargo rebuilds when they change.
  - Development builds get none, and use iroh's public relays.
  - Without the file, only a test installer (`-TestRelay`) can be built.
- **The app's default follows the build.** `RelaySetting::Builtin`, the default, is the built-in relay, or iroh's public relays in a build without one. Saved `public` and custom choices stay.
- **Plain http to private addresses** (the LAN) is allowed in development and test builds only. Release builds take https, or http on this PC.

## Consequences

- **Rooms should share one relay setting.** A member on iroh's public relays or a custom URL can't use ours, so a connection between them and one of us needs a direct path. Settings → Network says so. Release builds all default to ours.
- **Anyone can extract the token.** Abuse is bounded by the server's limits, the bandwidth cap and rotation, not by the token.
- **No change on the wire.** Invites already accepted any link ending in `#<payload>`, so https invite links needed no new version.
- **Secrets have one home.** The signing keys of M10b and M11 follow the same rule: a private file or hardware outside the repository, handed to the build by the build script, never committed.
