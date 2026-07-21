Keep `amux-server.py` as change-free as possible. This fork tracks upstream
(`mixpeek/amux`), and the in-app auto-updater (`_auto_update_check`) **overwrites
the entire `amux-server.py`** from upstream's raw GitHub copy — so any local
edit to that file is either wiped on update or causes merge conflicts in a
~40k-line file.

Therefore, before adding a feature, prefer this order:

1. **Sidecar process / standalone script** — a separate file (e.g.
   `amux-telegram.py`) that talks to the running server over its localhost HTTP
   API + SSE stream. Localhost bypasses auth (`_check_auth`), so no token is
   needed. This is the default for any new integration.
2. **External addon file** — a separate `.py`/script the server is *configured*
   to invoke, never inlined into `amux-server.py`.
3. **In-file change** — only when (1) and (2) genuinely can't work. If you must
   touch `amux-server.py`, keep the change minimal and generic enough to upstream
   (a reusable endpoint/event), not a personal one-off. When a feature must be
   first-class UI (a real dashboard tab) yet a sidecar/iframe won't do, still
   **split the bulk into referenced files** and keep only a tiny inline footprint
   — see "When an in-file change is unavoidable" below for the required checklist.

**Planned template — the session chat tab** ([`../../docs/session-chat.md`](../../docs/session-chat.md)):
a crucial, first-class dashboard tab that a sidecar/iframe couldn't deliver
cleanly. This is documented as a **Phase 0 design/audit only — it is NOT yet
implemented.** `chat.js`, `chat.css`, and `amux_chat.py` **do not exist** in
this repo; agents must not expect to find or edit them. The design resolves
the clause-3 way: feature code would live in those referenced files (upstream
has none → conflict-immune), with a planned ~20-line inline footprint
sentinel-marked per the convention below (`# AMUX-LOCAL:session-chat` …
`# /AMUX-LOCAL:session-chat`, never `# ── … ──`) and the Python module wired
via `amux_chat.init(ctx)` dependency injection. Treat this as the template for
any future "must be in-file" feature — split first, inline minimally, sentinel
it — once/if it is actually built.

Why: upstream tracks only `amux-server.py` and `skills/` — new files under
`docs/`, `.claude/`, or a sidecar at repo root are never touched by the updater
and apply cleanly on `git pull --ff-only`. Functionality belongs beside the app,
not inside its single tracked file.

The existing HTTP/SSE surface a sidecar can use without any server change:
- `GET /api/sessions` — list + status + last-output preview
- `POST /api/sessions/<slot>/send` — inject text into a session (`send_text`)
- `POST /api/sessions/<slot>/stop` · `/start`
- `GET /api/events` — SSE stream of status transitions + alerts

If a sidecar needs something the API lacks, add one small *generic* endpoint
upstream rather than a feature-specific block.

## When an in-file change is unavoidable

If clauses 1–2 genuinely don't apply, an in-file edit to `amux-server.py` or
`amux` must satisfy **all four** of these post-conditions, in the **same
commit** as the code change:

1. **Sentinel-wrap it.** Wrap the footprint in
   `# AMUX-LOCAL:<feature>` … `# /AMUX-LOCAL:<feature>` sentinel comments.
   **Never** use the house style `# ── … ──` — that's upstream's own divider
   convention (113 occurrences in `amux-server.py` today) and collides with it.
2. **Register it.** Add or update the delta's row in the
   [Local Delta Registry](../../MODIFICATIONS.md#local-delta-registry) —
   unique-to-local grep landmarks + a reapply-hunk anchor — in the same commit.
   A delta with no registry row isn't real.
3. **Put the resolution note in the registry row, not here.** The registry is
   the **only** home for per-area "keep local behavior, graft upstream around
   it" notes; don't duplicate them in `upstream-sync.md` or this file.
4. **Assess upstreamability.** Mark the registry row's `Upstreamable?` column;
   if the change is generic enough to be useful upstream, consider filing a PR
   to `mixpeek/amux` instead of carrying it as a permanent delta — the smallest
   durable delta is the one that no longer exists.

**Lazy retrofit policy:** existing deltas are not retroactively sentinel-wrapped
just to satisfy this checklist. Only `amux-server.py`'s account-routing block
(`# AMUX-LOCAL:account-routing`) has been retrofitted, as the dogfood anchor
for the registry, the pre-merge gate, and the advisory hook. Every other
existing delta stays tracked by its unique-to-local grep landmarks in the
registry alone, and only gets the sentinel wrap the next time it's genuinely
touched — don't go retrofit them preemptively.

**Commit conventions:**
- One commit per completed task; single-line commit message, no body, no
  trailers, no co-author line.
- `AMUX_COMMIT_STAMP=0` for merge commits (see `upstream-sync.md` step 6).
- Never edit `amux-server.py` in place during a merge — the live server
  watches its mtime and re-execs on change; conflict markers would break it.
  Merge in a temp worktree instead (`upstream-sync.md` step 3).

**Verification (run after any in-file/CLI change, before committing):**
```bash
python3 -c "import ast; ast.parse(open('amux-server.py').read())"
bash -n amux
curl -sk -o /dev/null -w "%{http_code}" https://localhost:8822/api/sessions   # expect 200
```

**`AMUX_AUTO_UPDATE_REPO` guardrail:** keep this env var **unset** on this fork
host, or set it to `origin` (`skalajan/amux`) — **never** `upstream`
(`mixpeek/amux`). `_auto_update_check` overwrites `amux-server.py` wholesale
from whatever repo this var points at; pointing it at `upstream` would let the
in-app auto-updater self-clobber every local delta mid-session, bypassing the
registry and the pre-merge gate entirely. (Also documented in
`docs/upstream-sync.md` Notes.)

Related: [single-file.md](single-file.md) governs not splitting `amux-server.py`
into modules; this rule governs not modifying it at all when a sidecar will do.
