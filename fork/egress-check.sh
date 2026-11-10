#!/usr/bin/env bash
# Live check of this machine's amux processes: every listener and every remote peer must be loopback or named in fork/egress-allowlist.
# usage: fork/egress-check.sh [SECONDS]   (samples connections for SECONDS, default 60); exit 0 = clean, 1 = violations
set -euo pipefail

allow="$(cd "$(dirname "$0")" && pwd)/egress-allowlist"
secs="${1:-60}"
pids=$(pgrep -f '/amux-server-rs( |$)|amux-server\.py' || true)
[[ -n "$pids" ]] || { echo "verdict=egress_unmeasured no amux server process found"; exit 1; }

samples=""
end=$((SECONDS + secs))
while :; do
  samples+=$(lsof -nP -a -p "$(echo "$pids" | paste -sd, -)" -i 2>/dev/null | awk 'NR>1 {print $1, $2, $8, $9, $10}')$'\n'
  (( SECONDS >= end )) && break
  sleep 5
done

ALLOW="$allow" python3 - "$samples" <<'PY'
import ipaddress, os, re, subprocess, sys
rules = []
if os.path.exists(os.environ["ALLOW"]):
    for line in open(os.environ["ALLOW"]):
        line = line.split("#", 1)[0].split()
        if len(line) >= 2 and line[0] in ("listen", "peer"):
            rules.append((line[0], line[1]))
def allowed(kind, value):
    for k, v in rules:
        if k != kind:
            continue
        if kind == "listen" and v == value:
            return True
        if kind == "peer":
            try:
                if ipaddress.ip_address(value) in ipaddress.ip_network(v, strict=False):
                    return True
            except ValueError:
                pass
    return False
def loopback(host):
    host = host.strip("[]")
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return host == "localhost"
def proc(pid):
    out = subprocess.run(["ps", "-o", "args=", "-p", pid], capture_output=True, text=True).stdout.strip()
    return "amux-server.py (legacy python server)" if "amux-server.py" in out else os.path.basename(out.split()[0]) if out else pid
bad, seen = [], set()
for row in sys.argv[1].splitlines():
    parts = row.split()
    if len(parts) < 4 or parts[2] not in ("TCP", "UDP"):
        continue
    pid, addr, state = parts[1], parts[3], " ".join(parts[4:])
    if "LISTEN" in state or ("->" not in addr and parts[2] == "UDP"):
        host, _, port = addr.rpartition(":")
        if not loopback(host) and not allowed("listen", port):
            seen.add(("listen", proc(pid), addr))
    elif "->" in addr:
        host, _, port = addr.split("->", 1)[1].rpartition(":")
        if not loopback(host) and not allowed("peer", host.strip("[]")):
            seen.add(("peer", proc(pid), f"{host}:{port}"))
if "amux-server.py" in " ".join(proc(p) for p in set(r.split()[1] for r in sys.argv[1].splitlines() if len(r.split()) > 1)):
    seen.add(("process", "amux-server.py (legacy python server)", "running"))
for kind, who, what in sorted(seen):
    print(f"VIOLATION {kind:8} {who}: {what}")
print(f"verdict={'egress_violation' if seen else 'egress_clean'} n={len(seen)} rules={len(rules)}")
sys.exit(1 if seen else 0)
PY
