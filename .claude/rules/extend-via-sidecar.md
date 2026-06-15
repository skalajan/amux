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
   — see the worked example below.

**Worked example — the session chat tab** ([`../../docs/session-chat.md`](../../docs/session-chat.md)):
a crucial, first-class dashboard tab that a sidecar/iframe couldn't deliver
cleanly. Resolved the clause-3 way: feature code in `chat.js` / `chat.css` /
`amux_chat.py` (upstream has none → conflict-immune), with a **~20-line inline
footprint** in marked `# ── session-chat ──` fences and the Python module wired
via `amux_chat.init(ctx)` dependency injection. This is the template for any
future "must be in-file" feature: split first, inline minimally.

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

Related: [single-file.md](single-file.md) governs not splitting `amux-server.py`
into modules; this rule governs not modifying it at all when a sidecar will do.
