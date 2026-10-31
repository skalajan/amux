# Local amux Modifications

Custom changes layered on top of upstream `mixpeek/amux`.

## Local Delta Registry

The single canonical list of every fork-local edit to a tracked file
(`amux-server.py`, `amux`). Both the feature-dev rules
([`.claude/rules/extend-via-sidecar.md`](.claude/rules/extend-via-sidecar.md))
and the sync SOP ([`docs/upstream-sync.md`](docs/upstream-sync.md)) point here
for the area list and per-area resolution notes — this table is their **only**
home. Any new in-file change gets a row here in the same commit (see
`extend-via-sidecar.md`'s post-conditions checklist).

### Target placement after the Rust cutover (P3, decided 2026-08-17)

This fork has migrated from the Python server (`amux-server.py`, dead upstream) onto
upstream's Rust workspace — see [`.omc/plans/rust-migration.md`](.omc/plans/rust-migration.md)
for the full plan.

> **Status correction (2026-09-10).** Everything below used to say cutover had not
> executed and that `amux-server.py` was still serving production on port 8822. That has
> been false since 2026-08-09. **The Rust server is what runs here now**: binary
> `~/.local/bin/amux-server-rs` on `https://localhost:8824`, `/health` reporting
> `"server":"amux-rust"`. Port 8822 is retired and upstream fails the build if it
> reappears (`tests/legacy_port_guard.rs`). `amux-server.py` is retained as the frozen
> rollback path and parity oracle, nothing more. Registry rows below describe the
> mechanism per delta; where a row says "today" it means the pre-cutover Python shape and
> should be read as history.

The table here is the *decided target*: where each delta lands now that the fork sits on
Rust.

**Net result, and the headline of this table: zero in-file Rust deltas.** Every surviving
delta resolves to config, a sidecar, or the unchanged bash CLI — none of them land inside
`crates/`. Two rows that this registry once carried as open questions (`AMUX_COMMIT_STAMP`
toggle, yolo-guardrail) turned out not to need placement at all: Rust already does natively
what they existed to bolt on, so they're obsolete, not ported. Phase P3's gate required
**zero in-file Rust deltas**; that gate has now been met by every row, not just aimed at
(see [`.claude/rules/extend-via-sidecar.md`](.claude/rules/extend-via-sidecar.md)).

> **Correction (2026-09-13): the count is no longer zero — it is three.** F-5, F-6 and F-7
> (rows at the end of the registry below) are in-file edits inside `crates/`, deliberately.
> All three are seam changes with no sidecar seam to take them: F-5 adds a field to a struct
> only upstream constructs, F-6 changes the admission decision of an upstream handler, and
> F-7 filters an upstream map at the one site upstream builds both spawn specs from. The
> paragraph above is kept rather than rewritten because the *claim it makes about the
> account-routing row is also wrong*: it says `bootstrap.rs` "injects worker env into the
> spawn", which is what F-5 had to add — the worker `environment` map reached only the
> interactive tmux spawn, so the config-only resolution that row promised did not work for
> orchestrator-dispatched runs at all.

| Delta | Today (pre-cutover) | Target placement | Notes |
|---|---|---|---|
| Account routing / multi-home (4 registry rows below) | Code delta in `amux-server.py` + `amux` | **Config** | Worker-scoped `CLAUDE_CONFIG_DIR` env var — upstream's `env` is a scopable capability and `bootstrap.rs` injects worker env into the spawn. Zero `crates/` edit. |
| Stub-writer guard | Code delta in `amux-server.py` | **Obsolete** | Deleted below — the incident it guarded against doesn't recur in the new architecture. |
| Localhost write-auth token | Code delta in `amux-server.py` + `amux` | **Config** | `AMUX_RS_NO_LOOPBACK_BYPASS=1`. **Caveat:** blunter than the delta it replaces — it removes the localhost auth bypass for reads as well as writes, so every `curl -sk $AMUX_URL` recipe in this repo's docs/skills/commands (which the bypass currently makes token-free) needs a bearer token after cutover. Default assumption if not otherwise decided: accept the blunter model and update the recipes, per the plan. |
| Session chat — dashboard tab (B2: `chat.js`/`chat.css`) | Code delta + referenced files | **Delete** | Never the front-end actually used (Telegram is); not ported. See plan phase P8. |
| Session chat — capture core + reply-summary (B1) | Code delta in `amux-server.py` | **Sidecar** | Extracted into standalone `amux-chat.py`, so Telegram delivery (`amux-telegram.py`) keeps working across cutover. In progress. |
| `amux` bash CLI rows (2, below) | Code delta in `amux` | **Unchanged** | The bash CLI survives upstream as a legacy client — still a single tracked file, still takes grafted deltas the same way. |
| `AMUX_COMMIT_STAMP` toggle | Code delta in `amux-server.py` | **Obsolete** | Rust has its own commit-hook machinery with `AMUX_COMMIT_GUARD` knobs — our Python-hook toggle has nothing to toggle. |
| Yolo-guardrail | Code delta in `amux-server.py` | **Obsolete** | Rust never auto-answers permission prompts in the first place — its adapter classifies the pane and returns `waiting("permission_prompt", s)` as a typed state (`backend/adapter.rs:642`), so there's nothing to guard against. |

**`Upstreamable?` column, below:** upstreaming to `mixpeek/amux` is permanently declined by
the owner — for code and for defect reports both. The column is kept, not deleted, because
it still records what *could* have gone up if that decision is ever revisited; every cell is
now prefixed **Inert** to make the column's live meaning (none, today) visually obvious
without losing the historical assessment.

**Grep landmarks** must be strings unique to the local delta (verified: none of
them currently match `upstream/main`) — not upstream-shared symbols. **Sentinel
status** is "retrofitted" only for account-routing (the lazy-retrofit dogfood
anchor); every other row is gate-tracked by its grep landmarks alone until it's
next touched (see `extend-via-sidecar.md`).

> **Audit note (2026-07-21):** the "restart-race fix" previously listed under
> `amux-server.py`'s files-touched line below (`_HIBERNATE_STARTUP_GRACE` /
> hibernate-restart death-loop patch, commit `a257fba`) is confirmed **already
> present in `upstream/main`** — it merged upstream at some point and is no
> longer a local delta. It has no registry row below and should not be treated
> as one; this is the delta-shrinking-to-zero outcome Principle 5 aims for.

### Upstream sync 2026-09-10 (merge `5a74e677`)

`git merge upstream/main` brought in 1,926 upstream commits against 100 fork-only ones
(merge base `6ebef136`, upstream head `a0f762f8`). Seven files conflicted. What changed
for this registry:

- **The `amux` CLI was taken from upstream wholesale and the hunks re-grafted**, not kept
  at the fork's older revision. Upstream's copy went 2,926 -> 4,961 lines; keeping the
  fork's would have discarded every new command. All three sentinel blocks were re-applied
  on top: `grep -c AMUX-LOCAL amux` = 14 (write-auth 4, account-routing 8, cli-defaults 2).
- **`_acurl` is retired — SUPERSEDED by upstream's `_curl`.** Upstream added its own
  single-chokepoint curl wrapper (connect-timeout + transport breadcrumb) that every
  server call already routes through, which is the shape this row always wanted. The
  fork's tokens now attach *inside* `_curl` instead of in a parallel wrapper. The
  "not on the `/health` probes" carve-out survives as a **path** gate: headers go on
  `*/api/*` URLs only, so the probes stay token-free even when they target a remote amux.
  It also skips its own header when the caller already passed `Authorization:` (upstream's
  `delegate read` does), so nothing is ever sent twice. New grep landmark: `CC_AUTH_TOKEN`
  / `CC_WRITE_TOKEN` / `AMUX-LOCAL:write-auth` inside `_curl`. **`_acurl` no longer exists
  — do not grep for it.**
- **Account routing is NOT superseded.** Upstream still has no config-dir or account
  concept anywhere in the CLI (`grep -E 'CLAUDE_CONFIG_DIR|config-dir' upstream/main:amux`
  = 0 hits), so hunks 1-3 and 5 were re-applied as written.
- **Hunk 3 now composes with upstream's preserve pass.** `cmd_register` MERGES instead of
  truncating: it carries over every `CC_*` key it does not own. The graft emits its
  `CC_CONFIG_DIR` default *before* that preserved block, so a re-register writes the key
  twice and the preserved (pinned) value wins, which is the intended "first-register
  default, re-register keeps". Verified live. The duplicate key is a wart, not a bug.
- **Hunk 4 (`cmd_exec` persist) was already absent from the fork's code** before this
  merge and stayed absent — doc/code drift this registry had not caught. `amux exec`
  therefore writes no `CC_CONFIG_DIR` and its sessions fall through to the default account.
  Left as-is rather than "fixed" mid-merge; decide it deliberately.
- **Hunk 5's second semantic change was also already absent.** The OAuth probe reads
  `$HOME/.claude.json`, not `${CC_CONFIG_DIR:-$HOME}/.claude.json`. Only the
  `export CLAUDE_CONFIG_DIR` half is live.
- **Hunk 7 is obsolete as written.** The `--no-yolo` case and the default-injection block
  in `parse_claude_flags` were replaced by the fork's own single yolo test in `cmd_start`
  long before this merge. `AMUX_NO_YOLO=1` still opts out; the `--no-yolo` flag does not
  exist.
- **The `--session-id` mint now sits above a new upstream default, deliberately.**
  Upstream added `cmd="$cmd --continue"` so a restarted worker resumes instead of starting
  untitled, and its own skip-list names `--session-id`. The mint is placed above that
  check, so minting suppresses `--continue` through upstream's designed hook and the two
  flags never travel together. **Cost: this fork gets no crash-recovery resume** — a
  restarted session starts a fresh conversation, exactly as before the merge. Getting both
  needs `--resume <saved-id>`, i.e. persisting the minted id per session. Not wired up;
  this is the open question the next person should pick up.
- **`skills/amux.md` is now a symlink** to `.claude/commands/amux.md` (upstream
  de-duplicated the two). The fork's auth annotations live in the symlink target only.
- **`scripts/hooks/hook-report.sh`**: both sides independently fixed the same
  exit-code-vs-HTTP-status bug. Upstream's version was taken (it also covers `000`, and
  says out loud that it fails open) with the fork's bearer-token block grafted back on —
  without it every report 401s, which is the whole reason that block exists.

| Area | Tracked file(s) | Grep landmarks (unique-to-local) | Sentinel status | Reapply-hunk anchor | Resolution note | Upstreamable? |
|---|---|---|---|---|---|---|
| Account routing / multi-home | `amux-server.py` | `_pick_claude_config_dir`, `_session_claude_config_dir`, `_claude_config_homes`, `_claude_project_dir`, `AMUX_WORK_PATHS` | **Retrofitted** — `AMUX-LOCAL:account-routing` sentinel open at `amux-server.py:1643`, close at `:1776` (verified: `grep -c AMUX-LOCAL amux-server.py` = 2) | Self-contained function block `_pick_claude_config_dir` → `_cc_session_id_for_name` (1643–1776); bounded by the `AMUX-LOCAL:account-routing` sentinel, not a hunk positioned on an upstream landmark — merges via git's own 3-way merge | Keep `_pick_claude_config_dir`, `_session_claude_config_dir`, `_claude_config_homes`, `_claude_project_dir` — keep local one-liners like `project_dir = _claude_project_dir(...)` over upstream's `CLAUDE_HOME / "projects" / ...` | Inert — upstreaming declined. Was: N (personal Fidoo work-path defaults; mechanism not pursued upstream) |
| Token stats — multi-home iteration | `amux-server.py` | `# Sessions may run under any Claude config home (work/personal routing)` (comment inside `_refresh_token_cache`) | Not sentinel-wrapped (lazy retrofit — gate-tracked by grep landmark only) | `_refresh_token_cache` (upstream-owned function name; local delta is the `_claude_config_homes()` iteration grafted inside it) | Adopt upstream's token-stats logic, but iterate all config homes (`_claude_config_homes()`) instead of just `~/.claude` | Inert — upstreaming declined. Was: N (depends on account-routing existing upstream first) |
| `start_session()` cmd build — `CLAUDE_CONFIG_DIR` prefix | `amux-server.py` | `CLAUDE_CONFIG_DIR={shlex.quote(claude_config_dir)}`, `Env-prefix selects the Claude.ai account` | Not sentinel-wrapped (lazy retrofit) | `cmd = _custom_claude or "claude"` (upstream-owned base command build in `start_session()`, ~line 10823) | Keep the `CLAUDE_CONFIG_DIR=` env prefix wrapped around whatever base command upstream uses | Inert — upstreaming declined. Was: N (depends on account-routing) |
| `POST /api/sessions` — `config_dir` block | `amux-server.py` | `config_dir = str(body.get("config_dir", "")).strip()` | Not sentinel-wrapped (lazy retrofit) | `creator = body.get("creator", "").strip()` (upstream-owned field immediately above, ~line 45980) | Keep the `config_dir` (`"work"`/`"personal"`/path) block | Inert — upstreaming declined. Was: **Y** — generic `config_dir` field, strongest PR candidate |
| `AMUX_COMMIT_STAMP` toggle — **post-cutover: obsolete, not ported** (Rust ships its own `AMUX_COMMIT_GUARD`-based commit-hook machinery; see "Target placement" above) | `amux-server.py` | `AMUX_COMMIT_STAMP` | Not sentinel-wrapped (lazy retrofit) | `_commit_stamp_enabled` / `_install_amux_commit_hook` (upstream-owned commit-stamp hook; local delta is the on/off toggle inside) | Keep the `AMUX_COMMIT_STAMP` toggle in the commit-stamp hook install/enable logic — graft around whatever upstream changes to the hook-install mechanism | Inert — upstreaming declined. Was: **Y** — strongest PR candidate |
| `amux` CLI — config-dir flags & auto-detect (Hunks 1–5) | `amux` | `pick_default_config_dir`, `AMUX_WORK_PATHS`, `CC_CONFIG_DIR` | Not sentinel-wrapped (lazy retrofit; bash CLI, not amux-server.py — same policy applies) | `parse_claude_flags`, `cmd_register`, `cmd_exec`, `cmd_start` shell_setup block (see Hunks 1–5 below) | Keep the MODIFICATIONS.md hunks (config-dir flags, auto-detect + persist) AND upstream's new commands — both sides | Inert — upstreaming declined. Was: N (tied to account-routing feature, not pursued upstream) |
| `amux` CLI — remote-control/model defaults, yolo-by-default & **minted `--session-id`** (Hunks 6–7) | `amux` | `AMUX_NO_REMOTE_CONTROL`, `AMUX_DEFAULT_MODEL`, `claude-opus-5[1m]`, `AMUX_NO_YOLO`, `AMUX-LOCAL:cli-defaults`, `uuidgen`/`--session-id` | **Sentinel-wrapped** (`AMUX-LOCAL:cli-defaults`) | `cmd_start` claude-branch, `else` of the `provider == "codex"` check (see Hunks 6–7 below); the `--session-id` mint is the last statement before the closing `/AMUX-LOCAL:cli-defaults` sentinel | Keep the MODIFICATIONS.md hunks (yolo default, default model, remote-control, session-id mint) AND upstream's new commands — both sides. The mint must stay INSIDE the non-codex branch (codex takes no `--session-id`) and must stay guarded by `[[ "$cmd" != *"--session-id"* ]]` so an explicit flag in `CC_FLAGS`/inline args wins — rewriting an existing session's id is the turn-theft bug with a new cause | **Y** for the session-id mint — generic capture-reliability fix, good upstream PR candidate (upstream's CLI has the same gap). Rest: inert — upstreaming declined |
| Localhost write-auth token | `amux-server.py`, `amux` | `X-Amux-Write-Token`, `_WRITE_TOKEN`, `_write_auth_ok`, `_is_write_path`, `_load_or_create_write_token`, `_WRITE_TOKEN_FILE`, `CC_WRITE_TOKEN`, `CC_AUTH_TOKEN`, `_curl`, `AMUX_WRITE_TOKEN`, `AMUX-LOCAL:write-auth` | Sentinel-wrapped in BOTH (`AMUX-LOCAL:write-auth`; `grep -c AMUX-LOCAL:write-auth amux` = 4 since the 2026-09-10 merge — one fence pair at the token reads, one inside `_curl`) | Helpers block after `_PUBLIC_PREFIXES = (...)`; hoisted gate = first logic in `_check_auth` (above both early-returns); dashboard `_AMUX_WRITE_TOKEN` inject after `window._AMUX_UI_TOKEN`; `_writeToken`/`_authHeaders` in the auth `<script>`; stub `AMUX_WRITE_TOKEN=` after `AMUX_URL=` in the `_amux_stub` heredoc; startup self-check after `_sync_skills_and_cli()`; `amux` CLI (post-2026-09-10): `CC_AUTH_TOKEN=`/`CC_WRITE_TOKEN=` immediately after `CC_HOME=`, and the header block INSIDE upstream's own `_curl()` — no per-call-site edits at all, because upstream already routes every server call through it | Keep the hoisted write-auth gate ABOVE `if not AUTH_TOKEN` and the localhost bypass; keep the independent 0600 `~/.amux/write_token` secret (never derive from AUTH_TOKEN); keep `X-Amux-Write-Token` on every HTTP write consumer (stub, CLI, dashboard, procwarden, skills). Only the file secret or `Bearer AUTH_TOKEN` (when set) grant a write — never `_UI_TOKEN`/derived. Graft around whatever upstream changes to `_check_auth`. **Post-cutover (rust) the CLI half is a WRAPPER, not per-call headers.** Two secrets, NOT interchangeable and never cross-fall-back: rust/8824 takes `Authorization: Bearer <~/.amux/auth_token>` only, the retained python/8822 rollback takes `X-Amux-Write-Token: <~/.amux/write_token>` only — the other combination 401s (verified live). `_curl` sends both so one CLI drives either server. It covers READS too: `AMUX_RS_NO_LOOPBACK_BYPASS=1` removes the loopback bypass for reads as well (the row-36 caveat). Deliberately NOT on the two `/health` probes in `_resolve_url`/`_warn_stale_amux_url` — those may target a REMOTE amux and the token must not leave this host. Since the merge that carve-out is enforced by a PATH gate (`*/api/*`) rather than by which sites call which wrapper, so it holds for upstream's future call sites too. Bash 3.2: no arrays (empty `"${a[@]}"` is unbound under `set -u`). **Regression history:** the merge at `21596153` took upstream's CLI and dropped this fork's per-curl token header, so the ENTIRE CLI 401'd (`amux board show` included) while this row still claimed the header existed — a wrapper is the fix's shape precisely so the next merge loses one greppable function rather than a header on 38 call sites. The 2026-09-10 merge is the test of that claim and it held: upstream's rewritten CLI needed one graft, not 38 | Inert — upstreaming declined. Was: **Y** — generic localhost write-auth; strong upstream PR candidate |
| Session chat (Scope B1 core + B2 dashboard tab + reply-summary marker) — **post-cutover: B2 deleted, B1 + reply-summary → `amux-chat.py` sidecar (see "Target placement" above)** | `amux-server.py`, `chat.js`, `chat.css`, `amux-telegram.py`, `docs/reply-summary.md` | `chat_messages`, `chat_replies`, `_chat_extract_turns`, `_chat_populate_replies`, `_chat_build_thread`, `_chat_insert_owner`, `_chat_insert_system`, `_chat_delivery_status`, `_chat_reconcile_all`, `_chat_populate_replies_throttled`, `_chat_schedule_post_delivery_populate`, `_chat_notify`, `_chat_replies_lock`, `_chat_limit_notified`, `_chatOnSSE`, `_chatPoll`, `_chatTabOpen`, `_chatTabClose`, `peek-tab-chat`, `peek-chat-panel`, `AMUX-LOCAL:session-chat`, `/api/chat`, `/chat.js`, `/chat.css`, `_chat_parse_summary_marker`, `_CHAT_SUMMARY_MARKER`, `_chat_summarize_text`, `_chat_summary_tick`, `_chat_summary_worker_loop`, `AMUX_SUMMARY_MODEL`, `AMUX_SUMMARY_TIMEOUT`, `AMUX_SUMMARY_DISABLE`, `chat-expand`, `_chat_resolve_jsonl_path`, `_chat_live_conv_path`, `_chat_conv_fallback_cache`, `_CHAT_CONV_FALLBACK_TTL`, `_chat_owned_conv`, `_chat_conv_jsonl` | Sentinel-wrapped (balanced `AMUX-LOCAL:session-chat` block fences — Python `#`, JS `//`, SQL `--`, HTML `<!-- -->` — plus a few open-only inline markers matching B1 style; `grep -c AMUX-LOCAL:session-chat` = 51, up from 37 — the reply-summary marker parser + Haiku worker + migration line + startup thread-start + the client-side `summary`-kind SSE handling all landed inside new/extended fences; the freshness/poll-flood fixes are net +1 (a new `AMUX-LOCAL:session-chat` fence pair around the post-delivery populate call in `_steer_try_deliver`, minus the inline `_chatOnSSE` summary marker that moved to `chat.js`); +2 in the dead-slot-leak fix, which fenced AMUX-10's `_chat_live_conv_path`/`_chat_resolve_jsonl_path` block — it landed unfenced). **B2 feature code lives in referenced files `chat.js`/`chat.css` (upstream has neither → conflict-immune); the in-file B2 footprint is ~15 functional lines.** | **B1:** schema tables in `_DB_SCHEMA` after `steering_history` (`--` fence); helper block after `get_db()` (`#`); `POST/GET /api/chat` in `_route_inner` after `/api/tunnel/stop` (GET calls `_chat_populate_replies_throttled(session)` before `_chat_build_thread` — Bug-1 freshness hook for BOTH dashboard + Telegram); `_classify_request` chat entry before `# System`; monitor idle-hook `_chat_populate_replies(sname)` inside `_on_idle`; fenced `_chat_schedule_post_delivery_populate(name)` in `_steer_try_deliver`'s `if ok:` success block (Bug-1: +5s/+15s/+30s deferred populate after a delivered steer); the two throttle/schedule helpers sit right after `_chat_populate_replies`; **AMUX-10 (fresh-session capture):** `_chat_populate_replies` resolves its transcript via `_chat_resolve_jsonl_path` (not `_session_jsonl_path` directly) — meta path primary, then `_chat_live_conv_path` fallback (defined just above `_chat_populate_replies`) which resolves the live conv id via `_live_conv_id` (running-process argv → newest jsonl across ALL config homes) and reconstructs the path, so fresh sessions with no recorded `cc_conversation_id` and transcripts under a non-CLAUDE_HOME config home (mac-server `~/.claude-personal`) are captured; the resolved id is cached in-memory in `_chat_conv_fallback_cache` (TTL `_CHAT_CONV_FALLBACK_TTL`) so `_live_conv_id`'s ps/tmux runs at most once per TTL per session, and NOT persisted to meta (a wrong shared-dir guess self-corrects next poll, never cements into resume); SSE `chat` emit after the alerts push + reconcile-on-connect before the `_sse_events` `while True`; usage-limit `_chat_insert_system` in `_rate_limit_auto_respond`; dashboard `_chatOnSSE` (now dispatch-only) `/_chatPoll` block + fallback wiring in `enablePollingFallback`. **Bug-2 poll-flood fix lives in `chat.js`** (its `amux:chat` listener owns the session-filter + trailing-edge <=1/2s throttle + `summary`-kind cursor-reset that used to be inline in `_chatOnSSE`). **B2:** `<link href="/chat.css">` after the xterm CSS `<link>`; `<script src="/chat.js" defer>` after the fullcalendar `<script>`; `peek-tab-chat` button after `peek-tab-terminal` in `.peek-tabs`; `<div id="peek-chat-panel">` after `peek-messages-panel`; chat dispatch appended in `setPeekTab` after the notes-panel block; `_chatTabClose()` call in `closePeek` after `peekSession = null`; `/chat.js`+`/chat.css` static route after the `/sw.js` route; `/chat.js`+`/chat.css` added to `_PUBLIC_PATHS`. **Reply-summary:** `summary` column + migration line grafted onto `chat_replies`' `CREATE TABLE` and the `_init_db()` migrations list; `_chat_parse_summary_marker` immediately before `_chat_extract_turns` (which now calls it per turn); `_chat_populate_replies`' INSERT and `_chat_build_thread`'s SELECTs extended with `summary`; the Haiku worker (`_chat_summary_env`/`_chat_summarize_text`/`_chat_summary_tick`/`_chat_summary_worker_loop`) block appended right before the `/AMUX-LOCAL:session-chat` close after `_chat_build_thread`; its daemon-thread start next to `_install_hooks_all_sessions` in the startup sequence; the `kind === 'summary'` cursor-reset branch now lives in `chat.js`'s `amux:chat` listener (moved out of the inline `_chatOnSSE` with the Bug-2 fix); `chat.js`'s `.chat-expand` affordance + `_isCollapsible`/`_expanded` state wired into `_bodyHtml`/`_msgHtml`/`_buildShell`/`_open`; `amux-telegram.py`'s `_render_outbound` server-summary branch before its local-summarizer call. | **B1:** keep `chat_messages` an OWNER/SYSTEM-input-only log (no `delivered` column, no `role='session'` rows); keep `chat_replies` a rebuildable materialized index (rebuild via `DELETE FROM chat_replies`, NEVER `DROP TABLE` — C-crit-2); keep single-writer `_chat_populate_replies` under `_chat_replies_lock` inserting in ascending `turn_index` (C-new-1); keep the `isSidechain` subagent filter in the pure `_chat_extract_turns`; owner input goes through `POST /api/chat` → `_steer_enqueue` only, never `cmd_history`; `GET /api/chat` stays read-loose (peek parity); keep the `chat` SSE type in BOTH `_sse_events` and the dashboard polling fallback; keep the AMUX-10 live-conv-id fallback in the orchestration layer (`_chat_resolve_jsonl_path`/`_chat_live_conv_path`), leaving `_live_conv_id` and `_session_jsonl_path` untouched (pure/meta-primary), and keep the resolved id in-memory only (`_chat_conv_fallback_cache`), never persisted to meta; keep `_chat_live_conv_path`'s four-step resolution ladder in that order — `is_running` gate, then argv (`_live_conv_id(name)` with NO work dir), then sticky `_chat_owned_conv`, and only then the `_live_conv_id(name, wd)` guess. Step 4 is racy by construction (its step 2 is "newest jsonl in the work dir"), so in a CC_DIR shared with any other Claude it resolves to whichever conversation was written last — including a bare `claude` CLI amux does not own — and the stolen turn is written to chat_replies AND pushed to Telegram. Both `~/Desktop/Projects/amux` slots were hit on 2026-08-01: `--help` (dead, 7/7 rows stolen → step 1) and `amux-helper` (LIVE, argv carries only `--name`, meta has no `cc_conversation_id` because it resumes a conversation born 2026-07-16, 10 rows stolen beside its own 92 → step 3). `_chat_owned_conv` must rank by ROW COUNT not recency — a stolen turn is one row while the real conversation has hundreds, so "most recent" would hand the slot straight back to the thief. The whole ladder belongs INSIDE the cache-miss branch so it stays TTL-bounded like the `_live_conv_id` call it guards (and for dead slots is cheaper than what it replaced: one `tmux list-sessions` vs list-panes + pgrep + ps-per-pid + a work-dir scan). Accepted residual: a brand-new slot with no argv id and no history still rides step 4, and step 3 pins a slot to its old conversation if it starts a new one without an argv id — pinned-but-stale beats stealing. The durable cure is passing `--session-id` at launch (the `amux` bash CLI does not, which is why `amux-helper` had no authoritative id at all). **B2:** keep the UI in `chat.js`/`chat.css` (referenced files) with only the tiny sentinel footprint in-file; `chat.js` consumes the B1 `amux:chat-thread`/`amux:chat` CustomEvents + `window._chatActiveSession`/`_chatCursor`/`_chatPoll` (merge-by-id dedups SSE+poll overlap); composer writes via `apiCall` → `POST /api/chat` carrying `X-Amux-Write-Token` (never bypass Scope A); the chat tab is a new peek-overlay tab (Chat) beside Terminal/Messages — raw terminal stays in Terminal. **Reply-summary:** keep `_chat_parse_summary_marker` pure/deterministic so a `chat_replies` rebuild re-derives `summary` identically from the transcript (no separate persistence path); keep the Haiku worker best-effort and never-blocking — ANY failure leaves `summary` NULL, backed off via the in-memory (not persisted) `_chat_summary_failed` map, never crashes capture/delivery; keep it single-flight via the dedicated sleep-loop thread (not `schedule_job`, which would overlap invocations against a slow subprocess); keep the client's `kind === 'summary'` SSE branch (a full cursor-reset refetch) — an incremental `since=` poll can never see an UPDATE to an already-delivered `rowid_seq`; keep `amux-telegram.py` preferring the server `summary` over its own local `claude -p` call, falling back to it only when absent. | Inert — upstreaming declined. Was: **Y** — generic per-session chat + reply-summary over the existing steering/SSE surface; upstreamable |
| Yolo-guardrail — never auto-approve permission-rule prompts — **post-cutover: obsolete, not ported** (Rust's adapter classifies the pane and returns a typed `waiting("permission_prompt", s)` state, `backend/adapter.rs:642` — it never auto-answers, so nothing needs guarding; see "Target placement" above) | `amux-server.py` | `AMUX-LOCAL:yolo-guardrail`, `_yolo_is_guarded_prompt`, `_YOLO_GUARD_DESTRUCTIVE`, `_yolo_guard_logged`, `yolo_guardrail_skipped` | Sentinel-wrapped (`AMUX-LOCAL:yolo-guardrail`, two fenced blocks — the helper/matcher above `_yolo_auto_respond`, and the skip check inside its per-session loop; `grep -c AMUX-LOCAL:yolo-guardrail amux-server.py` = 4) | `_yolo_auto_respond` — the guard block sits immediately before the `for i, (pattern, response) in enumerate(_YOLO_PROMPTS)` send loop; if `_yolo_is_guarded_prompt(clean)` returns true it `continue`s (leaves the session `waiting`) instead of `send_text(name, response)`. Helper + `_YOLO_GUARD_DESTRUCTIVE` regex defined just after `_yolo_last_responded` | Keep the destructive-command stand-down: when the pane shows Claude Code's `Permission rule … requires confirmation` chrome (primary signal — an `ask` rule fired) OR a destructive command inside a `Do you want to proceed?` prompt (secondary belt: `sudo`/`rm -rf|-fr`/`git push -f|--force`/`dd if=`/`mkfs`/`git clean -fdx|-xfd`), do NOT auto-answer — let it stay `waiting` for a human (Phase B Telegram notify). Manually kept in sync with the `permissions.ask` globs in each `settings.json` (different process/file — cannot share a literal; cross-ref comment lives in the Python block). Graft around whatever upstream changes to the `_yolo_auto_respond` send site | Inert — upstreaming declined. Was: **Y** — candidate upstream PR: "yolo auto-responder skips a configurable destructive-command set" / never auto-approve explicit permission-rule confirmations |
| **F-5 — worker `environment` reaches the headless child** (in-file Rust delta, 2026-09-13) | `crates/amux-server/src/opencode/structured.rs`, `crates/amux-server/src/backend/bootstrap.rs` | `env_is_refused`, `build_command`, `worker env key refused`, `worker_env_reaches_the_headless_child`, `worker_env_cannot_repoint_the_servers_own_state` | Not sentinel-wrapped (in-file Rust delta; gate-tracked by grep landmark) | `WorkerConfig`'s `conversation` field — `env` goes immediately after it; `run_prompt`'s spawn block, whose program/argv/cwd/stdio/env build now lives in `build_command` above it; the `ProtocolWorkerConfig { … }` literal in `bootstrap.rs::register_protocol` | **Why:** `_amux_workers.environment` is upstream's own worker-scoped env capability and it reached ONLY the interactive tmux spawn (`backend/tmux.rs` `-e` flags), never the orchestrator's headless `claude --print` child — measured 2026-09-13, the tmux leg carried `CLAUDE_CONFIG_DIR` and the headless leg did not, so every dispatched task ran on the server's own Claude account regardless of the worker's config. That also falsifies the account-routing row's "config, zero `crates/` edit" resolution, which assumed this passthrough existed. **How to reapply:** add `pub env: BTreeMap<String, String>` to `structured::WorkerConfig`, hydrate it from `row.environment.clone()` in `register_protocol`, and apply it in `build_command` AFTER `provider.envs()` so the worker map wins — inherited server env, then provider defaults, then worker map. Keep `env_is_refused` (`PATH`, `HOME`, `AMUX_*`): a worker that could set those would repoint the server's own binaries and state directory. Keep the `build_command` extraction — the tests read the env back off the built `Command`, which asserting on the map alone would not. If upstream re-inlines the spawn, re-graft the two `for` loops in that order. | Inert — upstreaming declined. Was: **Y** — generic gap; the env capability is upstream's, only one of its two spawn paths honored it |
| **F-6 — `force` reserved for human actors** (in-file Rust delta, 2026-09-13) | `crates/amux-server/src/api/board.rs` | `force_requires_human`, `ForceCaller`, `force_caller`, `force_by_machine`, `force is reserved for human actors`, `a_worker_may_not_force_its_own_card_but_a_human_may`, `call_as_human`, `human_member` | Not sentinel-wrapped (in-file Rust delta; gate-tracked by grep landmark) | `patch_item` — the refusal sits ABOVE the "force requires attribution" 400 (checked first, so a worker cannot slip through by omitting its header: Human is tested before Machine/Anonymous) and ABOVE the AMUX-3464 "force requires a reason" 400 (so "you may not force" precedes "with what justification"). `force_caller`/`ForceCaller` live next to `actor_from_headers` | **Why:** measured 2026-09-13 — a worker PATCHed its OWN gated card to `verified` with `force: true` under its own `X-Amux-Worker` and got 200 with the gate skipped, while the same worker's plain PATCH was correctly refused 409. The gate held on the honest path and opened on the audited bypass, so independent verification was self-service. Attribution (ts-gke) answers WHO and was never the gap: all 41 historical forces were attributed. **How to reapply:** resolve the caller through `force_caller` and refuse `ForceCaller::Machine` with 403 `{"error":"force is reserved for human actors","code":"force_requires_human"}`. `Human` MUST be `org::is_verified_local_member` and MUST be checked first — that marker is inserted server-side after an invite cookie resolves and stripped from the wire, so it is the only actor kind this server can verify, and the same middleware sets `x-amux-session: member:<email>` for a member who sent no header, so a headers-first check would refuse the humans this reserves force for. Machine = `X-Amux-Worker`/`X-Amux-Session` or a body `session`; both, because either alone is walkable from the other. **COST, decide deliberately:** `amux board <status> --force` sends `X-Amux-Worker`, so the bash CLI now gets 403 — after F-6 the only caller that can force is an invite-verified local member. That is intentional (a human at a CLI and a dispatched worker are byte-identical on the wire) but it is a real operational change, not a no-op | Inert — upstreaming declined. Was: **Y** — generic: a gate an actor can bypass on its own attribution is the gate verifying itself |
| **F-7 — one env refusal list, on BOTH spawn legs** (in-file Rust delta, 2026-09-13) | `crates/amux-server/src/backend/bootstrap.rs`, `crates/amux-server/src/opencode/structured.rs` | `env_is_refused`, `spawn_env`, `worker env key refused`, `refused_env_keys_reach_neither_spawn_leg`, `the_refusal_list_covers_each_family_and_spares_the_rest`, `the_headless_leg_does_not_re_filter_what_bootstrap_handed_it` | Not sentinel-wrapped (in-file Rust delta; gate-tracked by grep landmark) | `bootstrap.rs` above `ProtocolRegistrar`; both call sites are `env: spawn_env(&row.environment, &row.id)` — the `ProtocolWorkerConfig` literal in `register_protocol` and the `SessionSpec` literal in the spawn sweep | **Why:** security review C-2 on F-5. F-5 put the refusal list inside the HEADLESS command build, so it covered one leg; `backend/tmux.rs` turns every `spec.env` pair into an `-e KEY=VALUE` flag verbatim, and that map comes from the same `row.environment`. A worker env carrying `PATH` or `DYLD_INSERT_LIBRARIES` therefore reached the interactive spawn intact — loader injection is arbitrary code in the child, not configuration. **How to reapply:** keep the filter in `bootstrap.rs` and ONLY there — it is the single place a worker row's env becomes spawn input for either leg (`SessionSpec` is constructed at exactly one site, `ProtocolWorkerConfig` at one more, both in that file), and a second copy in the consumer is how the two legs came apart in the first place. Refused, case-insensitively: `PATH`, `HOME`, `AMUX_*`, `DYLD_*`, `LD_*`, `NODE_OPTIONS`, `SSL_CERT_FILE`, `SSL_CERT_DIR`, `GIT_CONFIG*`, `GIT_EXEC_PATH`, `ANTHROPIC_API_KEY`, `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`. Case-insensitive because the lowercase proxy spellings are live (curl reads `http_proxy`). `SSL_CERT_DIR` is not in the review's list and is here anyway: it redirects the same trust root as `SSL_CERT_FILE`, so blocking one is a bypass under a sibling name. Explicitly allowed: `CLAUDE_CONFIG_DIR` (the point of the map), `AIFIRST_*`, `GIT_AUTHOR_*`/`GIT_COMMITTER_*`, plain vars. If a third spawn leg ever appears, it calls `spawn_env` too — do not re-filter in the consumer | Inert — upstreaming declined. Was: **Y** — generic: upstream's own `environment` capability is unfiltered on the leg it does reach |

Goal: per-session Claude account selection. The session's `CLAUDE_CONFIG_DIR` decides which Claude.ai account is used inside the tmux pane — work vs personal vs anything else.

> **2026-06-12 — implemented in BOTH the CLI and the server.**
> The routing now lives in two places that must stay in sync:
>
> 1. **`amux` (bash CLI)** — the hunks below (register/exec persist `CC_CONFIG_DIR`, cmd_start exports it). Sessions started via `amux start` use this path.
> 2. **`amux-server.py`** — `_pick_claude_config_dir` / `_session_claude_config_dir` plus `CLAUDE_CONFIG_DIR` injection in `start_session()`. Sessions started via the dashboard, the API, or auto-wake use this path. The server also routes transcript/resume/memory/stats lookups across all config homes (`_claude_config_homes`, `_claude_project_dir`).
>
> **Incident note:** the server's `_auto_trust_dir` used to rewrite `/usr/local/bin/amux` with its minimal CLI stub whenever the content differed. Since that path is a symlink into this repo, it silently destroyed the modified bash CLI (uncommitted at the time). The stub-writer now refuses to follow symlinks or overwrite anything that isn't the stub itself, and the CLI hunks were reapplied and committed. The CLI additionally gained server-API passthroughs (`crm`, `restart`, `sessions`, `share`, `unshare`) that previously only existed in the stub.

## Rule

- **Personal is the default.**
- **Work** is auto-selected when the session's working directory (`--dir`) is under any path listed in `$AMUX_WORK_PATHS` (colon-separated, default `$HOME/Desktop/Projects/Fidoo`).
- New CLI flags override the auto-detect:
  - `--config-dir <abs-path>` — explicit
  - `--work` — shortcut for `$AMUX_WORK_CONFIG_DIR` (default `$HOME/.claude-work`)
  - `--personal` — shortcut for `$AMUX_PERSONAL_CONFIG_DIR` (default `$HOME/.claude-personal`)

Decision sticks: the chosen dir is persisted to `~/.amux/sessions/<name>.env` as `CC_CONFIG_DIR=...`. Every subsequent `amux start <name>` re-reads it.

## Configurable environment

| Variable | Default | Purpose |
|---|---|---|
| `AMUX_WORK_PATHS` | `$HOME/Desktop/Projects/Fidoo` | Colon-separated paths whose subdirs auto-select work |
| `AMUX_WORK_CONFIG_DIR` | `$HOME/.claude-work` | Target dir for `--work` and auto-selection |
| `AMUX_PERSONAL_CONFIG_DIR` | `$HOME/.claude-personal` | Target dir for `--personal` and default |
| `AMUX_DEFAULT_MODEL` | `claude-opus-5[1m]` | Model passed to `claude` when the session didn't set its own `--model` |
| `AMUX_NO_REMOTE_CONTROL` | (unset) | Set to `1` to skip auto-appending `--remote-control` |
| `AMUX_NO_YOLO` | (unset) | Set to `1` to disable the default `--dangerously-skip-permissions` (yolo) for new sessions |

Set these in `~/.amux/server.env` or your shell rc to override.

## Files touched

- `amux` (the bash CLI) — hunks 1–7 below + server-API passthrough commands
- `amux-server.py` — account routing in `start_session()` + cross-home lookups

## How to reapply after `git pull` from upstream

Re-run the 5 hunks below against the freshly pulled `amux` file, then deploy:

```bash
bash -n amux
```

**Do not `cp` over `/usr/local/bin/amux`.** It is a symlink into this worktree, so the
file you just edited is already the live CLI — copying onto it would replace the symlink
with a stale snapshot. Upstream's `make install-cli` describes a different (snapshot)
model that this machine does not use.

If any `old_string` no longer matches verbatim (upstream refactored the surrounding code), find the closest equivalent — the hunks are positioned around stable structural landmarks (`parse_claude_flags`, `cmd_register`, `cmd_exec`, `cmd_start` shell_setup block).

### Hunk 1 — add helper `pick_default_config_dir`

Insert immediately after `parse_claude_flags()` closing `}`, before the `# ── Commands ──` divider.

```bash
# pick_default_config_dir <resolved_dir> — decide which Claude config dir
# (= which Claude.ai account) a new session should use when neither
# --config-dir nor --work nor --personal was passed.
#
# Rule: personal by default; work when the resolved working dir is under
# any path listed in $AMUX_WORK_PATHS (colon-separated). Configurable via:
#   AMUX_WORK_PATHS          (default: $HOME/Desktop/Projects/Fidoo)
#   AMUX_WORK_CONFIG_DIR     (default: $HOME/.claude-work)
#   AMUX_PERSONAL_CONFIG_DIR (default: $HOME/.claude-personal)
pick_default_config_dir() {
  local resolved="$1"
  local work_paths="${AMUX_WORK_PATHS:-$HOME/Desktop/Projects/Fidoo}"
  local work_dir="${AMUX_WORK_CONFIG_DIR:-$HOME/.claude-work}"
  local personal_dir="${AMUX_PERSONAL_CONFIG_DIR:-$HOME/.claude-personal}"
  local IFS=':' p
  for p in $work_paths; do
    [[ -z "$p" ]] && continue
    case "$resolved" in
      "$p"|"$p"/*) echo "$work_dir"; return ;;
    esac
  done
  echo "$personal_dir"
}
```

### Hunk 2 — three new flags in `parse_claude_flags`

Insert immediately AFTER the existing `--dir)` case and BEFORE the catch-all `*)`:

```bash
      # Claude config directory — selects which account to launch as.
      # --config-dir specifies an absolute path; --work / --personal are
      # shortcuts for the standard locations (see pick_default_config_dir).
      --config-dir)
        mkdir -p "$2" || die "cannot create --config-dir: $2"
        CC_CONFIG_DIR="$(cd "$2" && pwd)" || die "cannot resolve --config-dir to absolute path: $2"
        shift 2
        ;;
      --work)
        CC_CONFIG_DIR="${AMUX_WORK_CONFIG_DIR:-$HOME/.claude-work}"
        shift
        ;;
      --personal)
        CC_CONFIG_DIR="${AMUX_PERSONAL_CONFIG_DIR:-$HOME/.claude-personal}"
        shift
        ;;
```

### Hunk 3 — `cmd_register` auto-detect + persist

In `cmd_register()`, after the `case "$resolved_dir" ... esac` validation block, insert the auto-detect call. Then extend the heredoc with `CC_CONFIG_DIR` and the final echo with a `config-dir:` line:

```bash
  # Auto-detect Claude config dir (account) if not explicitly set.
  [[ -z "${CC_CONFIG_DIR:-}" ]] && CC_CONFIG_DIR="$(pick_default_config_dir "$resolved_dir")"

  local file="$CC_SESSIONS/$name.env"
  cat > "$file" <<EOF
# amux session: $name
# registered: $(date -Iseconds)
CC_NAME="$name"
CC_DIR="$resolved_dir"
CC_FLAGS="${CC_FLAGS_STR}"
CC_CONFIG_DIR="$CC_CONFIG_DIR"
EOF

  echo "${GREEN}registered${RESET} ${BOLD}$name${RESET} → $resolved_dir"
  echo "  config-dir: $CC_CONFIG_DIR"
  [[ -n "$CC_FLAGS_STR" ]] && echo "  flags: $CC_FLAGS_STR"
```

### Hunk 4 — `cmd_exec` auto-detect + persist

Same auto-detect + CC_CONFIG_DIR heredoc line in `cmd_exec()` (no echo change there since exec immediately chains into start):

```bash
  # Auto-detect Claude config dir (account) if not explicitly set.
  [[ -z "${CC_CONFIG_DIR:-}" ]] && CC_CONFIG_DIR="$(pick_default_config_dir "$resolved_dir")"

  # Register if not exists, or update
  local file="$CC_SESSIONS/$name.env"
  cat > "$file" <<EOF
# amux session: $name
# registered: $(date -Iseconds)
CC_NAME="$name"
CC_DIR="$resolved_dir"
CC_FLAGS="${CC_FLAGS_STR}"
CC_CONFIG_DIR="$CC_CONFIG_DIR"
EOF
```

### Hunk 5 — `cmd_start` shell_setup: inject export + fix OAuth check

In `cmd_start()`, replace the existing `shell_setup` block (the one that unsets `CLAUDECODE` and conditionally `ANTHROPIC_API_KEY`) with:

```bash
  local shell_setup=""
  if [[ "$provider" != "codex" ]]; then
    shell_setup="unset CLAUDECODE CLAUDE_CODE_ENTRYPOINT; "
    # Per-session Claude config dir (account isolation: work vs personal vs ...).
    # Injecting the export *inside* the tmux command ensures correctness
    # regardless of whether the tmux server was started before or after amux
    # itself had CLAUDE_CONFIG_DIR set — tmux server env doesn't update on
    # client reconnect.
    if [[ -n "${CC_CONFIG_DIR:-}" ]]; then
      shell_setup="${shell_setup}export CLAUDE_CONFIG_DIR=$(printf '%q' "$CC_CONFIG_DIR"); "
    fi
    # OAuth check: look in the *session's* config dir so ANTHROPIC_API_KEY only
    # gets unset when this session's target account actually has OAuth.
    local oauth_file="${CC_CONFIG_DIR:-$HOME}/.claude.json"
    if [[ -f "$oauth_file" ]] && grep -q '"oauthAccount"' "$oauth_file" 2>/dev/null; then
      shell_setup="${shell_setup}unset ANTHROPIC_API_KEY; "
    fi
  fi
```

Two semantic changes here:
1. Injects `export CLAUDE_CONFIG_DIR=...` into the tmux command (not just amux's own env) so it propagates regardless of tmux-server state.
2. Looks up `.claude.json` under the **session's** config dir, not always `$HOME`, so `ANTHROPIC_API_KEY` is unset only when the target account actually has OAuth.

## Verification (post-reapply)

```bash
bash -n /usr/local/bin/amux && echo "syntax ok"

# Test 1 — Fidoo path auto-picks work
amux register _t1 --yolo --dir "$HOME/Desktop/Projects/Fidoo/fidoo-mobile-apps"
grep CC_CONFIG_DIR ~/.amux/sessions/_t1.env  # expect $HOME/.claude-work

# Test 2 — non-Fidoo path auto-picks personal
amux register _t2 --yolo --dir /tmp
grep CC_CONFIG_DIR ~/.amux/sessions/_t2.env  # expect $HOME/.claude-personal

# Test 3 — explicit --personal overrides Fidoo auto-detect
amux register _t3 --personal --yolo --dir "$HOME/Desktop/Projects/Fidoo/fidoo-mobile-apps"
grep CC_CONFIG_DIR ~/.amux/sessions/_t3.env  # expect $HOME/.claude-personal

# Test 4 — explicit --config-dir
amux register _t4 --config-dir "$HOME/.claude" --yolo --dir /tmp
grep CC_CONFIG_DIR ~/.amux/sessions/_t4.env  # expect $HOME/.claude

# Cleanup
for s in _t1 _t2 _t3 _t4; do amux rm $s; done
```

All four lines should print the expected paths.

### Hunk 6 — `cmd_start`: auto-append `--remote-control` and bump default model to Opus 4.7 (1M)

In `cmd_start()`, in the **claude branch** of the codex-vs-claude conditional (the `else` of `if [[ "$provider" == "codex" ]]`), replace:

```bash
  else
    [[ "$cmd" == *"--model"* ]] || cmd="$cmd --model sonnet"
  fi
```

with:

```bash
  else
    # Always enable Claude Code's per-session remote-control URL
    # (claude.ai/code/session_XXX). Complementary to amux's dashboard — gives
    # a stable per-session browser handle for that one session.
    # Opt out per host with: AMUX_NO_REMOTE_CONTROL=1
    [[ "${AMUX_NO_REMOTE_CONTROL:-}" == "1" ]] || [[ "$cmd" == *"--remote-control"* ]] || cmd="$cmd --remote-control"
    # Default model: opus 4.7 with 1M-context. Override per-session with --model
    # on amux register, or globally with $AMUX_DEFAULT_MODEL.
    local default_model="${AMUX_DEFAULT_MODEL:-claude-opus-5[1m]}"
    [[ "$cmd" == *"--model"* ]] || cmd="$cmd --model $(printf '%q' "$default_model")"
  fi
```

Two changes wrapped together:
1. **Auto-append `--remote-control`** — every claude launch under amux now also gets Claude Code's own remote-control web URL (claude.ai/code/...). amux's dashboard remains the primary control surface; this is an extra handle for driving a single session from a browser. Set `AMUX_NO_REMOTE_CONTROL=1` to skip.
2. **Default model = `claude-opus-5[1m]`** (Opus 5 with 1M context window) instead of upstream's `sonnet`. Brackets are escape-safe via `printf '%q'`. Override per-session by passing `--model X` to `amux register`, or globally with `AMUX_DEFAULT_MODEL`.

Both apply only to the claude provider — the codex branch is untouched.

### Hunk 6 verification

```bash
# After deploy, verify the new defaults appear in cmd_start's command-build path:
grep -nE "(remote-control|AMUX_DEFAULT_MODEL|claude-opus-4-7\[1m\])" /usr/local/bin/amux
```

Should match 3 lines: the `--remote-control` line, the `AMUX_DEFAULT_MODEL` lookup line, and the comment referencing the model id.

Quick logic check (no real claude launch needed):
```bash
bash <<'T'
cmd="claude"
[[ "${AMUX_NO_REMOTE_CONTROL:-}" == "1" ]] || [[ "$cmd" == *"--remote-control"* ]] || cmd="$cmd --remote-control"
default_model="${AMUX_DEFAULT_MODEL:-claude-opus-5[1m]}"
[[ "$cmd" == *"--model"* ]] || cmd="$cmd --model $(printf '%q' "$default_model")"
echo "$cmd"
T
# expect: claude --remote-control --model claude-opus-4-7\[1m\]
```

### Hunk 7 — `parse_claude_flags`: `--yolo` (`--dangerously-skip-permissions`) ON by default

amux sessions run unattended inside tmux, so Claude Code's permission prompts deadlock them. Make `--dangerously-skip-permissions` the default for every new session, with explicit opt-outs.

Two parts:

**7a.** Add a `--no-yolo` case to the boolean-flag section of `parse_claude_flags`. Insert immediately after the existing `--yolo|--verbose|...)` boolean-flag block:

```bash
      # Opt-out of the implicit yolo default (see end of parse_claude_flags).
      # Use this when you explicitly want Claude Code's normal permission prompts.
      --no-yolo)
        CC_NO_YOLO=1
        shift
        ;;
```

**7b.** Just before the final `CC_FLAGS_STR="${CC_FLAGS[*]:-}"` at the end of `parse_claude_flags`, insert the default-injection block:

```bash
  # Default: enable --dangerously-skip-permissions (a.k.a. --yolo). amux sessions
  # run unattended inside tmux, so permission prompts deadlock them. Opt out per
  # session with --no-yolo, or globally with AMUX_NO_YOLO=1.
  if [[ "${CC_NO_YOLO:-}" != "1" && "${AMUX_NO_YOLO:-}" != "1" ]]; then
    local already_yolo=0 f
    for f in "${CC_FLAGS[@]:-}"; do
      case "$f" in
        --dangerously-skip-permissions|--allow-dangerously-skip-permissions) already_yolo=1; break ;;
      esac
    done
    [[ $already_yolo -eq 0 ]] && CC_FLAGS+=("--dangerously-skip-permissions")
  fi
```

Help text update (line ~1075 area): note `(default ON)` and add a `--no-yolo` row.

### Hunk 7 verification

```bash
# Default ON
amux register _y1 --dir /tmp >/dev/null && grep CC_FLAGS ~/.amux/sessions/_y1.env
# expect: CC_FLAGS="--dangerously-skip-permissions"

# --no-yolo opts out
amux register _y2 --no-yolo --dir /tmp >/dev/null && grep CC_FLAGS ~/.amux/sessions/_y2.env
# expect: CC_FLAGS=""

# AMUX_NO_YOLO env opts out
AMUX_NO_YOLO=1 amux register _y3 --dir /tmp >/dev/null && grep CC_FLAGS ~/.amux/sessions/_y3.env
# expect: CC_FLAGS=""

# Explicit --yolo not duplicated
amux register _y4 --yolo --verbose --dir /tmp >/dev/null && grep CC_FLAGS ~/.amux/sessions/_y4.env
# expect: CC_FLAGS="--dangerously-skip-permissions --verbose"

for s in _y1 _y2 _y3 _y4; do amux rm $s; done
```

## Known limitations

- ~~Dashboard-created sessions don't get auto-detect.~~ **Resolved 2026-06-12:** `amux-server.py` now auto-detects at every `start_session()` (explicit `CC_CONFIG_DIR` in the env file still wins), and POST `/api/sessions` accepts an optional `config_dir` field (`"work"`, `"personal"`, or an absolute path).
- **Existing session env files without `CC_CONFIG_DIR`** now get the auto-detect at start time (work for `$AMUX_WORK_PATHS` dirs, personal otherwise) instead of falling through to `$HOME`. Pin a session to a specific account by adding `CC_CONFIG_DIR="<path>"` to its `~/.amux/sessions/<name>.env`.
- **First start after switching accounts starts fresh.** Transcripts written under `~/.claude/projects` before the routing landed aren't visible from `~/.claude-work` / `~/.claude-personal`, so the first routed start of an old session can't `--resume` its previous conversation.

## Re-running the deploy

After editing `amux` in this fork:

```bash
bash -n amux                         # syntax — and that is the whole deploy
```

`/usr/local/bin/amux` is a symlink into this worktree: a saved, syntax-clean `amux` IS
the deployed CLI. There is nothing to copy, and copying would break the symlink. Do not
run `install.sh` — it writes both the CLI and the server into place and would overwrite
the live Rust server.
