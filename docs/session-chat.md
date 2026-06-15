# Session chat tab — phone-first dev/research bridge

**Status:** design, pending implementation approval. No code written yet.
**Goal:** develop/research from the phone without losing terminal power — a
chat-style view of each session with structured, short messages by default and
the full formatted output on request, all inside the amux app.

**Form factor:** a new **chat tab in the dashboard**, alongside the existing
terminal tab, built **into `amux-server.py`**. This is a deliberate exception to
[`extend-via-sidecar.md`](../.claude/rules/extend-via-sidecar.md) (its clause-3
last-resort path): the feature is crucial and must be first-class, which a
sidecar/iframe can't deliver as cleanly. See §7 for managing the resulting
upstream divergence.

---

## 1. Locked decisions

| # | Decision | Consequence |
|---|---|---|
| Scope | **Work *and* personal** sessions | Content must stay self-hosted — drives the transport choice |
| Functions | **All** — free-text prompts + every command | Full control from the phone |
| Messages | **Inline short by default, full formatted on request — all in-app, no links** | Self-hosted transport required (no third party) |
| Transport | **Custom amux chat tab** (native/web app), **Telegram dropped** | Data never leaves your infra; security model collapses (§6) |

**Why these are coherent:** "work code + full content in the messenger" would be
an exfiltration problem on Telegram (non-E2E, third-party storage). Making the
*messenger* your own amux app — served over Tailscale, self-hosted — means
"everything in-app" keeps everything on your infrastructure. The earlier
Telegram security apparatus (bot token, chat-id allowlist, offset replay,
privacy mode) is no longer needed at all.

---

## 2. Architecture

With the UI inline and Telegram gone, the design collapses into two layers
**inside `amux-server.py`**, with a clean internal seam preserved so an external
adapter (Telegram/Slack) stays *possible* later without a rewrite:

- **Engine (server-side):** produces a structured conversation/event view of a
  session and consumes input commands. Reuses what already exists.
- **Chat tab (frontend):** renders that view as a chat, alongside the terminal
  tab, phone-optimized (the dashboard is already a PWA).

### 2.1 Engine — reuse, don't rebuild
The server already does most of this:
- **Structured conversation source:** Claude transcript JSONL is already parsed
  in many places (`_read_jsonl_tail:2286`, conversation-file handling ~`2970–3011`,
  `list_session_transcripts:1712`, `backup_session_jsonl:1671`). **First
  implementation step: audit the existing transcript/conversation endpoints and
  extend them** rather than writing new parsing.
- **Status/events:** `_detect_claude_status:5584` + the SSE stream
  (`GET /api/events`) already broadcast `active`/`waiting`/`idle` transitions.
- **Input consumer:** `send_text:7865` (via `POST /api/sessions/<slot>/send`) for
  prompts; the raw-key path for control sequences (`C-c`, etc.).
- **Raw terminal source (for "full output"):** `tmux_capture:1449` and the
  pipe-pane log files (`_attach_log_streaming:6498`).

New backend surface (kept minimal, in a marked block):
- `GET /api/sessions/<slot>/chat` — structured turns for the chat view
  (user/assistant turns, tool calls, results) parsed from JSONL; **short summary
  per turn + a handle to expand full content**.
- chat deltas pushed over the **existing SSE** (`/api/events`) — add a `chat`
  event type rather than a new stream (respects the SSE rules in
  [`.claude/rules/sse-realtime.md`](../.claude/rules/sse-realtime.md): update the
  polling fallback too).
- input reuses the existing `/send` + raw-key endpoints — no new injector.

### 2.2 Two source tiers
- **Structured tier → the chat messages.** Transcript JSONL turns rendered as
  chat bubbles; status events as inline state markers. This is the "short,
  readable" default — *not* TUI scraping.
- **Raw tier → "full terminal output on request."** Tap a turn → expand to the
  full formatted content (markdown render of the assistant message + tool
  blocks); a further "raw terminal" affordance shows the literal `tmux_capture` /
  pipe-pane output for that span.

---

## 3. Chat-tab UX

- **Tab placement:** new "Chat" tab beside the existing "Terminal" tab, scoped to
  the currently-selected session (the dashboard already has session selection +
  per-session terminal — chat follows the same selection). Multi-session is thus
  handled by the existing session navigation; no per-session channels to manage.
- **Short by default:** each turn is a collapsed one/two-line summary + state.
- **Full on request:** tap to expand → full formatted message (markdown, code
  blocks with highlighting, tool calls/results); optional "raw terminal" toggle
  for the literal output. All rendered in-app — no external links, no downloads
  required.
- **Input:** a chat composer at the bottom → `send_text`. Quick actions for
  common keys (interrupt `C-c`, approve/deny when a permission prompt is detected
  via status).
- **Phone-first:** must obey the mobile CSS rules
  ([`.claude/rules/css-mobile.md`](../.claude/rules/css-mobile.md)) — 44px touch
  targets, safe-area insets, 375px width, `viewport-fit=cover`.

---

## 4. Multi-session

No forum-topic / channel machinery (that was a Telegram concern). Each session
already appears in the dashboard's session list; the chat tab renders the
selected session's conversation. Switching sessions switches the chat — the same
model as the terminal tab.

---

## 5. Work-code safety

Because the transport is the self-hosted dashboard over Tailscale, work (Fidoo)
source, diffs, and logs **never leave your infrastructure** — no third-party
servers, no compliance issue. This is the entire reason the native-app route was
chosen over Telegram for work sessions.

---

## 6. Security model (now minimal)

- The chat tab **inherits the dashboard's existing auth** (`AUTH_TOKEN` / gateway
  `X-Amux-User-Email` / localhost bypass / Tailscale). No new auth system.
- It **adds no new attack surface beyond the existing terminal tab**, which
  already lets an authenticated user send input to any session. Driving a session
  from the chat composer is the same capability the terminal tab already grants.
- It is still RCE into `--yolo` terminals — but that property is unchanged from
  today's dashboard; the mitigation is the same: keep the dashboard behind its
  auth + Tailscale, don't expose it publicly.
- No bot token, no allowlist, no replay/offset concerns — all dropped with
  Telegram.

---

## 7. Managing upstream divergence (the cost of going inline)

The feature lives in `amux-server.py`, which this fork keeps change-free for
upstream tracking. Going inline is a conscious exception; manage it:

1. **Localize the code.** Put the backend block and the frontend tab in clearly
   marked contiguous regions (e.g. `# ── session-chat ──` fences) so
   `git merge upstream/main` conflicts are contained and easy to resolve.
2. **Auto-updater must target the fork.** `_auto_update_check:38830` **overwrites
   `amux-server.py` wholesale** from `AMUX_AUTO_UPDATE_REPO`. It is currently
   disabled (unset). If ever enabled, it **must** point at `skalajan/amux` (the
   fork with this feature), never `mixpeek/amux` — otherwise the next upstream
   commit wipes the chat tab. Document this in `server.env`.
3. **Sync upstream manually:** `git fetch upstream && git merge upstream/main`,
   resolving the marked regions. `docs/` and `.claude/` stay conflict-free.
4. **Keep the internal engine/render seam clean** so the feature could be
   extracted to a sidecar later if the divergence cost ever outweighs the
   integration benefit.

---

## 8. Telegram (dropped, door left open)

Telegram is out of scope. The server-side engine still emits a structured
event/command model, so a future Telegram/Slack adapter could subscribe without
reworking the engine — but it would re-introduce the data-residency problem for work
code and would be **personal-only** if ever added.

---

## 9. Phased plan

**Phase 0 — Audit (no code).** Map the existing transcript/conversation parsing
and endpoints; confirm exactly what the engine can reuse vs. what's new. Output:
a short list of reused functions + the minimal new endpoint(s).

**Phase 1 — Read-only chat view (MVP).**
- Backend: `GET /api/sessions/<slot>/chat` (structured turns from JSONL) + a
  `chat` SSE event type (+ polling fallback).
- Frontend: Chat tab beside Terminal, collapsed summaries, tap-to-expand full
  formatted content, follows selected session. Mobile-compliant.
- Acceptance: §10 read-path criteria.

**Phase 2 — Input + control.**
- Chat composer → `send_text`; quick keys (`C-c`); approve/deny on detected
  permission prompts; raw-terminal toggle per turn.

**Phase 3 — Polish.**
- Live token/streaming feel, unread markers per session, search within a
  conversation, optional external adapter seam if ever wanted.

---

## 10. Acceptance criteria

- Chat tab appears beside the terminal tab and follows the selected session.
- Each turn shows a short summary by default; expanding shows the full formatted
  message (markdown + code highlighting + tool blocks) **in-app, no link**.
- A "raw terminal" affordance shows literal `tmux_capture`/pipe-pane output for a
  turn.
- Composer input reaches the session (verified via terminal/JSONL); `C-c` and
  approve/deny work.
- Updates arrive live via SSE within ~2 s; polling fallback also fetches chat
  (per `sse-realtime.md`).
- Mobile: usable at 375px, 44px touch targets, safe-area correct.
- Chat-tab code is confined to marked regions in `amux-server.py`;
  `git merge upstream/main` conflicts are limited to those regions.
- `python3 -c "import ast; ast.parse(open('amux-server.py').read())"` passes.

---

## 11. ADR

- **Decision:** build a session **chat tab in the dashboard** (inline in
  `amux-server.py`), reusing existing transcript-JSONL parsing + status/SSE +
  `send_text`; structured turns as short messages, expandable to full formatted
  content and raw terminal output; **all in-app over Tailscale**; **Telegram
  dropped**; accept upstream divergence with localized code + fork-targeted
  auto-update.
- **Drivers:** phone-driven dev for **work and personal** without losing terminal
  power; **work-code data residency** (self-hosted only); a crucial,
  must-be-spot-on feature warranting first-class integration.
- **Alternatives considered:** Telegram bridge (rejected — non-E2E third party,
  unsafe for work code with full content); generic iframe/custom-tab hook
  (rejected — not "spot on" enough for a crucial feature); separate sidecar web
  app (rejected — not integrated alongside the terminal tab); terminal scraping
  for messages (rejected — lossy vs. existing JSONL parsing).
- **Why chosen:** only the native, self-hosted tab satisfies work+personal scope,
  full-content-in-app, and first-class integration simultaneously, while
  collapsing the security model to "same as the existing terminal tab."
- **Consequences:** `amux-server.py` diverges from upstream (managed via §7); a
  second on-disk source (JSONL) parsed for rendering; auto-updater must target
  the fork if enabled.
- **Follow-ups:** optional external adapter (personal-only) via the preserved
  engine seam; conversation search; streaming token feel.
