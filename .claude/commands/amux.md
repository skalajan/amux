---
description: Interact with amux — the shared board, other sessions, messages, and schedules. Use when the user says "add to the board", "ask another session", "what are my sessions doing", or wants to schedule recurring work.
allowed-tools: Bash, Read
argument-hint: [board|sessions|send|schedules|help] [args...]
---

# /amux — drive the amux system

You are running inside an amux-managed session. **Run this setup line first** — every
example below assumes `$U` and `$A`:

```bash
U="$(amux url)"; A="Authorization: Bearer $(cat ~/.amux/auth_token)"
```

`amux url` self-heals past a retired port (it reads `~/.amux/endpoint.json`, which the
server rewrites every boot) — **never hardcode a port.** The API requires auth: an
unauthenticated request returns `401`. TLS is self-signed, so always `curl -sk`.

**`$AMUX_URL` and `$AMUX_SESSION` are NOT set inside sessions** — verified from a live
session, both unset. Don't use them; derive instead. For your own session name use
`amux whoami` (it says when it can't tell). Do **not** derive it from
`tmux display-message` — with no attached client that returns whatever session tmux
considers current, which is usually a different one.

## Board (shared kanban)

```bash
curl -sk -H "$A" "$U/api/board" | python3 -m json.tool                      # list
curl -sk -H "$A" -X POST -H 'Content-Type: application/json' \
  -d '{"title":"...","status":"todo","session":"YOUR-SESSION"}' "$U/api/board"   # add
curl -sk -H "$A" -X DELETE "$U/api/board/ITEM-ID"                           # delete
amux board doing ITEM-ID   # or: done / todo / backlog (CLI shorthand, handles auth itself)
```
Keep exactly one item in `doing` while you work; mark it `done` with a result note when finished.

## Sessions (the rest of the fleet)

```bash
curl -sk -H "$A" "$U/api/sessions" | python3 -c "import json,sys; [print(s['name'], s.get('status','')) for s in json.load(sys.stdin)]"
curl -sk -H "$A" "$U/api/sessions/OTHER/peek?lines=100"          # see what another session is doing
amux send OTHER "message"                                        # message it (origin-stamped — prefer over raw curl)
```

## Schedules (recurring prompts)

```bash
curl -sk -H "$A" -X POST -H 'Content-Type: application/json' \
  -d '{"title":"...","session":"YOUR-SESSION","command":"the prompt","schedule_expr":"daily at 9am"}' \
  "$U/api/schedules"
```
Expressions: `daily at HH:MM`, `every 15m`, `every weekday at HH:MM`, or 5-field cron.

Run whatever the arguments ask for. No arguments → show the board and the session list.
