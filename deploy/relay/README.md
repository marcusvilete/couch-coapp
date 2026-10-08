# Your own relay

Couch Co-App always tries a direct connection between two PCs first. A relay starts each connection, helps the two ends find each other, and carries the traffic when no direct path works (some home routers and most phone networks). There are no official installers or relay ([ADR 0009](../../docs/adr/0009-source-only-releases.md)): whoever builds installers for their friends runs a relay for them. iroh's public relays are for testing only.

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

It runs on Oracle Cloud's free tier for the maintainer's friends (October 2026). That setup is the worked example at the end.

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

- **Its job:** it keeps other iroh apps, and other people's Couch Co-App builds, off your relay. It's inside every installer, so it isn't a secret; the rate limits and the bandwidth cap protect the server.
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
- **The log has noise.** Within minutes of starting, scanners from all over probe any HTTPS server. They show up as harmless errors: handshakes cut short, or plain HTTP sent to 443. Without the token, they can't use the relay.
- **If it goes away** (the VM stopped, a reboot), it comes back by itself once the VM runs again: the service starts at boot with its saved certificate. Running apps reconnect on their own. Meanwhile, Settings → Network says "Not connected to a relay yet", and copying an invite fails.

## Worked example: Oracle Cloud's free tier

This is how the maintainer's relay was set up (October 2026). Oracle's console changes often, so the names may differ slightly.

1. **The account** (oracle.com/cloud/free):
   - **Pick the home region carefully:** it can't be changed later, and free resources exist only there.
   - **A card is needed** to verify you; the free trial doesn't charge it.
   - **During the trial,** create only resources marked "Always Free-eligible", or they stop when it ends.
2. **The network first.** The create-instance form often won't assign a public IP to a network it creates itself. So:
   1. Go to Networking → Virtual cloud networks → **Start VCN Wizard → "Create VCN with Internet Connectivity"**, with the defaults.
   2. In its **public subnet → Default Security List → Add Ingress Rules**, add three rules with source `0.0.0.0/0`:

      | Protocol | Port | Description |
      |---|---|---|
      | TCP | 80 | iroh-relay: captive portal check (plain HTTP) |
      | TCP | 443 | iroh-relay: relay over HTTPS, and Let's Encrypt certificate check |
      | UDP | 7842 | iroh-relay: QUIC address discovery |
3. **The VM** (Compute → Instances → Create):
   - **Image:** Canonical Ubuntu 24.04.
   - **Shape:** Ampere A1 Flex (1 OCPU, 6 GB) if available. Otherwise **VM.Standard.E2.1.Micro**, which is what this relay runs on. On the Micro, `install.sh` adds swap, and the build takes 30 to 60 minutes.
   - **Network:** the existing VCN, its **public** subnet, and "Automatically assign public IPv4 address".
   - **SSH key:** download the private key and keep it safe. Without it you can't get in.
4. **A name:** a DuckDNS subdomain pointed at the VM's public IP. SSH to the name works too.
5. **Install:** as in the first run above. Run it inside `tmux`, so a dropped SSH connection doesn't stop the long build:
   ```powershell
   scp -i <key> -r deploy/relay ubuntu@<name>:
   ```
   then, on the server, `tmux`, then `cd relay && chmod +x install.sh && sudo ./install.sh <name> <email>`. Oracle's Ubuntu images block every port but SSH with their own iptables rules; `install.sh` opens the three ports there too.
6. **Oracle can reclaim idle free VMs:** they get stopped after about a week below 20% use, and a relay is mostly idle.
   - **What to do:** press Start in the console. The VM kept its public IP through a stop and start, so DuckDNS needed no change.
   - **To avoid it:** upgrade the account to Pay As You Go, where Always Free resources stay free and aren't reclaimed. Set a budget alert first, under Billing → Budgets.
