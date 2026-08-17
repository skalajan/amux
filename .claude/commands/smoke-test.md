---
description: Use after making changes to the amux server or dashboard to verify the dashboard and APIs are working correctly
allowed-tools: Bash, Read
argument-hint: [quick|full]
context: fork
---

> **Auth:** every `curl` below sends `-H "$AMUX_AUTH"`. Define it once per shell:
> `export AMUX_AUTH="Authorization: Bearer $(cat ~/.amux/auth_token)"`
> This fork runs the server with `AMUX_RS_NO_LOOPBACK_BYPASS=1`, so localhost is
> NOT trusted — an unauthenticated request gets 401, including reads.

# /smoke-test — Product Verification

Quick health check of the amux server and dashboard after code changes.

Checks below hit `$AMUX_URL`, which defaults to `https://localhost:8822` when unset.

The user's request is: **$ARGUMENTS**

## Quick (default)

Run these checks and report pass/fail for each:

```bash
# 1. Server is up
curl -sk -H "$AMUX_AUTH" -o /dev/null -w '%{http_code}' $AMUX_URL/ | grep -q 200

# 2. Dashboard HTML is valid (contains expected markers)
curl -sk -H "$AMUX_AUTH" $AMUX_URL/ | grep -q 'id="app"'

# 3. Sessions API responds
curl -sk -H "$AMUX_AUTH" $AMUX_URL/api/sessions | python3 -c "import json,sys; d=json.load(sys.stdin); print(f'{len(d)} sessions')"

# 4. Board API responds
curl -sk -H "$AMUX_AUTH" $AMUX_URL/api/board | python3 -c "import json,sys; d=json.load(sys.stdin); print(f'{len(d)} items')"

# 5. SSE endpoint connects (grab first event within 15s)
timeout 15 curl -sk -H "$AMUX_AUTH" -N $AMUX_URL/api/events 2>/dev/null | head -5

# 6. Workspace compiles + client JS parses
cargo check --workspace --quiet && echo "cargo check ok"
node --check crates/amux-dashboard/static/app.js && echo "client JS ok"
```

## Full

Run all quick checks plus:

```bash
# 7. Notes API
curl -sk -H "$AMUX_AUTH" $AMUX_URL/api/notes | python3 -c "import json,sys; json.load(sys.stdin); print('notes ok')"

# 8. Schedules API
curl -sk -H "$AMUX_AUTH" $AMUX_URL/api/schedules | python3 -c "import json,sys; json.load(sys.stdin); print('schedules ok')"

# 9. CRM API
curl -sk -H "$AMUX_AUTH" $AMUX_URL/api/crm/contacts | python3 -c "import json,sys; json.load(sys.stdin); print('crm ok')"

# 10. Email inbox API
curl -sk -H "$AMUX_AUTH" "$AMUX_URL/api/email/inbox?count=1" | python3 -c "import json,sys; json.load(sys.stdin); print('email ok')"

# 11. Calendar feed
curl -sk -H "$AMUX_AUTH" $AMUX_URL/api/calendar.ics | head -1 | grep -q 'BEGIN:VCALENDAR' && echo "ical ok"
```

Report a summary table: check name, status (pass/fail), and any error details for failures.

## Gotchas

- Always use `curl -sk -H "$AMUX_AUTH"` — self-signed TLS cert.
- The SSE check may hang if the server is down — the `timeout` command handles this.
- A syntax check pass does not mean the server reloaded — it auto-restarts on file save, but if the save was recent, give it 1-2 seconds.
