# Session chat tab — phone-first dev/research bridge

**Status:** ⚠ **B2 (dashboard tab) RETIRED 2026-08-17** at the Rust cutover — the Rust
dashboard contains no `chat.js` reference and Telegram is the sole front-end. **B1 (chat
core) LIVES ON** in the standalone sidecar `amux-chat.py`, which serves `GET/POST /api/chat`
on port 8825 and is what `amux-telegram.py` reads. `chat.js`/`chat.css` remain in the repo
only as part of the retained rollback path alongside the frozen `amux-server.py`; nothing
loads them. Everything below describes the tab **as it was built** — read it as history,
not as a description of a running feature.

**Original status:** IMPLEMENTED (B1 chat core + B2 dashboard tab), 2026-07-22. Shipped per
the consensus plan [`.omc/plans/chat-layer-auth.md`](../.omc/plans/chat-layer-auth.md)
(§4 B1, §5 B2-β) on top of Scope A localhost write-auth. Feature code lives in the
referenced files `chat.js` / `chat.css` (loaded by the dashboard) plus the Python
chat core in `amux-server.py`; the in-file footprint is a tiny sentinel-fenced set of
hooks (link/script tags, a `Chat` peek-overlay tab + `#peek-chat-panel`, a
`/chat.js`+`/chat.css` static route, `setPeekTab`/`closePeek` dispatch lines,
`_PUBLIC_PATHS` entries). B3 (Telegram sidecar) is not yet built.

> **Fence-style supersession (binding):** the `# ── session-chat ──` fence style
> shown throughout this doc is **superseded** by the fork-governance rules
> ([`single-file.md`](../.claude/rules/single-file.md),
> [`extend-via-sidecar.md`](../.claude/rules/extend-via-sidecar.md)) — the shipped
> code uses the `# AMUX-LOCAL:session-chat` … `# /AMUX-LOCAL:session-chat` sentinel
> (Python `#`, JS `//`, SQL `--`, HTML `<!-- -->`), tracked by a Local Delta Registry
> row in [`MODIFICATIONS.md`](../MODIFICATIONS.md). The DI-seam / `amux_chat.py`
> module described below was **not** used: the B1 chat core landed inline in
> `amux-server.py` (sentinel-fenced), and B2 added only `chat.js`/`chat.css`. The rest
> of this document is retained as the original design record.
**Goal:** develop/research from the phone without losing terminal power — a
chat-style view of each session with structured, short messages by default and
the full formatted output on request, all inside the amux app.

**Form factor:** a new **chat tab in the dashboard**, alongside the existing
terminal tab, built as **referenced files** (`chat.js` / `chat.css` /
`amux_chat.py`) loaded by the dashboard, with a **~20-line inline footprint** in
`amux-server.py` (§2.4). This is a deliberate exception to
[`extend-via-sidecar.md`](../.claude/rules/extend-via-sidecar.md) (clause-3
last-resort) and to [`single-file.md`](../.claude/rules/single-file.md): the
feature is crucial and must be first-class, which a sidecar/iframe can't deliver
as cleanly — but it's split into files to keep upstream merges clean (§7).

---

## 1. Locked decisions

| Decision | | Consequence |
|---|---|---|
| Scope | **Work *and* personal** sessions | Content must stay self-hosted |
| Functions | **All** — free-text prompts + every command | Full control from the phone |
| Messages | **Short by default, full formatted on request — all in-app, no links** | Self-hosted transport required |
| Transport | **Custom amux chat tab**, **Telegram dropped** | Data never leaves your infra; security collapses (§6) |
| Code layout | **Split into referenced files** (`chat.js`/`chat.css`/`amux_chat.py`); ~20-line inline footprint | Minimizes upstream-merge surface (§2.4, §7) |
| Deployment | **Local/personal only for now** — not shipped to cloud | Cloud deploy pipeline untouched; revisit later |

**Why these are coherent:** "work code + full content in the messenger" would be
an exfiltration problem on Telegram (non-E2E, third-party storage). Making the
*messenger* your own amux app — served over Tailscale, self-hosted — keeps
everything on your infrastructure. The Telegram security apparatus (bot token,
allowlist, replay, privacy mode) is no longer needed at all.

---

## 2. Architecture

Two layers, with a clean internal seam so an external adapter (Telegram/Slack)
stays *possible* later without a rewrite:

- **Engine (`amux_chat.py`):** produces a structured conversation/event view of a
  session and consumes input commands. Reuses what already exists.
- **Chat tab (`chat.js`/`chat.css`):** renders that view as a chat alongside the
  terminal tab, phone-optimized (the dashboard is already a PWA).

### 2.1 Engine — reuse, don't rebuild
The server already does most of this; `amux_chat.py` calls these via the DI seam
(§2.4):
- **Structured conversation source:** Claude transcript JSONL is already parsed in
  many places (`_read_jsonl_tail:2286`, conversation handling ~`2970–3011`,
  `list_session_transcripts:1712`, `backup_session_jsonl:1671`). **First step:
  audit and extend the existing transcript/conversation endpoints** rather than
  writing new parsing.
- **Status/events:** `_detect_claude_status:5584` + the SSE stream
  (`GET /api/events`) already broadcast `active`/`waiting`/`idle`.
- **Input consumer:** `send_text:7865` (via `POST /api/sessions/<slot>/send`);
  raw-key path for control sequences (`C-c`, …).
- **Raw terminal source:** `tmux_capture:1449` + pipe-pane logs
  (`_attach_log_streaming:6498`).

New backend surface — lives in `amux_chat.py`, reached through one dispatch hook:
- `GET /api/sessions/<slot>/chat` — structured turns (user/assistant/tool blocks)
  from JSONL; **short summary per turn + a handle to expand full content**.
- chat deltas over the **existing SSE** (`/api/events`) — add a `chat` event type
  rather than a new stream (respects [`sse-realtime.md`](../.claude/rules/sse-realtime.md):
  update the polling fallback too).
- input reuses the existing `/send` + raw-key endpoints — no new injector.

### 2.2 Two source tiers
- **Structured tier → the chat messages.** JSONL turns as chat bubbles; status as
  inline state markers. The "short, readable" default — *not* TUI scraping.
- **Raw tier → "full terminal output on request."** Tap a turn → full formatted
  content (markdown + tool blocks); a "raw terminal" affordance shows the literal
  `tmux_capture`/pipe-pane output for that span.

### 2.3 Liveness tier (responsiveness)

The structured tier alone is **not** live: transcript JSONL is written **per
completed turn**, not token-by-token. Between turns (a long generation or a
long-running tool) the JSONL is silent — the chat would look frozen. A separate,
fast liveness signal is required, independent of the 60 s snapshot loop:

- **Primary "is it producing" signal = pipe-pane log growth.** The session log
  (`_attach_log_streaming:6498`) grows in real time as Claude emits output;
  watching its size/mtime (~1 s) is a near-instant "working" signal, zero TUI
  parsing.
- **State distinctions = `_detect_claude_status`** for `waiting` (permission
  prompt) vs `idle` (done).
- **Optional live tail:** the last N raw-log lines as transient "partial output"
  (later phase; mostly not needed mid-run).

Per-session liveness state machine:

| State | Signal | Chat shows |
|---|---|---|
| producing / thinking | log growing, or status `active` | "⚙️ working…" (+ optional live tail) |
| waiting | status `waiting` | "⏸ needs your input" + approve/deny |
| ready | status `idle`, log quiet | no indicator; composer ready |

**Two-layer rendering:** committed turns (JSONL) are the permanent chat spine; the
liveness indicator is a transient element at the bottom, replaced by the next
committed turn when it lands — a streaming-chat pattern approximated without a
token stream.

### 2.4 Code layout — split into referenced files (minimize merge surface)

The feature lives in **separate files**, not inline:

| File | Contents | Upstream conflict risk |
|---|---|---|
| `chat.js` | the whole tab UI (render, SSE, liveness, diff view, mobile) | none — upstream has no such file |
| `chat.css` | chat-tab styles | none |
| `amux_chat.py` | engine + endpoint handlers | none |

**Inline footprint contract — `amux-server.py` changes capped at ~20 lines**, all
in marked `# ── session-chat ──` fences, in stable / rarely-touched spots:
1. `<script src="/chat.js">` + `<link href="/chat.css">` in `DASHBOARD_HTML`
   (idiomatic — external `<script src>` is already used for CDN libs).
2. one tab-registry entry `{ id:'chat', label:'Chat' }` + an empty
   `<div id="chat-view">`.
3. a static-asset route serving `chat.js`/`chat.css` from `Path(__file__).parent`
   (mirrors the existing `sw.js`/icon routes; precedent — the server already
   reads a sibling `templates/` dir, `TEMPLATES_DIR:73`).
4. one dispatch line in `_route_inner` → `amux_chat`.
5. (optional) add the two assets to `SHELL_URLS` for PWA offline caching.

**Dependency-injection seam:** `amux-server.py`'s hyphenated name means
`amux_chat.py` cannot `import` it back. So at startup `amux-server.py` calls
`amux_chat.init(ctx)`, passing the helpers it needs (`send_text`, `tmux_capture`,
`_detect_claude_status`, `_read_jsonl_tail`, `CC_SESSIONS`, …). This doubles as
the clean engine seam that keeps a future external adapter possible.

This is a sanctioned exception, carved out in
[`single-file.md`](../.claude/rules/single-file.md) and templated in
[`extend-via-sidecar.md`](../.claude/rules/extend-via-sidecar.md).

---

## 3. Chat-tab UX

Tuned to the real workflow: **many short progress messages during development
(full output not wanted), then a final message with explanation + code diffs** —
and the chat must feel responsive throughout.

- **Tab placement:** new "Chat" tab beside "Terminal", scoped to the selected
  session (the dashboard already has session selection + per-session terminal —
  chat follows the same selection). Multi-session via existing navigation; no
  per-session channels.
- **Responsiveness = two things:** (1) short turn-summary bubbles commit as each
  turn completes (JSONL), so progress visibly accrues; (2) the liveness indicator
  (§2.3) fills the gaps so it never looks frozen. The user's own message is echoed
  **optimistically** the instant they send it.
- **Short by default:** each turn is a collapsed one/two-line summary + state
  (assistant → first line; `tool_use` → `🔧 Edit · auth.py`; `tool_result` →
  `✓`/`✗` + line count). The stream skimmed during development.
- **End-of-feature payload — a first-class diff view.** The final turn shows the
  explanation (markdown); a prominent **"Changes" affordance** renders the session
  repo's real **`git diff`** — working tree **plus** recent session commits (Claude
  commits per task via the stamping hook) — formatted, in-app. The message the
  user actually reads.
- **Full on request:** tap any turn → full formatted content (markdown, code
  blocks, tool input/output); "raw terminal" toggle for literal output. No links,
  no downloads.
- **Input:** chat composer → `send_text`. Quick actions for common keys (interrupt
  `C-c`, approve/deny when status reports a permission prompt).
- **Phone-first:** obey [`css-mobile.md`](../.claude/rules/css-mobile.md) — 44px
  touch targets, safe-area insets, 375px width, `viewport-fit=cover`.

---

## 4. Multi-session

No forum-topic / channel machinery (that was a Telegram concern). Each session
already appears in the dashboard's session list; the chat tab renders the selected
session's conversation. Switching sessions switches the chat — same model as the
terminal tab.

---

## 5. Work-code safety

The transport is the self-hosted dashboard over Tailscale, so work (Fidoo) source,
diffs, and logs **never leave your infrastructure** — no third-party servers, no
compliance issue. This is the entire reason the native-app route was chosen over
Telegram for work sessions.

---

## 6. Security model (now minimal)

- The chat tab **inherits the dashboard's existing auth** (`AUTH_TOKEN` / gateway
  `X-Amux-User-Email` / localhost bypass / Tailscale). No new auth system.
- It **adds no new attack surface beyond the existing terminal tab**, which
  already lets an authenticated user send input to any session.
- Still RCE into `--yolo` terminals — but unchanged from today's dashboard;
  mitigation is the same: keep the dashboard behind its auth + Tailscale, not
  public.
- No bot token, no allowlist, no replay/offset concerns — all dropped with
  Telegram.

---

## 7. Managing upstream divergence

Most of the feature lives in separate files (§2.4) that upstream doesn't have, so
they never conflict. Only the ~20-line inline footprint in `amux-server.py` is
merge-exposed; manage it:

1. **Split + localize.** Keep feature code in `chat.js`/`chat.css`/`amux_chat.py`;
   confine the inline footprint to marked `# ── session-chat ──` fences in stable
   spots so `git merge upstream/main` conflicts are rare and tiny.
2. **Auto-updater must target the fork.** `_auto_update_check:38830` **overwrites
   `amux-server.py` wholesale** from `AMUX_AUTO_UPDATE_REPO` (currently disabled).
   If ever enabled, it **must** point at `skalajan/amux`, never `mixpeek/amux` —
   else the next upstream commit wipes the inline hooks. (It never touches
   `chat.js`/`chat.css`/`amux_chat.py`, so those survive regardless.)
3. **Sync upstream manually:** `git fetch upstream && git merge upstream/main`,
   resolving only the fenced regions. `docs/` and `.claude/` stay conflict-free.
4. **Keep the engine/render seam clean** so the feature could be extracted to a
   sidecar later if divergence ever outweighs the integration benefit.

---

## 8. Telegram (dropped, door left open)

Out of scope. The engine still emits a structured event/command model, so a future
Telegram/Slack adapter could subscribe without reworking it — but that would
re-introduce the data-residency problem for work code and would be
**personal-only** if ever added.

---

## 9. Phased plan

**Phase 0 — Audit (no code).** Map existing transcript/conversation parsing and
endpoints. Verify the liveness-critical facts: (a) exactly when Claude writes JSONL
entries (per-turn vs incremental) — decides how much the liveness tier carries;
(b) the slot → live JSONL file + pipe-pane log-file mapping; (c) per-session
`git diff` feasibility from the known cwd; (d) the DI `init(ctx)` seam + the
static-asset serving approach (§2.4); (e) **tool-lifecycle granularity** — whether
the live JSONL (or a Claude Code hook) exposes a distinct *"tool started"* marker
before the matching `tool_result`. If it does, the liveness tier can show a
specific `🔧 running Edit · auth.py…` card instead of a generic "⚙️ working…",
matching OpenClaw's explicit `thinking`/`responding`/`tool-use` lifecycle events
(its Gateway emits these natively; we'd derive them from JSONL/hook markers since
we observe the runtime rather than own it). Decides whether the §2.3 state machine
gains a `tool-running` sub-state. Output: reused functions + minimal new
endpoints + confirmed liveness signal + tool-lifecycle verdict + the exact
inline-footprint list.

**Phase 1 — Read-only chat + liveness (MVP).**
- Create `amux_chat.py` (structured turns from JSONL + a `chat` SSE event +
  polling fallback + a liveness signal from log-growth/status) and the ~20-line
  inline hooks (§2.4).
- In `chat.js`/`chat.css`: Chat tab beside Terminal; collapsed turn summaries; the
  liveness indicator + state machine (§2.3); tap-to-expand full content; follows
  the selected session; mobile-compliant.
- Acceptance: §10 read-path + liveness criteria.

**Phase 2 — Input, diff view, control.**
- Composer → `send_text` with optimistic echo; quick keys (`C-c`); approve/deny on
  `waiting`; the first-class **"Changes" diff view** (`git diff` working tree +
  session commits); per-turn raw-terminal toggle.

**Phase 3 — Polish.**
- Optional live raw tail; unread badges per session; conversation search; optional
  external adapter via the engine seam.

---

## 10. Acceptance criteria

- Chat tab appears beside the terminal tab and follows the selected session.
- Each turn shows a short summary by default; expanding shows the full formatted
  message (markdown + code highlighting + tool blocks) **in-app, no link**.
- **Liveness:** a "working" indicator appears within ~1–2 s of activity (log
  growth) and clears within ~2 s of idle; `waiting` shows "needs input" +
  approve/deny. Never looks frozen during long turns/tools.
- **Optimistic echo:** the user's sent message appears immediately.
- **Diff view:** the "Changes" affordance renders the session's real `git diff`
  (working tree + recent commits), formatted, in-app.
- A "raw terminal" affordance shows literal `tmux_capture`/pipe-pane output.
- Composer input reaches the session (verified via terminal/JSONL); `C-c` and
  approve/deny work.
- Updates arrive live via SSE within ~2 s; polling fallback also fetches chat (per
  `sse-realtime.md`).
- Mobile: usable at 375px, 44px touch targets, safe-area correct.
- **Layout:** feature code lives in `chat.js`/`chat.css`/`amux_chat.py`;
  `amux-server.py` changes are ≤ ~20 lines in marked fences, and
  `git merge upstream/main` conflicts (if any) are limited to those fences.
- `python3 -c "import ast; ast.parse(open('amux-server.py').read())"` passes (and
  `amux_chat.py` parses).

---

## 11. ADR

- **Decision:** build a session **chat tab in the dashboard**, with feature code in
  **referenced files** (`chat.js`/`chat.css`/`amux_chat.py`) and a ~20-line inline
  footprint in `amux-server.py` (DI seam); reuse existing transcript-JSONL parsing
  + status/SSE + `send_text`; structured turns as short messages, expandable to
  full formatted content and raw terminal; **all in-app over Tailscale**;
  **Telegram dropped**; **local/personal only for now**.
- **Drivers:** phone-driven dev for **work and personal** without losing terminal
  power; **work-code data residency** (self-hosted only); a crucial,
  must-be-spot-on feature warranting first-class integration; **clean upstream
  merges** (track `mixpeek/amux`).
- **Alternatives considered:** Telegram bridge (rejected — non-E2E third party,
  unsafe for work code); fully-inline in `amux-server.py` (rejected — large
  merge surface vs. the split); generic iframe/custom-tab hook (rejected — not
  "spot on"); separate sidecar web app (rejected — not integrated alongside the
  terminal tab); terminal scraping for messages (rejected — lossy vs. JSONL).
- **Why chosen:** the split native tab gives first-class integration *and* a small
  merge surface, satisfies work+personal + full-content-in-app, and collapses
  security to "same as the existing terminal tab."
- **Consequences:** small upstream divergence (managed via §7); `single-file.md`
  carve-out in place for `chat.*`/`amux_chat.py`; DI wiring due to the hyphenated filename;
  liveness needs a fast signal (log-growth) separate from the turn-complete JSONL;
  cloud deploy intentionally untouched (local-only).
- **Follow-ups:** ship to cloud later (would require deploy to bundle the 3 files);
  optional external adapter (personal-only) via the engine seam; conversation
  search; streaming token feel.
