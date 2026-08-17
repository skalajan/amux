#!/usr/bin/env bash
# HISTORICAL (python->rust migration tooling): the rehearsal this automated has
# happened — the Python server was removed 2026-08-09 and the Rust server now
# answers both ports. Kept as migration history alongside docs/rust-migration/;
# do not run it against the live system.
# Migration rehearsal (Phase 11, RR checklist §Migration rehearsal).
#
# Proves, against a COPY of the live database, that:
#   1. the Rust server's migration path applies cleanly (additive only),
#   2. every pre-existing table and row count survives untouched,
#   3. the PYTHON server's queries still work on the migrated file
#      (rollback compatibility — the DB must stay bilingual).
#
# The live DB is opened READ-ONLY via sqlite backup; nothing here can write
# to production. Run it any time; it is the repeatable go/no-go evidence
# generator for cutover.
set -euo pipefail

LIVE_DB="${AMUX_LIVE_DB:-$HOME/.amux/amux.db}"
WORK="$(mktemp -d /tmp/amux-rehearsal.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
COPY="$WORK/amux.db"

echo "== 1. snapshot (read-only backup of $LIVE_DB)"
sqlite3 "file:${LIVE_DB}?mode=ro" ".backup '$COPY'"

echo "== 2. pre-migration census"
sqlite3 "$COPY" "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%';" > "$WORK/tables_before"
sqlite3 "$COPY" "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name;" > "$WORK/names_before"
# Row counts for the tables the Python server reads hottest.
for t in issues schedules prefs cal_events crm_contacts session_events token_ledger; do
  echo "$t $(sqlite3 "$COPY" "SELECT COUNT(*) FROM $t;")" >> "$WORK/rows_before"
done
cat "$WORK/rows_before"

echo "== 3. rust migration path (the EXACT production Store::open)"
AMUX_HOME="$WORK" AMUX_DB="$COPY" AMUX_RS_MIGRATE_ONLY=1 \
  "${AMUX_RS_BIN:-./target/debug/amux-server}"

echo "== 4. post-migration invariants"
# 4a. Every pre-existing table still exists.
sqlite3 "$COPY" "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name;" > "$WORK/names_after"
if ! comm -23 "$WORK/names_before" "$WORK/names_after" | grep -q .; then
  echo "ok: no table lost"
else
  echo "FAIL: tables LOST by migration:"; comm -23 "$WORK/names_before" "$WORK/names_after"; exit 1
fi
# 4b. Row counts unchanged in Python's tables (additive-only proof).
for t in issues schedules prefs cal_events crm_contacts session_events token_ledger; do
  before=$(grep "^$t " "$WORK/rows_before" | awk '{print $2}')
  after=$(sqlite3 "$COPY" "SELECT COUNT(*) FROM $t;")
  if [ "$before" != "$after" ]; then
    echo "FAIL: $t row count moved $before -> $after"; exit 1
  fi
done
echo "ok: row counts unchanged across all sampled tables"
# 4c. Integrity.
[ "$(sqlite3 "$COPY" "PRAGMA integrity_check;")" = "ok" ] && echo "ok: integrity_check" || { echo "FAIL: integrity"; exit 1; }

echo "== 5. python-side reads still work (rollback direction)"
python3 - "$COPY" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.row_factory = sqlite3.Row
# The Python server's real hot queries, verbatim shapes.
open_issues = db.execute(
    "SELECT COUNT(*) FROM issues WHERE deleted IS NULL AND status NOT IN ('done','verified','discarded')"
).fetchone()[0]
schedules = db.execute("SELECT COUNT(*) FROM schedules WHERE enabled=1").fetchone()[0]
prefs = dict(db.execute("SELECT key, value FROM prefs").fetchall())
# A WRITE in the python direction (on the copy): the rollback server must
# still be able to mutate.
db.execute("INSERT INTO prefs (key, value) VALUES ('rehearsal_probe','1') "
           "ON CONFLICT(key) DO UPDATE SET value='1'")
db.commit()
assert db.execute("SELECT value FROM prefs WHERE key='rehearsal_probe'").fetchone()[0] == '1'
print(f"ok: python reads+writes post-migration (open_issues={open_issues}, enabled_schedules={schedules}, prefs={len(prefs)})")
PY

echo "== 6. data CONTINUITY through the Rust APIs (every subsystem serves its migrated data)"
PORT=18931
AMUX_HOME="$WORK" AMUX_DB="$COPY" AMUX_RS_PORT=$PORT \
  "${AMUX_RS_BIN:-./target/debug/amux-server}" >"$WORK/server.log" 2>&1 &
SRV=$!
trap 'kill "$SRV" 2>/dev/null || true; rm -rf "$WORK"' EXIT
for _ in $(seq 1 60); do
  curl -sk --max-time 2 "https://localhost:$PORT/health" >/dev/null 2>&1 && break; sleep 0.5
done
TOKEN=$(cat "$WORK/auth-token")
probe() { # name url jq_count sql_count
  local name=$1 url=$2 api_n sql_n
  api_n=$(curl -sk --max-time 10 -H "Authorization: Bearer $TOKEN" "https://localhost:$PORT$url" | python3 -c "$3" 2>/dev/null || echo ERR)
  sql_n=$(sqlite3 "$COPY" "$4")
  if [ "$api_n" = "$sql_n" ]; then
    echo "ok: $name — API serves all $sql_n migrated rows"
  else
    echo "FAIL: $name — API $api_n vs SQL $sql_n"; exit 1
  fi
}
PYC_LEN="import json,sys; print(len(json.load(sys.stdin)))"
probe board "/api/board?done_limit=0&archived=all" "$PYC_LEN" \
  "SELECT COUNT(*) FROM issues WHERE deleted IS NULL;"
# deleted IS NULL: the Python GET filters soft-deleted (amux-server.py:70800)
# — a probe counting them would fail the API for agreeing with Python.
probe schedules "/api/schedules" "$PYC_LEN" \
  "SELECT COUNT(*) FROM schedules WHERE deleted IS NULL;"
probe prefs "/api/prefs" "import json,sys; print(len(json.load(sys.stdin)))" \
  "SELECT COUNT(*) FROM prefs;"
probe cal_events "/api/cal-events" "$PYC_LEN" \
  "SELECT COUNT(*) FROM cal_events WHERE deleted IS NULL;"
probe journal "/api/journal?limit=100000" "$PYC_LEN" \
  "SELECT COUNT(*) FROM journal_entries WHERE deleted IS NULL;"
probe history "/api/history?limit=1000000" \
  "import json,sys; d=json.load(sys.stdin); print(len(d if isinstance(d,list) else d.get('items',d.get('history',[]))))" \
  "SELECT COUNT(*) FROM cmd_history;"
# Continuity WRITE through the Rust API, visible to a Python-shaped read:
NEW=$(curl -sk --max-time 10 -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"title":"continuity probe (rehearsal)","status":"todo"}' "https://localhost:$PORT/api/board" | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
python3 - "$COPY" "$NEW" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1]); db.row_factory = sqlite3.Row
r = db.execute("SELECT title, status, typeof(created) t FROM issues WHERE id=?", (sys.argv[2],)).fetchone()
assert r and r["title"] == "continuity probe (rehearsal)" and r["t"] == "integer", dict(r or {})
print(f"ok: rust-written card {sys.argv[2]} readable Python-side with int timestamps")
PY
kill "$SRV" 2>/dev/null || true
echo "== REHEARSAL PASSED"
