# Drain-restart (`amux-drain-restart.py`)

A standalone, stdlib-only sidecar for lossless fleet/session restarts. Talks
to the running amux server purely over its localhost HTTPS API (see
[`.claude/rules/extend-via-sidecar.md`](../.claude/rules/extend-via-sidecar.md))
plus read-only `ps`/`tmux`/`pgrep` introspection — it makes **zero** changes
to `amux-server.py`.

## The incident this fixes

A hard-kill-based restart lost 7 sessions' conversations in one pass —
each came back with `resumed: false` because Claude Code never got the
chance to run its rename/close dance before the process was signalled.
The existing `amux restart <session>` CLI command is not much safer: it
does `POST /stop`, a **fixed 1-second sleep**, then `POST /start` — no
verification that the process actually finished exiting, and no protection
against restarting a session mid-turn.

A **graceful** stop of an already-idle session is lossless in amux today:
`stop_session()` renames the Claude session to the amux session name (so a
resume can find it), then sends `/exit`. The only missing piece was
sequencing — never sending that stop until the session has genuinely
finished its in-flight turn, and never calling `/start` before the previous
process has actually exited.

## What it does

For each target session:

1. **Drain** — poll `GET /api/sessions` and wait for a **stable** idle: two
   consecutive polls (~3s apart) both reporting `status: "idle"`. A single
   idle reading is not enough (the status detector can catch Claude between
   spinner frames); flapping `active`/`idle`/`active` never counts.
2. **Snapshot** — capture any unsubmitted composer text via `GET .../peek`
   (a line starting with `❯ ` followed by non-empty text) so nothing typed
   but not sent is silently lost.
3. **Stop** — `POST /api/sessions/<name>/stop` (the existing rename+`/exit`
   dance).
4. **Verify exit** — wait (bounded to ~30s) for the session's real `claude`
   process to disappear, checked via system introspection (the API's
   `status` field cannot tell "Claude idle" apart from "shell after /exit" —
   only watching the actual process proves it exited). If the process
   survives the full 30s, the session is marked a **stop-failed straggler**
   and is **never killed** — it needs a human look.
5. **Start** — `POST /api/sessions/<name>/start`, recording whether Claude
   resumed the prior conversation (the API's `resumed` field).

Sessions restart **as soon as they individually drain** — a slow session
never blocks a fast one (rolling restart), and the report streams per-session
as each one completes.

### Stragglers — never touched further

- **Timeout**: a session that never reaches a stable idle within
  `--timeout-mins` is left completely alone and listed with its last known
  status. There is no `--force` kill option, deliberately — a hard kill
  stays a manual, per-session human decision.
- **Stop-failed**: a session whose process is still alive 30s after a
  graceful stop was requested. Also never killed by this tool.

### The "dead" straggler class

An **empty** `status` field means Claude isn't running at all in that
session (crashed, or the tmux pane's shell is sitting idle with no Claude
child). There's no in-flight turn to lose, so these are treated as
immediately drained and revived via the normal stop→start cycle — this is
also how today's crash-recovery already works, we're just making the
sidecar do it for the whole fleet in one pass instead of one-by-one by hand.

## Usage

```bash
# Preview only — one status poll, zero POST requests, exits 0
python3 amux-drain-restart.py all --dry-run

# Fleet-wide, with interactive confirmation
python3 amux-drain-restart.py all

# Specific sessions, no confirmation prompt, 5-minute drain bound
python3 amux-drain-restart.py worker-1 worker-2 --timeout-mins 5 --yes
```

CLI: `amux-drain-restart.py [session ...|all] [--timeout-mins N] [--dry-run] [--yes]`

- `session ...` — one or more explicit session names, **or** the single
  literal `all` (every currently-*running* session).
- `--timeout-mins` (default `15`) — how long to wait for a session to drain
  before giving up on it as a timeout straggler.
- `--dry-run` — print the plan and a single status classification per
  target; makes **no** POST requests at all (verified: the dry-run code path
  never even calls into the stop/start client methods).
- `--yes` — skip the interactive confirmation prompt (needed for
  non-interactive/cron use — still get owner approval out-of-band first).

Auth: reads (`GET /api/sessions`, peek) are unauthenticated on localhost, per
amux's write-auth model; writes (`POST .../stop`, `.../start`) carry
`X-Amux-Write-Token` read from `~/.amux/write_token`.

## When to use this

**Always, instead of a hard kill**, and only after the fleet owner has
explicitly approved the restart window. This tool refuses to guess consent
for you: unless `--yes` is passed, it prints the plan and blocks on an
interactive confirmation before touching anything.

## Exit codes

- `0` — every target restarted successfully with `resumed: true`.
- `1` — the user declined the confirmation prompt (aborted).
- `2` — the server was unreachable, an unknown session name was given, or
  the final report contains any straggler / failed restart / `resumed:
  false`.

## Testing

`tests/test_drain_restart.py` covers the pure logic (drain-stability
tracking, the rolling restart state machine, straggler classification,
composer-snapshot extraction, and self-match-safe process matching) against
mocked HTTP + process layers — no live server, no real `ps`/`tmux`/`pgrep`,
no real sleeping. Run directly:

```bash
python3 tests/test_drain_restart.py
```
