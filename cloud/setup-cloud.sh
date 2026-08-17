#!/bin/bash
# amux cloud multi-tenant bootstrap — Hetzner Ubuntu 22.04
# Run as root after SSH-ing into the new server.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

log() { echo "[$(date '+%H:%M:%S')] $*"; }
log "=== amux cloud setup ==="

# ── Deps ──────────────────────────────────────────────────────────────────────
apt-get update -qq
apt-get install -y -qq curl git python3 python3-pip nginx certbot python3-certbot-nginx

# ── Docker ────────────────────────────────────────────────────────────────────
if ! command -v docker &>/dev/null; then
  log "Installing Docker..."
  curl -fsSL https://get.docker.com | sh
fi
systemctl enable --now docker

# ── Python deps for gateway ───────────────────────────────────────────────────
# The GATEWAY stays python. It is a host process, not the product server: it
# verifies Clerk JWTs, owns per-tenant docker orchestration and billing, and
# never runs a customer's code. Deleting the python SERVER did not make it
# python-free, and rewriting it was not part of the server cutover.
pip3 install -q "PyJWT[crypto]" cryptography stripe

# ── Directories ───────────────────────────────────────────────────────────────
mkdir -p /var/amux/users /opt/amux/cloud

# ── Clone / copy amux repo ────────────────────────────────────────────────────
if [ ! -d /opt/amux/.git ]; then
  git clone https://github.com/mixpeek/amux.git /opt/amux
else
  git -C /opt/amux pull --ff-only
fi

# ── Build the workspace image ─────────────────────────────────────────────────
# The image is a RUST build now (AMUX-2619). Two things changed and both are
# load-bearing:
#   * there is no `cp amux-server.py cloud/docker/` step — that file was deleted
#     from the repo; the server is compiled into the image from crates/.
#   * the build CONTEXT is the repo root, not cloud/docker/, because the
#     Dockerfile needs Cargo.toml, Cargo.lock, crates/ and the `amux` CLI.
# Normally the image arrives from ghcr.io via .github/workflows/deploy-cloud.yml;
# this local build is the bootstrap/offline path.
log "Building amux Docker image (rust)..."
docker build -f /opt/amux/cloud/docker/Dockerfile -t ghcr.io/mixpeek/amux:latest /opt/amux

# ── Gateway env ───────────────────────────────────────────────────────────────
mkdir -p /etc/amux
# AC-239: these were HARDCODED here — Clerk secret, R2 access+secret, CF
# account id — committed to a PUBLIC repo since 2026-03-11. The R2 pair is what
# litestream uses to replicate every per-user database, so it was live storage
# credentials for user data, not just a test key. Removing them here does NOT
# undo git history: the values must be ROTATED, which is the owner's action.
# What this change buys is that the file stops teaching the pattern and HEAD
# stops carrying the values, so the CI secret scan can gate every future push.
#
# Provide them in the environment when running this script, e.g.
#   CLERK_SECRET_KEY=... R2_SECRET_KEY=... ./setup-cloud.sh
# Same shape seed.py and cloud/tests/e2e_smoke.py already use.
if [ ! -f /etc/amux/gateway.env ]; then
  : "${CLERK_PUBLISHABLE_KEY:?set CLERK_PUBLISHABLE_KEY in the environment}"
  : "${CLERK_SECRET_KEY:?set CLERK_SECRET_KEY in the environment}"
  : "${R2_ACCESS_KEY:?set R2_ACCESS_KEY in the environment}"
  : "${R2_SECRET_KEY:?set R2_SECRET_KEY in the environment}"
  : "${CF_ACCOUNT_ID:?set CF_ACCOUNT_ID in the environment}"
  # No quoted heredoc: these must expand. chmod BEFORE writing secrets so the
  # values never exist in a world-readable file, even briefly.
  install -m 600 /dev/null /etc/amux/gateway.env
  cat > /etc/amux/gateway.env << EOF
CLERK_PUBLISHABLE_KEY=${CLERK_PUBLISHABLE_KEY}
CLERK_SECRET_KEY=${CLERK_SECRET_KEY}
R2_ACCESS_KEY=${R2_ACCESS_KEY}
R2_SECRET_KEY=${R2_SECRET_KEY}
CF_ACCOUNT_ID=${CF_ACCOUNT_ID}
GATEWAY_PORT=${GATEWAY_PORT:-8080}
AMUX_CLOUD_DATA=/var/amux/users
GATEWAY_DB=/var/amux/gateway.db
IDLE_TIMEOUT=${IDLE_TIMEOUT:-600}
EOF
fi

# ── Gateway systemd service ───────────────────────────────────────────────────
cat > /etc/systemd/system/amux-gateway.service << 'EOF'
[Unit]
Description=amux cloud gateway
After=network.target docker.service

[Service]
Type=simple
WorkingDirectory=/opt/amux/cloud/gateway
EnvironmentFile=/etc/amux/gateway.env
ExecStart=/usr/bin/python3 /opt/amux/cloud/gateway/gateway.py
Restart=always
RestartSec=3
StandardOutput=append:/var/log/amux-gateway.log
StandardError=append:/var/log/amux-gateway.log

[Install]
WantedBy=multi-user.target
EOF

# ── nginx ─────────────────────────────────────────────────────────────────────
cp /opt/amux/cloud/gateway/nginx.conf /etc/nginx/sites-available/amux-cloud
ln -sf /etc/nginx/sites-available/amux-cloud /etc/nginx/sites-enabled/amux-cloud
rm -f /etc/nginx/sites-enabled/default

# Start nginx with HTTP only first (certbot needs it to verify domain)
sed -i 's/listen 443 ssl;//; s/ssl_certificate.*//; s/ssl_protocols.*//; s/ssl_ciphers.*//' \
  /etc/nginx/sites-available/amux-cloud 2>/dev/null || true
nginx -t && systemctl reload nginx

# ── TLS — run after DNS A record points here ──────────────────────────────────
SERVER_IP=$(curl -s ifconfig.me)
log ""
log "=== Setup complete ==="
log "Server IP: $SERVER_IP"
log ""
log "Next steps:"
log "  1. Add DNS A record:  cloud.amux.io → $SERVER_IP"
log "  2. Once DNS propagates, run:"
log "     certbot --nginx -d cloud.amux.io --non-interactive --agree-tos -m hello@mixpeek.com"
log "  3. Then start the gateway:"
log "     systemctl daemon-reload && systemctl enable --now amux-gateway"
log ""
log "  Check logs: tail -f /var/log/amux-gateway.log"

systemctl daemon-reload
systemctl enable amux-gateway
