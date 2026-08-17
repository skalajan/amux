#!/usr/bin/env bash
# Performance baseline (Phase 9, RR checklist §Performance regressions).
#
# Boots the Rust server against a copy of the LIVE database (real data
# volume — 640k+ rows — because an empty-DB baseline measures nothing), then
# measures the plan's targets:
#   dashboard load  < 500ms cold
#   /api/board      < 200ms with real data (the Python takes 50ms+ on 6MB;
#                   the Rust list is desc-truncated by design)
#   /health         < 50ms
#   RSS             < 200MB
# Prints a JSON baseline line suitable for committing to docs/perf-baseline.json
# and comparing in CI (a >10% p95 regression is a failure per Phase 10).
set -euo pipefail

LIVE_DB="${AMUX_LIVE_DB:-$HOME/.amux/amux.db}"
WORK="$(mktemp -d /tmp/amux-perf.XXXXXX)"
PORT=18911
trap 'kill "$SERVER_PID" 2>/dev/null || true; rm -rf "$WORK"' EXIT

sqlite3 "file:${LIVE_DB}?mode=ro" ".backup '$WORK/amux.db'"

AMUX_HOME="$WORK" AMUX_DB="$WORK/amux.db" AMUX_RS_PORT=$PORT \
  "${AMUX_RS_BIN:-./target/release/amux-server}" >"$WORK/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 60); do
  curl -sk --max-time 2 "https://localhost:$PORT/health" >/dev/null 2>&1 && break
  sleep 0.5
done

TOKEN=$(cat "$WORK/auth-token" 2>/dev/null || echo "")

measure() { # name url [auth]
  local name=$1 url=$2 auth=${3:-}
  local total=0 worst=0
  for _ in 1 2 3 4 5; do
    local t
    if [ -n "$auth" ]; then
      t=$(curl -sk -o /dev/null -w '%{time_total}' -H "Authorization: Bearer $TOKEN" "$url")
    else
      t=$(curl -sk -o /dev/null -w '%{time_total}' "$url")
    fi
    t_ms=$(python3 -c "print(int(float('$t')*1000))")
    total=$((total + t_ms))
    [ "$t_ms" -gt "$worst" ] && worst=$t_ms
  done
  echo "\"${name}_avg_ms\": $((total / 5)), \"${name}_worst_ms\": $worst"
}

M1=$(measure dashboard "https://localhost:$PORT/")
M2=$(measure health "https://localhost:$PORT/health")
M3=$(measure board "https://localhost:$PORT/api/board" auth)
M4=$(measure board_full "https://localhost:$PORT/api/board?done_limit=0" auth)
M5=$(measure workers "https://localhost:$PORT/api/workers" auth)
RSS_MB=$(ps -o rss= -p "$SERVER_PID" | awk '{print int($1/1024)}')
BOARD_BYTES=$(curl -sk -H "Authorization: Bearer $TOKEN" "https://localhost:$PORT/api/board" | wc -c | tr -d ' ')

echo "{ $M1, $M2, $M3, $M4, $M5, \"rss_mb\": $RSS_MB, \"board_default_bytes\": $BOARD_BYTES }"

# Target assertions — each CAN fail (ethos 7).
fail=0
avg() { echo "$1" | sed 's/.*_avg_ms": \([0-9]*\).*/\1/'; }
[ "$(avg "$M1")" -lt 500 ] || { echo "FAIL: dashboard >= 500ms"; fail=1; }
[ "$(avg "$M2")" -lt 50 ] || { echo "FAIL: health >= 50ms"; fail=1; }
[ "$(avg "$M3")" -lt 200 ] || { echo "FAIL: board >= 200ms"; fail=1; }
[ "$RSS_MB" -lt 200 ] || { echo "FAIL: RSS ${RSS_MB}MB >= 200MB"; fail=1; }
[ "$fail" -eq 0 ] && echo "BASELINE PASSED" || exit 1
