#!/usr/bin/env bash
# Installs our relay (M7) on a fresh Debian or Ubuntu server, as root:
#   sudo ./install.sh relay.example.com you@example.com
# It builds iroh-relay (the version below, which must match the app's iroh pin in
# Cargo.toml), installs the config and the systemd unit from this folder, asks for the
# token (release.toml's relay_token), and starts it. Run it again to update.
# Untested until we have a server: follow README.md and check each step's output.
set -euo pipefail

RELAY_VERSION="1.3.0"

if [[ $# -ne 2 ]]; then
    echo "usage: sudo $0 <relay hostname> <contact email for Let's Encrypt>" >&2
    exit 2
fi
HOSTNAME_="$1"
CONTACT="$2"
HERE="$(cd "$(dirname "$0")" && pwd)"

# Both go into the config through sed: only plain names and addresses.
if [[ ! "$HOSTNAME_" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]]; then
    echo "not a host name: $HOSTNAME_" >&2
    exit 2
fi
if [[ ! "$CONTACT" =~ ^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+$ ]]; then
    echo "not an email address: $CONTACT" >&2
    exit 2
fi

if [[ $EUID -ne 0 ]]; then
    echo "run it as root (sudo)" >&2
    exit 1
fi

echo "== Build tools"
apt-get update -q
apt-get install -y -q build-essential curl ca-certificates

# Compiling needs about 2 GB of memory; small VMs get a swap file for it.
if [[ $(awk '/MemTotal/ {print $2}' /proc/meminfo) -lt 2000000 && ! -e /swapfile ]]; then
    echo "== Adding a 2 GB swap file for the build"
    fallocate -l 2G /swapfile
    chmod 600 /swapfile
    mkswap /swapfile
    swapon /swapfile
fi

# rustup's own toolchain, even if the distribution has an (older) cargo.
if [[ ! -x "$HOME/.cargo/bin/cargo" ]]; then
    echo "== Rust (rustup, minimal profile)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
export PATH="$HOME/.cargo/bin:$PATH"
rustup update stable

echo "== iroh-relay $RELAY_VERSION"
cargo install --locked iroh-relay --version "$RELAY_VERSION" --features server --root /usr/local
/usr/local/bin/iroh-relay --version

echo "== Configuration"
install -d -m 755 /etc/iroh-relay
sed -e "s/RELAY_HOSTNAME/$HOSTNAME_/" -e "s/CONTACT_EMAIL/$CONTACT/" \
    "$HERE/iroh-relay.toml" > /etc/iroh-relay/iroh-relay.toml
chmod 644 /etc/iroh-relay/iroh-relay.toml

if [[ ! -s /etc/iroh-relay/token.env ]]; then
    read -r -s -p "The relay token (release.toml's relay_token): " TOKEN
    echo
    if [[ -z "$TOKEN" ]]; then
        echo "no token given" >&2
        exit 1
    fi
    umask 077
    printf 'IROH_RELAY_ACCESS_TOKEN=%s\n' "$TOKEN" > /etc/iroh-relay/token.env
    umask 022
fi
chmod 600 /etc/iroh-relay/token.env

# Before the service starts: it asks Let's Encrypt for a certificate at once,
# which checks back on port 443.
echo "== Firewall"
# Oracle Cloud's Ubuntu images reject everything but SSH with iptables rules
# (no ufw): open our ports just before their REJECT rule, and keep them
# across reboots.
if command -v netfilter-persistent >/dev/null && iptables -S INPUT | grep -q -- '-j REJECT'; then
    for rule in "-p tcp --dport 80" "-p tcp --dport 443" "-p udp --dport 7842"; do
        # shellcheck disable=SC2086
        if ! iptables -C INPUT $rule -m state --state NEW -j ACCEPT 2>/dev/null; then
            at=$(iptables -L INPUT --line-numbers | awk '/REJECT/ {print $1; exit}')
            # shellcheck disable=SC2086
            iptables -I INPUT "$at" $rule -m state --state NEW -j ACCEPT
        fi
    done
    netfilter-persistent save
fi
if command -v ufw >/dev/null; then
    # Rules only: turning ufw on is left to you, so SSH can't be locked out by accident.
    ufw allow 22/tcp
    ufw allow 80/tcp
    ufw allow 443/tcp
    ufw allow 7842/udp
fi
echo "Open TCP 80 and 443 and UDP 7842 in your provider's firewall too (README.md)."

echo "== Service"
install -m 644 "$HERE/iroh-relay.service" /etc/systemd/system/iroh-relay.service
systemctl daemon-reload
systemctl enable iroh-relay
systemctl restart iroh-relay
sleep 3
systemctl --no-pager status iroh-relay || true
echo
echo "Done. Check https://$HOSTNAME_/ in a browser, then README.md's first run."
