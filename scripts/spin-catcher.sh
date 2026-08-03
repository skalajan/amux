#!/usr/bin/env bash
# Catch the amux server IN THE ACT of spinning and dump its stack (AC-170).
#
# The spin is intermittent and self-clearing: the process pegs a core, /api/board
# and /api/email/* hang, then launchd replaces it. By the time a human notices,
# the evidence is a fresh pid. Three wedges produced the SHAPE every time and the
# cause never, for exactly this reason.
#
# So this watches rather than samples. /health gained store/store_ms/degraded in
# AC-164 precisely so the degradation is observable from outside; this is the
# consumer that makes that signal worth having. STAT=R means the stack will NAME
# the function, so one dump taken at the right moment is worth more than a day of
# inference from sizes.
#
# Uses SIGUSR1, NOT py-spy. py-spy REQUIRES ROOT ON macOS, so on this machine it
# is not an available instrument for an unattended session — discovered by
# running it, not by reading about it. The server registers faulthandler on
# SIGUSR1 instead: it dumps ITS OWN stacks into ~/.amux/logs/server.log, needs no
# privileges, and does not exit. Verified against a synthetic pure-Python spin —
# the looping frame is named with its line number.
#
# Read-only in the sense that matters: SIGUSR1 only makes the process print. It
# does not stop or restart anything — the standing rule on this machine's
# launchd agents holds.
#
#   ./scripts/spin-catcher.sh [seconds_between_polls] [cpu_trigger]
set -uo pipefail
AMUX="${AMUX_URL:-https://localhost:8822}"
POLL="${1:-3}"
CPU_TRIP="${2:-70}"
OUT="${HOME}/.amux/spin-dumps"
mkdir -p "$OUT"

echo "spin-catcher: polling ${AMUX}/health every ${POLL}s"
echo "  trigger: store != ok  OR  degraded  OR  cpu_percent >= ${CPU_TRIP}"
echo "  stacks -> ~/.amux/logs/server.log (copies in ${OUT})"

caught=0
while :; do
  H="$(curl -sk --max-time 4 "${AMUX}/health" 2>/dev/null)"
  if [ -z "$H" ]; then
    # /health itself not answering is ALSO the event: the old endpoint could not
    # express this, and a silent curl failure is how it stayed invisible before.
    TS="$(date -u +%Y%m%dT%H%M%SZ)"
    echo "[$TS] /health did not answer — server may be gone or fully wedged" | tee -a "$OUT/events.log"
    PID="$(pgrep -f 'amux-server\.py' | head -1)"
    if [ -n "$PID" ]; then
      echo "[$TS] pid $PID still alive with /health dead — dumping" | tee -a "$OUT/events.log"
      kill -USR1 "$PID" 2>/dev/null && echo "  -> SIGUSR1 sent; stacks appended to ~/.amux/logs/server.log" 
      caught=$((caught+1))
    fi
    sleep "$POLL"; continue
  fi
  read -r PID CPU STORE MS DEG <<<"$(printf '%s' "$H" | python3 -c '
import json,sys
d=json.load(sys.stdin)
print(d.get("pid",0), d.get("cpu_percent",0), d.get("store","?"),
      d.get("store_ms",-1), 1 if d.get("degraded") else 0)
' 2>/dev/null)"
  [ -z "${PID:-}" ] && { sleep "$POLL"; continue; }

  TRIP=0
  case "$STORE" in ok) ;; *) TRIP=1 ;; esac
  [ "$DEG" = "1" ] && TRIP=1
  awk "BEGIN{exit !($CPU >= $CPU_TRIP)}" && TRIP=1

  if [ "$TRIP" = "1" ]; then
    TS="$(date -u +%Y%m%dT%H%M%SZ)"
    echo "[$TS] TRIP pid=$PID cpu=$CPU store=$STORE store_ms=$MS degraded=$DEG" | tee -a "$OUT/events.log"
    # Two dumps ~2s apart: one frame can catch an innocent function mid-call,
    # two that agree are a loop.
    kill -USR1 "$PID" 2>/dev/null
    sleep 2
    kill -USR1 "$PID" 2>/dev/null
    tail -c 4000 "$HOME/.amux/logs/server.log" > "$OUT/stacks-${TS}-pid${PID}.txt" 2>/dev/null
    caught=$((caught+1))
    echo "[$TS] captured 2 dumps (total events: $caught)" | tee -a "$OUT/events.log"
    sleep 20   # one event, not a dump storm
  fi
  sleep "$POLL"
done
