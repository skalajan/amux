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

| Area | Tracked file(s) | Grep landmarks (unique-to-local) | Sentinel status | Reapply-hunk anchor | Resolution note | Upstreamable? |
|---|---|---|---|---|---|---|
| Account routing / multi-home | `amux-server.py` | `_pick_claude_config_dir`, `_session_claude_config_dir`, `_claude_config_homes`, `_claude_project_dir`, `AMUX_WORK_PATHS` | **Retrofitted** — `AMUX-LOCAL:account-routing` sentinel open at `amux-server.py:1643`, close at `:1776` (verified: `grep -c AMUX-LOCAL amux-server.py` = 2) | Self-contained function block `_pick_claude_config_dir` → `_cc_session_id_for_name` (1643–1776); bounded by the `AMUX-LOCAL:account-routing` sentinel, not a hunk positioned on an upstream landmark — merges via git's own 3-way merge | Keep `_pick_claude_config_dir`, `_session_claude_config_dir`, `_claude_config_homes`, `_claude_project_dir` — keep local one-liners like `project_dir = _claude_project_dir(...)` over upstream's `CLAUDE_HOME / "projects" / ...` | N (personal Fidoo work-path defaults; mechanism not pursued upstream) |
| Token stats — multi-home iteration | `amux-server.py` | `# Sessions may run under any Claude config home (work/personal routing)` (comment inside `_refresh_token_cache`) | Not sentinel-wrapped (lazy retrofit — gate-tracked by grep landmark only) | `_refresh_token_cache` (upstream-owned function name; local delta is the `_claude_config_homes()` iteration grafted inside it) | Adopt upstream's token-stats logic, but iterate all config homes (`_claude_config_homes()`) instead of just `~/.claude` | N (depends on account-routing existing upstream first) |
| `start_session()` cmd build — `CLAUDE_CONFIG_DIR` prefix | `amux-server.py` | `CLAUDE_CONFIG_DIR={shlex.quote(claude_config_dir)}`, `Env-prefix selects the Claude.ai account` | Not sentinel-wrapped (lazy retrofit) | `cmd = _custom_claude or "claude"` (upstream-owned base command build in `start_session()`, ~line 10823) | Keep the `CLAUDE_CONFIG_DIR=` env prefix wrapped around whatever base command upstream uses | N (depends on account-routing) |
| `POST /api/sessions` — `config_dir` block | `amux-server.py` | `config_dir = str(body.get("config_dir", "")).strip()` | Not sentinel-wrapped (lazy retrofit) | `creator = body.get("creator", "").strip()` (upstream-owned field immediately above, ~line 45980) | Keep the `config_dir` (`"work"`/`"personal"`/path) block | **Y** — generic `config_dir` field, strongest PR candidate |
| Stub-writer guard | `amux-server.py` | `Self-update only when the installed file IS the stub. Never follow` | Not sentinel-wrapped (lazy retrofit) | `_sync_skills_and_cli` stub-install block (upstream-owned function; local delta is the symlink + content-marker guard grafted inside, ~lines 6023–6035) | Keep the symlink + content-marker guard (`stub_path.is_symlink()` check + `"amux CLI stub"` marker match) around whatever upstream does with the stub-install block — never let it write through a symlink or overwrite a non-stub file | N (fork-specific incident fix tied to this host's `/usr/local/bin/amux` symlink install layout) |
| `AMUX_COMMIT_STAMP` toggle | `amux-server.py` | `AMUX_COMMIT_STAMP` | Not sentinel-wrapped (lazy retrofit) | `_commit_stamp_enabled` / `_install_amux_commit_hook` (upstream-owned commit-stamp hook; local delta is the on/off toggle inside) | Keep the `AMUX_COMMIT_STAMP` toggle in the commit-stamp hook install/enable logic — graft around whatever upstream changes to the hook-install mechanism | **Y** — strongest PR candidate |
| `amux` CLI — config-dir flags & auto-detect (Hunks 1–5) | `amux` | `pick_default_config_dir`, `AMUX_WORK_PATHS`, `CC_CONFIG_DIR` | Not sentinel-wrapped (lazy retrofit; bash CLI, not amux-server.py — same policy applies) | `parse_claude_flags`, `cmd_register`, `cmd_exec`, `cmd_start` shell_setup block (see Hunks 1–5 below) | Keep the MODIFICATIONS.md hunks (config-dir flags, auto-detect + persist) AND upstream's new commands — both sides | N (tied to account-routing feature, not pursued upstream) |
| `amux` CLI — remote-control/model defaults & yolo-by-default (Hunks 6–7) | `amux` | `AMUX_NO_REMOTE_CONTROL`, `AMUX_DEFAULT_MODEL`, `claude-opus-5[1m]`, `AMUX_NO_YOLO` | Not sentinel-wrapped (lazy retrofit) | `cmd_start` claude-branch, `else` of the `provider == "codex"` check (see Hunks 6–7 below) | Keep the MODIFICATIONS.md hunks (yolo default, default model, remote-control) AND upstream's new commands — both sides | N (personal defaults — Opus-1M, yolo, remote-control — not pursued upstream) |
| Localhost write-auth token | `amux-server.py`, `amux` | `X-Amux-Write-Token`, `_WRITE_TOKEN`, `_write_auth_ok`, `_is_write_path`, `_load_or_create_write_token`, `_WRITE_TOKEN_FILE`, `CC_WRITE_TOKEN`, `AMUX_WRITE_TOKEN`, `AMUX-LOCAL:write-auth` | Sentinel-wrapped (`AMUX-LOCAL:write-auth` in `amux-server.py`; grep-tracked in `amux`) | Helpers block after `_PUBLIC_PREFIXES = (...)`; hoisted gate = first logic in `_check_auth` (above both early-returns); dashboard `_AMUX_WRITE_TOKEN` inject after `window._AMUX_UI_TOKEN`; `_writeToken`/`_authHeaders` in the auth `<script>`; stub `AMUX_WRITE_TOKEN=` after `AMUX_URL=` in the `_amux_stub` heredoc; startup self-check after `_sync_skills_and_cli()`; `amux` CLI `CC_WRITE_TOKEN=` after `CC_HOME=` + `-H "X-Amux-Write-Token: $CC_WRITE_TOKEN"` on write curls | Keep the hoisted write-auth gate ABOVE `if not AUTH_TOKEN` and the localhost bypass; keep the independent 0600 `~/.amux/write_token` secret (never derive from AUTH_TOKEN); keep `X-Amux-Write-Token` on every HTTP write consumer (stub, CLI, dashboard, procwarden, skills). Only the file secret or `Bearer AUTH_TOKEN` (when set) grant a write — never `_UI_TOKEN`/derived. Graft around whatever upstream changes to `_check_auth` | **Y** — generic localhost write-auth; strong upstream PR candidate |
| Session chat (Scope B1 core + B2 dashboard tab + reply-summary marker) | `amux-server.py`, `chat.js`, `chat.css`, `amux-telegram.py`, `docs/reply-summary.md` | `chat_messages`, `chat_replies`, `_chat_extract_turns`, `_chat_populate_replies`, `_chat_build_thread`, `_chat_insert_owner`, `_chat_insert_system`, `_chat_delivery_status`, `_chat_reconcile_all`, `_chat_populate_replies_throttled`, `_chat_schedule_post_delivery_populate`, `_chat_notify`, `_chat_replies_lock`, `_chat_limit_notified`, `_chatOnSSE`, `_chatPoll`, `_chatTabOpen`, `_chatTabClose`, `peek-tab-chat`, `peek-chat-panel`, `AMUX-LOCAL:session-chat`, `/api/chat`, `/chat.js`, `/chat.css`, `_chat_parse_summary_marker`, `_CHAT_SUMMARY_MARKER`, `_chat_summarize_text`, `_chat_summary_tick`, `_chat_summary_worker_loop`, `AMUX_SUMMARY_MODEL`, `AMUX_SUMMARY_TIMEOUT`, `AMUX_SUMMARY_DISABLE`, `chat-expand`, `_chat_resolve_jsonl_path`, `_chat_live_conv_path`, `_chat_conv_fallback_cache`, `_CHAT_CONV_FALLBACK_TTL`, `_chat_owned_conv`, `_chat_conv_jsonl` | Sentinel-wrapped (balanced `AMUX-LOCAL:session-chat` block fences — Python `#`, JS `//`, SQL `--`, HTML `<!-- -->` — plus a few open-only inline markers matching B1 style; `grep -c AMUX-LOCAL:session-chat` = 51, up from 37 — the reply-summary marker parser + Haiku worker + migration line + startup thread-start + the client-side `summary`-kind SSE handling all landed inside new/extended fences; the freshness/poll-flood fixes are net +1 (a new `AMUX-LOCAL:session-chat` fence pair around the post-delivery populate call in `_steer_try_deliver`, minus the inline `_chatOnSSE` summary marker that moved to `chat.js`); +2 in the dead-slot-leak fix, which fenced AMUX-10's `_chat_live_conv_path`/`_chat_resolve_jsonl_path` block — it landed unfenced). **B2 feature code lives in referenced files `chat.js`/`chat.css` (upstream has neither → conflict-immune); the in-file B2 footprint is ~15 functional lines.** | **B1:** schema tables in `_DB_SCHEMA` after `steering_history` (`--` fence); helper block after `get_db()` (`#`); `POST/GET /api/chat` in `_route_inner` after `/api/tunnel/stop` (GET calls `_chat_populate_replies_throttled(session)` before `_chat_build_thread` — Bug-1 freshness hook for BOTH dashboard + Telegram); `_classify_request` chat entry before `# System`; monitor idle-hook `_chat_populate_replies(sname)` inside `_on_idle`; fenced `_chat_schedule_post_delivery_populate(name)` in `_steer_try_deliver`'s `if ok:` success block (Bug-1: +5s/+15s/+30s deferred populate after a delivered steer); the two throttle/schedule helpers sit right after `_chat_populate_replies`; **AMUX-10 (fresh-session capture):** `_chat_populate_replies` resolves its transcript via `_chat_resolve_jsonl_path` (not `_session_jsonl_path` directly) — meta path primary, then `_chat_live_conv_path` fallback (defined just above `_chat_populate_replies`) which resolves the live conv id via `_live_conv_id` (running-process argv → newest jsonl across ALL config homes) and reconstructs the path, so fresh sessions with no recorded `cc_conversation_id` and transcripts under a non-CLAUDE_HOME config home (mac-server `~/.claude-personal`) are captured; the resolved id is cached in-memory in `_chat_conv_fallback_cache` (TTL `_CHAT_CONV_FALLBACK_TTL`) so `_live_conv_id`'s ps/tmux runs at most once per TTL per session, and NOT persisted to meta (a wrong shared-dir guess self-corrects next poll, never cements into resume); SSE `chat` emit after the alerts push + reconcile-on-connect before the `_sse_events` `while True`; usage-limit `_chat_insert_system` in `_rate_limit_auto_respond`; dashboard `_chatOnSSE` (now dispatch-only) `/_chatPoll` block + fallback wiring in `enablePollingFallback`. **Bug-2 poll-flood fix lives in `chat.js`** (its `amux:chat` listener owns the session-filter + trailing-edge <=1/2s throttle + `summary`-kind cursor-reset that used to be inline in `_chatOnSSE`). **B2:** `<link href="/chat.css">` after the xterm CSS `<link>`; `<script src="/chat.js" defer>` after the fullcalendar `<script>`; `peek-tab-chat` button after `peek-tab-terminal` in `.peek-tabs`; `<div id="peek-chat-panel">` after `peek-messages-panel`; chat dispatch appended in `setPeekTab` after the notes-panel block; `_chatTabClose()` call in `closePeek` after `peekSession = null`; `/chat.js`+`/chat.css` static route after the `/sw.js` route; `/chat.js`+`/chat.css` added to `_PUBLIC_PATHS`. **Reply-summary:** `summary` column + migration line grafted onto `chat_replies`' `CREATE TABLE` and the `_init_db()` migrations list; `_chat_parse_summary_marker` immediately before `_chat_extract_turns` (which now calls it per turn); `_chat_populate_replies`' INSERT and `_chat_build_thread`'s SELECTs extended with `summary`; the Haiku worker (`_chat_summary_env`/`_chat_summarize_text`/`_chat_summary_tick`/`_chat_summary_worker_loop`) block appended right before the `/AMUX-LOCAL:session-chat` close after `_chat_build_thread`; its daemon-thread start next to `_install_hooks_all_sessions` in the startup sequence; the `kind === 'summary'` cursor-reset branch now lives in `chat.js`'s `amux:chat` listener (moved out of the inline `_chatOnSSE` with the Bug-2 fix); `chat.js`'s `.chat-expand` affordance + `_isCollapsible`/`_expanded` state wired into `_bodyHtml`/`_msgHtml`/`_buildShell`/`_open`; `amux-telegram.py`'s `_render_outbound` server-summary branch before its local-summarizer call. | **B1:** keep `chat_messages` an OWNER/SYSTEM-input-only log (no `delivered` column, no `role='session'` rows); keep `chat_replies` a rebuildable materialized index (rebuild via `DELETE FROM chat_replies`, NEVER `DROP TABLE` — C-crit-2); keep single-writer `_chat_populate_replies` under `_chat_replies_lock` inserting in ascending `turn_index` (C-new-1); keep the `isSidechain` subagent filter in the pure `_chat_extract_turns`; owner input goes through `POST /api/chat` → `_steer_enqueue` only, never `cmd_history`; `GET /api/chat` stays read-loose (peek parity); keep the `chat` SSE type in BOTH `_sse_events` and the dashboard polling fallback; keep the AMUX-10 live-conv-id fallback in the orchestration layer (`_chat_resolve_jsonl_path`/`_chat_live_conv_path`), leaving `_live_conv_id` and `_session_jsonl_path` untouched (pure/meta-primary), and keep the resolved id in-memory only (`_chat_conv_fallback_cache`), never persisted to meta; keep `_chat_live_conv_path`'s four-step resolution ladder in that order — `is_running` gate, then argv (`_live_conv_id(name)` with NO work dir), then sticky `_chat_owned_conv`, and only then the `_live_conv_id(name, wd)` guess. Step 4 is racy by construction (its step 2 is "newest jsonl in the work dir"), so in a CC_DIR shared with any other Claude it resolves to whichever conversation was written last — including a bare `claude` CLI amux does not own — and the stolen turn is written to chat_replies AND pushed to Telegram. Both `~/Desktop/Projects/amux` slots were hit on 2026-08-01: `--help` (dead, 7/7 rows stolen → step 1) and `amux-helper` (LIVE, argv carries only `--name`, meta has no `cc_conversation_id` because it resumes a conversation born 2026-07-16, 10 rows stolen beside its own 92 → step 3). `_chat_owned_conv` must rank by ROW COUNT not recency — a stolen turn is one row while the real conversation has hundreds, so "most recent" would hand the slot straight back to the thief. The whole ladder belongs INSIDE the cache-miss branch so it stays TTL-bounded like the `_live_conv_id` call it guards (and for dead slots is cheaper than what it replaced: one `tmux list-sessions` vs list-panes + pgrep + ps-per-pid + a work-dir scan). Accepted residual: a brand-new slot with no argv id and no history still rides step 4, and step 3 pins a slot to its old conversation if it starts a new one without an argv id — pinned-but-stale beats stealing. The durable cure is passing `--session-id` at launch (the `amux` bash CLI does not, which is why `amux-helper` had no authoritative id at all). **B2:** keep the UI in `chat.js`/`chat.css` (referenced files) with only the tiny sentinel footprint in-file; `chat.js` consumes the B1 `amux:chat-thread`/`amux:chat` CustomEvents + `window._chatActiveSession`/`_chatCursor`/`_chatPoll` (merge-by-id dedups SSE+poll overlap); composer writes via `apiCall` → `POST /api/chat` carrying `X-Amux-Write-Token` (never bypass Scope A); the chat tab is a new peek-overlay tab (Chat) beside Terminal/Messages — raw terminal stays in Terminal. **Reply-summary:** keep `_chat_parse_summary_marker` pure/deterministic so a `chat_replies` rebuild re-derives `summary` identically from the transcript (no separate persistence path); keep the Haiku worker best-effort and never-blocking — ANY failure leaves `summary` NULL, backed off via the in-memory (not persisted) `_chat_summary_failed` map, never crashes capture/delivery; keep it single-flight via the dedicated sleep-loop thread (not `schedule_job`, which would overlap invocations against a slow subprocess); keep the client's `kind === 'summary'` SSE branch (a full cursor-reset refetch) — an incremental `since=` poll can never see an UPDATE to an already-delivered `rowid_seq`; keep `amux-telegram.py` preferring the server `summary` over its own local `claude -p` call, falling back to it only when absent. | **Y** — generic per-session chat + reply-summary over the existing steering/SSE surface; upstreamable |

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
- `amux-server.py` — account routing in `start_session()` + cross-home lookups + stub-writer guard + restart-race fix

## How to reapply after `git pull` from upstream

Re-run the 5 hunks below against the freshly pulled `amux` file, then deploy:

```bash
bash -n amux && cp amux /usr/local/bin/amux
```

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
bash -n amux                         # syntax
cp amux /usr/local/bin/amux          # deploy CLI only
# (don't run ./install.sh unless you also want to push amux-server.py)
```

`install.sh` deploys **both** the CLI and the server. If your `amux-server.py` has unrelated local edits that aren't ready, use the explicit `cp` instead.
