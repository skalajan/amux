# Reply summary marker (amux chat integration)

**Status:** IMPLEMENTED, 2026-07-29. Extends the session-chat core
([`session-chat.md`](session-chat.md)) and the Telegram sidecar
([`telegram-chat.md`](telegram-chat.md)) with a short, human-produced (or, failing
that, AI-produced) summary of a long session reply, so both the dashboard chat tab
and Telegram can show a one-line collapsed bubble instead of a wall of text.

---

## 1. The convention (main-model side)

The marker is a global Claude Code convention, not amux-specific — it lives in
`~/.claude/common.md` so every session (amux or not) that ends a substantive reply
emits it. Verbatim:

```markdown
## Reply Summary Marker (amux chat integration)
- When a substantive reply exceeds a few paragraphs (~10+ lines), end it with a
  final standalone line: `⌁ <one sentence: the outcome + any blocker/question
  needing me>`, written in the conversation's language.
- One sentence only, no markdown, no code. Skip the marker for short replies,
  pure questions, or trivial confirmations.
- Why: amux's chat layer parses this line into a short summary shown in the
  dashboard chat tab and Telegram (full text stays available on expand /last).
  Emit it even outside amux sessions — elsewhere it reads as a harmless TL;DR.
```

Deploying this to another machine (e.g. a mac-server host) is exactly: copy that
block into that machine's `~/.claude/common.md` (or equivalent global instructions
file) verbatim.

Why a model-authored marker rather than always summarizing server-side: the model
that just did the work has the full session context (what was attempted, what's
risky, what still needs a decision) that a detached one-shot summarizer call never
sees. It's also free — no extra `claude -p` subprocess.

---

## 2. Parser contract

One parser, `_chat_parse_summary_marker(text)` in `amux-server.py` (session-chat
sentinel fence), implements the contract — pure and deterministic so a
`chat_replies` rebuild (`DELETE FROM chat_replies` + replay) re-derives the same
summary from the same transcript text every time:

- **Marker = the LAST non-empty line of the reply, if and only if it starts with
  the glyph `⌁`.** Any amount of whitespace after the glyph is accepted and
  stripped — `⌁ text`, `⌁text`, `⌁   text` all parse to the same summary.
- The matched line is **removed** from the stored/returned reply text (the marker
  never leaks into the chat bubble's full-text view). If removing it would leave
  nothing (the reply was only the marker line), the text is left untouched instead
  of being stored empty — the summary is still extracted.
- The summary is capped to **300 characters** (`_CHAT_SUMMARY_MAX_CHARS`).
- No marker on the last line → `summary = None`, text unchanged.

This runs once, at capture time, inside `_chat_extract_turns` (the same pure
projection that turns JSONL rows into `chat_replies` rows) — never re-parsed
per-read, so `GET /api/chat` just returns the stored `summary` column.

---

## 3. Degradation chain (consensus design)

Three tiers, each a fallback for the one above, so a summary is "best available"
rather than "all or nothing":

1. **Marker** (§2) — the main model's own one-sentence summary, parsed at capture
   time. Zero cost, full context, but the model has to remember to emit it.
2. **Server Haiku worker** — a background daemon (`_chat_summary_worker_loop`,
   session-chat sentinel) that finds `chat_replies` rows with `summary IS NULL`
   and `length(text) > 600` (not stale — under `AMUX_SUMMARY_*`-governed age), and
   asks a cheap model (`claude -p --model haiku` by default) to produce one. Covers
   replies where the model didn't self-report a marker. Single-flight (one
   subprocess at a time, throttled by its own sleep loop) and never blocks capture
   or delivery: on ANY failure (timeout, missing binary, non-zero exit, empty
   output) the row's `summary` simply stays `NULL` and the in-memory
   `_chat_summary_failed` backoff avoids hammering the same row every tick.
3. **Deterministic truncation** — if neither of the above produced a summary, both
   consumers (§4) fall back to a client-side / sidecar-side truncation of the raw
   text. No AI call, no network, always available — this tier is what guarantees a
   *usable* short view even with the summarizer fully disabled
   (`AMUX_SUMMARY_DISABLE=1`) or unreachable.

Server env vars (read once at startup, `~/.amux/server.env`-overridable like the
rest of the server's config):

| Variable | Default | Purpose |
|---|---|---|
| `AMUX_SUMMARY_MODEL` | `haiku` | Model passed to the background summarizer's `claude -p` |
| `AMUX_SUMMARY_TIMEOUT` | `90` | Seconds before the summarizer subprocess is killed |
| `AMUX_SUMMARY_DISABLE` | (unset) | Set to `1` to disable the background worker entirely (tier 2 off; tiers 1 and 3 unaffected) |

---

## 4. Which surfaces consume it

- **Dashboard chat tab (`chat.js`)** — ⚠ **RETIRED 2026-08-17** (Rust cutover). The Rust
  dashboard does not load `chat.js`; Telegram is the sole front-end. Described here for
  the historical record only. It rendered: a session bubble with `summary` (or without
  one but longer than 600 chars) renders collapsed by default: the summary (or a
  truncated preview) plus a "zobrazit vše" expand affordance revealing the full
  text already in the payload. Expand state is per-message and in-memory only
  (cleared when the tab is reopened or the session switched) — nothing is
  persisted. Because the background Haiku worker can fill in a summary for a row
  that was already delivered to an open tab, its SSE notification uses a distinct
  `chat` event kind (`"summary"`, vs. `"reply"`/`"owner"`) that the client
  recognizes as "this rowid_seq's *content* changed, not just new rows" and
  triggers a full re-fetch (cursor reset to 0) so the bubble updates in place
  instead of being silently missed by the normal incremental `since=` poll.
- **Telegram sidecar (`amux-telegram.py`), smart mode** — `_render_outbound`
  prefers a reply item's server-provided `summary` when present (prefixed `≡ ` and
  suffixed with the `/last` hint, same as its own local summarizer's output) and
  **skips the local `claude -p` call** entirely for that reply. The sidecar's own
  `Summarizer` (see its "smart-mode summarizer" section) remains the fallback for
  replies the server hasn't summarized (marker absent, background worker hasn't
  gotten to it yet, or `AMUX_SUMMARY_DISABLE=1`) — brief/full modes and `/last`
  (always the full, unsummarized text) are unaffected.
