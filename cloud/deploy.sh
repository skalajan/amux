#!/usr/bin/env bash
# amux cloud deploy — provision + deploy + test
# Usage: ./deploy.sh [--destroy]
#
# ⚠ NOT MIGRATED TO RUST (AMUX-2619). This script provisions the SINGLE-VM GCP
# box (`amux-dev`, main.tf + setup.sh) — a different topology from the
# multi-tenant cloud.amux.io host, which is served by
# .github/workflows/deploy-cloud.yml and a container image.
#
# It deploys `amux-server.py` to a systemd unit that runs it under python3.
# That file was deleted from the repo (792ce1f), so every path below the
# terraform apply is dead. It fails immediately rather than provisioning a VM
# whose service can never start — a half-provisioned VM that looks deployed is
# worse than a refusal.
#
# `--destroy` still works: tearing down infrastructure that exists must not
# depend on the deploy path being alive.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AMUX_SERVER="$SCRIPT_DIR/../amux-server.py"

log()  { echo "$(tput bold)→$(tput sgr0) $*"; }
ok()   { echo "$(tput setaf 2)✓$(tput sgr0) $*"; }
err()  { echo "$(tput setaf 1)✗$(tput sgr0) $*" >&2; exit 1; }
warn() { echo "$(tput setaf 3)⚠$(tput sgr0) $*"; }

# ── Destroy mode ──
if [[ "${1:-}" == "--destroy" ]]; then
  log "Destroying infrastructure..."
  cd "$SCRIPT_DIR"
  terraform destroy -auto-approve
  ok "Destroyed."
  exit 0
fi

# ── Check prerequisites ──
command -v terraform &>/dev/null || err "terraform not found"
command -v tailscale &>/dev/null || err "tailscale not found"
# Name the real cause. "amux-server.py not found" invited someone to go and
# recreate it; the file is gone on purpose and this topology needs porting to
# the rust binary before it can deploy anything.
[ -f "$AMUX_SERVER" ] || err "this single-VM deploy path is NOT migrated to the rust server.
  It expects $AMUX_SERVER, which was deleted in 792ce1f — do not recreate it.
  For the multi-tenant host use .github/workflows/deploy-cloud.yml (container image).
  To port THIS path: build/ship target/release/amux-server and change the
  systemd ExecStart in cloud/setup.sh from 'python3 amux-server.py' to the binary.
  './deploy.sh --destroy' still works."

# ── terraform.tfvars ──
cd "$SCRIPT_DIR"
if [ ! -f terraform.tfvars ]; then
  log "terraform.tfvars not found."
  read -r -p "  GCP project ID: " PROJECT_ID
  [ -z "$PROJECT_ID" ] && err "GCP project ID required"
  read -r -s -p "  Tailscale auth key (tskey-auth-...): " TS_KEY
  echo
  [ -z "$TS_KEY" ] && err "Tailscale auth key required"
  cat > terraform.tfvars <<EOF
project_id         = "$PROJECT_ID"
tailscale_auth_key = "$TS_KEY"
EOF
  ok "terraform.tfvars created"
fi

# ── Terraform init + apply ──
log "Initialising Terraform..."
terraform init -upgrade -input=false 2>&1 | grep -E "provider|Installed|Reusing|error" || true

log "Applying Terraform (this takes ~2 min)..."
terraform apply -auto-approve -input=false
PUBLIC_IP=$(terraform output -raw public_ip 2>/dev/null || true)
ok "VM provisioned — public IP: $PUBLIC_IP"

# ── Wait for Tailscale peer ──
log "Waiting for amux-cloud to appear in Tailscale (up to 10 min)..."
TS_HOST=""
for i in $(seq 1 120); do
  TS_HOST=$(tailscale status --json 2>/dev/null \
    | python3 -c "
import sys, json
d = json.load(sys.stdin)
for peer in d.get('Peer', {}).values():
    hn = peer.get('HostName', '')
    dns = peer.get('DNSName', '').rstrip('.')
    if 'amux-cloud' in hn or 'amux-cloud' in dns:
        print(dns or hn)
        break
" 2>/dev/null || true)
  if [ -n "$TS_HOST" ]; then
    ok "Found: $TS_HOST"
    break
  fi
  printf "."
  sleep 5
done
echo
[ -z "$TS_HOST" ] && err "amux-cloud did not appear in Tailscale after 10 min. Check GCP console logs."

# ── Deploy amux-server.py via IAP ──
log "Deploying amux-server.py via gcloud IAP..."
PROJECT_ID=$(grep project_id terraform.tfvars | awk -F'"' '{print $2}')
# Derive OS Login username: email with @ and . replaced by _
IAP_USER=$(gcloud config get-value account 2>/dev/null | tr '@.' '_')
sleep 5
gcloud compute scp \
    "$AMUX_SERVER" \
    "$SCRIPT_DIR/../scripts/watchdog.py" \
    "${IAP_USER}@amux-dev:/tmp/" \
    --zone=us-central1-a --project="$PROJECT_ID" \
    --tunnel-through-iap --quiet
gcloud compute ssh "${IAP_USER}@amux-dev" \
    --zone=us-central1-a --project="$PROJECT_ID" \
    --tunnel-through-iap --quiet \
    --command="sudo cp /tmp/amux-server.py /opt/amux/amux-server.py && sudo mkdir -p /opt/amux/scripts && sudo cp /tmp/watchdog.py /opt/amux/scripts/watchdog.py"
ok "amux-server.py + watchdog deployed"

# ── Start amux service via IAP ──
log "Starting amux service..."
gcloud compute ssh "${IAP_USER}@amux-dev" \
    --zone=us-central1-a --project="$PROJECT_ID" \
    --tunnel-through-iap --quiet \
    --command="sudo systemctl start amux && sleep 3 && sudo systemctl is-active amux"
ok "amux service started"

# ── Enable Tailscale Funnel for public iCal access ──
# Funnel routes through Tailscale's edge (no firewall changes needed).
# This makes /api/calendar.ics subscribable from Google Calendar, etc.
log "Enabling Tailscale Funnel on port 8822..."
ssh -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    "root@$TS_HOST" \
    "tailscale funnel --bg 8822 2>/dev/null || tailscale serve --bg --https=443 / proxy https://localhost:8822"
FUNNEL_URL="https://$(echo "$TS_HOST" | sed 's/\.$//').ts.net"
ok "Tailscale Funnel enabled → $FUNNEL_URL"

# ── Test endpoints ──
log "Testing API endpoints..."
sleep 3

AMUX_URL="https://$TS_HOST:8822"

# Test /api/sessions
SESSIONS=$(curl -sk --max-time 10 "$AMUX_URL/api/sessions" 2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin); print(f'{len(d)} sessions')" 2>/dev/null || echo "failed")
echo "  GET /api/sessions → $SESSIONS"

# Test /api/board
BOARD=$(curl -sk --max-time 10 "$AMUX_URL/api/board" 2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin); print(f'{len(d)} items')" 2>/dev/null || echo "failed")
echo "  GET /api/board    → $BOARD"

# Create a test board item
CREATED=$(curl -sk --max-time 10 -X POST \
  -H 'Content-Type: application/json' \
  -d '{"title":"GCP deploy test","desc":"Smoke test from deploy.sh","status":"done"}' \
  "$AMUX_URL/api/board" 2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('id','?'))" 2>/dev/null || echo "failed")
echo "  POST /api/board   → id=$CREATED"

echo ""
ok "===================================================="
ok " amux cloud is live!"
ok "===================================================="
echo ""
echo "  Dashboard  : $AMUX_URL"
echo "  SSH        : ssh root@$TS_HOST"
echo ""
echo "  Add to your local amux server switcher:"
echo "    Name : amux-cloud"
echo "    URL  : $AMUX_URL"
echo ""
echo "  Calendar subscription (Google Calendar / Apple Calendar):"
echo "    $FUNNEL_URL/api/calendar.ics"
echo ""
warn "The VM has a public IP for internet access."
warn "All inbound except Tailscale UDP 41641 is blocked by firewall."
warn "Access the dashboard ONLY via Tailscale ($TS_HOST)."
warn "The iCal feed is public via Tailscale Funnel — it contains board due dates only (no secrets)."
