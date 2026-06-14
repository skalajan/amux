# Telegram ↔ amux terminal bridge — architecture

**Status:** design, pending implementation approval.
**Form factor:** standalone sidecar (`amux-telegram.py`), **zero changes to
`amux-server.py`**. See [`.claude/rules/extend-via-sidecar.md`](../.claude/rules/extend-via-sidecar.md)
for why this is a separate process rather than in-file code.

## Goal

Drive amux-managed Claude Code sessions from Telegram: send prompts/keys from a
phone into a session's terminal, and receive a message when a session needs
input or finishes a turn.

## Why a sidecar (not in-file)

This fork tracks upstream `mixpeek/amux`, and the auto-updater
(`_auto_update_check`) overwrites the entire `amux-server.py` on every upstream
change. In-file Telegram code would be wiped on update or conflict in a ~40k-line
file. A separate script talking to the server's localhost API avoids both —
upstream never touches files outside `amux-server.py` / `skills/`.

## Transport: long polling (not webhooks)

- A `getUpdates` long-poll loop (`timeout≈30s`, offset-tracked). Outbound only:
  no public URL, no inbound HTTP route, NAT-friendly, identical local/cloud.
- Webhooks rejected: they need a public HTTPS endpoint (differs local vs cloud)
  plus an unauthenticated inbound route into terminals — wrong place to relax
  auth, and a single-codebase violation if it were ever in-file.

## Integration surface (existing API, no server change)

Localhost bypasses auth (`_check_auth`), so the sidecar needs no token.

| Need | Endpoint |
|---|---|
| Inject text → terminal | `POST /api/sessions/<slot>/send` (the `send_text` path) |
| List sessions + status (`/ls`, `/peek`) | `GET /api/sessions` |
| Outbound idle/needs-input events | `GET /api/events` (SSE — already emits status transitions + alerts) |

Using the SSE stream for outbound means **no custom status poller** — the server
already broadcasts `active/waiting → idle` transitions.

## Inbound (Telegram → terminal)

- Allowlist `chat.id` against `AMUX_TELEGRAM_CHAT_ID`; reject + log everything
  else (this is the RCE gate).
- Routing = **bound-session-per-chat + reply-to override**:
  - plain text → `POST /api/sessions/<bound>/send`
  - `/ls` → reply with `GET /api/sessions`
  - `/use <slot>` → bind this chat to a slot (persisted)
  - `/peek [n]` → reply with the session's recent output
  - reply-to a session's outbound message → route to that session (overrides bind)
- Chosen over forum-topics-per-session (setup friction) and stateless
  `/send <slot> <text>` (verbose for conversation).

## Outbound (terminal → Telegram)

- Subscribe to `GET /api/events`; on `active/waiting → idle` (and on `waiting`,
  for permission prompts) send the slot name + last intelligible output line.
- Chunk messages at Telegram's 4096-char cap; serialize per-chat sends
  (~1 msg/s) with a lock; debounce status flapping.

## State — `~/.amux/telegram-state.json`

```json
{ "offset": 0, "bindings": { "<chat_id>": "<slot>" } }
```

- `offset` written after each successful `getUpdates` ack → survives sidecar
  restarts and prevents replay of un-acked commands (**replay = RCE replay**).
- **Cold start with no stored offset: discard the backlog** (set offset past the
  latest update without executing) so messages predating the feature never run.

## Config

| Var | Meaning |
|---|---|
| `AMUX_TELEGRAM_BOT_TOKEN` | Bot token (from @BotFather). Unset → sidecar dormant. |
| `AMUX_TELEGRAM_CHAT_ID` | Allowlisted chat id(s). Reject all others. |
| `AMUX_URL` | amux base URL, default `https://localhost:8822` (self-signed → unverified TLS context). |

Pure `urllib` — no pip dependencies. Run as its own amux session (dogfooding) or
a launchd unit.

## Security model

The bridge is a **remote shell into `--yolo` terminals**. Mitigations:
- chat-id allowlist; off by default (no token = no process);
- persisted offset + backlog-discard on cold start;
- never echo secrets;
- optional conservative mode: restrict v1 to explicit `/`-commands (no free-text
  injection) — **decision pending**.

Cloud multi-tenant (one shared bot bridging many gateway users) is **out of scope
for v1**; the feature is simply dormant on cloud because the env var isn't set
(env-gated, no `if IS_CLOUD`). A future cloud design would key bindings by
`X-Amux-User-Email` and require per-org tokens.

## Acceptance criteria

- `git diff amux-server.py` is empty (zero server changes).
- Sidecar dormant without `AMUX_TELEGRAM_BOT_TOKEN`.
- Message from a non-allowlisted chat is rejected and logged; no send occurs.
- Plain text from a bound chat reaches the correct session.
- A session going `active → idle` reaches Telegram in ~1–2s via SSE.
- Sidecar restart does not replay already-processed commands (offset persisted).
- Messages > 4096 chars are chunked; rapid flapping does not spam.

## ADR

- **Decision:** standalone `amux-telegram.py` sidecar over localhost HTTP+SSE;
  long-poll transport; state in `~/.amux/telegram-state.json`; env-gated; v1 local.
- **Drivers:** keep `amux-server.py` change-free for upstream tracking; low-latency
  input alerts; restart-safe replay protection.
- **Alternatives:** in-file code (rejected — wiped by auto-updater); webhook
  (rejected — public-URL + auth-bypass); forum-topics / stateless `/send`
  (deferred).
- **Consequences:** a second process to supervise; bounded by what the HTTP/SSE
  API exposes (covered for v1).
- **Follow-ups:** `/keys` raw-key support if/when an endpoint exists; `/follow`
  verbose streaming; forum-topics-per-session; cloud per-org tokens.
