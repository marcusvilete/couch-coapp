# Our relay

Couch Co-App always tries a direct connection between two PCs first. A relay starts each connection, helps the two ends find each other, and carries the traffic when no direct path works (some home routers and most phone networks). Production builds use only our own relay. iroh's public relays are for development.

The relay is iroh's own server, `iroh-relay`, at the version the app's iroh pin names (`iroh = "=X.Y.Z"` in the root `Cargo.toml`; 1.3.0 now). It forwards encrypted traffic it can't read. Admission to rooms happens between the PCs, never at the relay.

**What's in this folder:**
- `iroh-relay.toml`: the production config. It covers:
  - Let's Encrypt certificates;
  - QUIC address discovery;
  - the token;
  - rate limits;
  - metrics on localhost.
- `iroh-relay.service`: the systemd unit (its own user, sandboxed).
- `install.sh`: builds and installs it on Debian or Ubuntu.
- `dev.toml`: the same limits and access for running it on this PC (`scripts\relay.ps1`).

None of it has run on a real server yet. Follow the first run below and check each step.

## Choosing a server

**Bandwidth is what costs money.** CPU and memory matter little: the smallest VM is enough.
- A relayed viewer receives at most about 2 Mbit/s: the app caps video on relayed paths.
- That traffic passes through the relay twice, in from the host and out to the viewer. That's about **1.8 GB per relayed viewer per hour**, counted once each way.
- Most connections go direct, so only some sessions use the relay.

**Options:**
- **A small VPS** with a monthly traffic allowance (Hetzner, DigitalOcean, Vultr and similar), a few dollars a month.
- **Oracle Cloud's Always Free tier:** free small VMs with a generous monthly traffic allowance. Its catches:
  - signup needs a card;
  - capacity in some regions is hard to get;
  - idle free VMs can be reclaimed.

**Always:**
- **Set a bandwidth cap or billing alert** at the provider, so a surprise can't become a bill.
- **Pick a region** near most of your players.

## A name

Let's Encrypt needs a DNS name pointing at the server.
- **Your own domain:** an `A` record (and `AAAA` for IPv6), such as `relay.example.com`.
- **A free subdomain** from a dynamic-DNS service such as DuckDNS: point `yourname.duckdns.org` at the server's address.

## First run

1. **Prepare the server.** Create the VM with Debian 12 or Ubuntu 24.04, point the name at it, and wait until `ping <name>` answers from the server's address.
2. **Open the ports** in the provider's firewall (its security list or group):
   - TCP 80: the captive-portal check (plain HTTP);
   - TCP 443: the relay, and Let's Encrypt's certificate check (TLS-ALPN);
   - UDP 7842: QUIC address discovery.

   Keep SSH (TCP 22) open.
3. **Install.** Copy this folder to the server (`scp -r deploy/relay you@server:`), then run `sudo ./install.sh <name> <your email>`. When asked, paste the `relay_token` from your `release.toml`, created on your PC by `cargo xtask release-config`.
4. **Check it.**
   - `https://<name>/` should show iroh-relay's page in a browser, with a valid certificate. The first certificate can take a minute.
   - `journalctl -u iroh-relay -f` shows its log.
5. **Try it from your PC.** Run `couch-cli latency-probe --relay https://<name> --relay-token <token> --relay-only --secs 20`. The HUD shows "Relay", and the probe finishes.

   The same command without `--relay-token` must fail to connect: that proves the token is required.
6. **Point the app at it.** Set `relay_url = "https://<name>"` in `release.toml`, bump the version, and build an installer with `scripts\build-installer.ps1`. Friends install it. Their Settings → Network shows "Couch Co-App relay".

## The token

- **Its job:** it keeps other iroh apps off our relay. It's inside every installer, so it isn't a secret; the rate limits and the bandwidth cap protect the server.
- **Where it lives:**
  - `release.toml` on your PC, outside the repository;
  - `/etc/iroh-relay/token.env` on the server (root only, mode 600);
  - a password manager, as the backup.

  Never put it in this folder or anywhere in git.
- **Missing token:** the committed config's token list is empty on purpose (`[""]`). Without `IROH_RELAY_ACCESS_TOKEN`, the relay refuses to start rather than admit anyone.
- **Rotating it:** `IROH_RELAY_ACCESS_TOKEN` holds one token. During a rotation, put both tokens in the config instead:
  1. Write `access.shared_token = ["<old>", "<new>"]` in `/etc/iroh-relay/iroh-relay.toml`. Then comment out the line in `token.env`, since that variable would replace the list, and restart.

     The config file is readable by every user on the server (the service's dynamic user needs it). That's acceptable for this token, which ships in every installer anyway.
  2. Ship builds with the new token, and give everyone time to update.
  3. Go back to the new token alone in `token.env`, and restart.

  Don't run `install.sh` during a rotation: it puts back the empty list from this folder, and with `token.env` commented out the relay would then refuse to start.

## Running it

- **Metrics:** Prometheus text on the server's localhost only. Read them over SSH with `ssh -L 9090:127.0.0.1:9090 you@server`, then open `http://localhost:9090/metrics`.
- **Updating:**
  1. When the app's iroh pin changes, change `RELAY_VERSION` in `install.sh` to match.
  2. Run `install.sh` again: it rebuilds, keeps `token.env`, and restarts.
  3. Members on older builds keep working only if the new relay still speaks their version, so update the relay together with the app.
- **Limits** (`[limits]` in the config):
  - each client may send 2 MB/s, bursting to 4 MB: a host relaying to 7 viewers needs about 1.75 MB/s;
  - new connections are limited to 20 a second, bursting to 100.
