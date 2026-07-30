#!/usr/bin/env bash
# amux-pull-update.sh — pull-only auto-update runner for mac-server (Decision 1A
# + 2A + D1-A in .omc/plans/mac-server-deploy.md). Invoked on a launchd
# StartInterval cadence by com.amux.pull-update (rendered from
# com.amux.pull-update.plist.tmpl by install.sh). NOT installed/loaded by this
# repo — see docs/mac-server-deploy.md. Runs FROM the tracked checkout (never a
# copy elsewhere) so a pull that changes this very script takes effect next
# cadence.
#
# Contract (do not weaken without re-reading the plan's Decision D1 + the three
# mandatory executor notes at the top of .omc/plans/mac-server-deploy.md):
#   1. Every pathspec this script feeds to git goes through --pathspec-from-file
#      (fed one pattern per invocation — see "Reconcile" below for why one-at-
#      a-time, not the whole allowlist file in a single call) or a literal bash
#      array element. NEVER an unquoted `$(cat allowlist)` — that lets the
#      shell glob against the CURRENT worktree, silently dropping any path
#      that's been deleted, which is exactly the dirt case this script exists
#      to reconcile (a staged deletion is the textbook trigger).
#   2. Pull-only. This script must never push. Enforced by the git() wrapper
#      below, not just by convention.
#   3. Tracked dirt OUTSIDE the allowlist aborts the pull and raises a loud
#      local alert. It is never force-resolved.
#   4. This script does not decide whether it should be running at all — that
#      gate (P3.b: allowlist populated from OBSERVED git status, not the seed
#      candidates alone) lives in whether com.amux.pull-update is loaded, which
#      is entirely install.sh's / the operator's responsibility, never this
#      script's.
set -euo pipefail

# ── git() wrapper — hard block on push, no matter what calls it ────────────
# This is a real runtime guard, not just a comment: every "git" call below
# (and any future one added to this file) is routed through this function.
git() {
  if [[ "${1:-}" == "push" ]]; then
    log "REFUSING: attempted 'git push' — amux-pull-update.sh is pull-only"
    return 1
  fi
  command git "$@"
}

log() {
  printf '[%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"
}

# ── Self-locate the checkout root ───────────────────────────────────────────
# This script lives at <checkout>/deploy/mac-server/amux-pull-update.sh — two
# dirs up is the checkout root. No @@CHECKOUT@@ templating needed here: the
# plist points ProgramArguments at this file's real (checkout) path, and this
# resolves relative to wherever that copy of the file actually is — which,
# because install.sh symlinks/points at the checkout (never a copy), is always
# the live tracked checkout.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
ALLOWLIST="$ROOT/deploy/mac-server/pull-reconcile-allowlist.txt"
AMUX_STATE_DIR="${CC_HOME:-$HOME/.amux}"

cd "$ROOT"

if [[ ! -d .git ]]; then
  log "FATAL: $ROOT is not a git checkout — aborting"
  exit 1
fi

ERR_TMP="$(mktemp)"
trap 'rm -f "$ERR_TMP"' EXIT

# ── Alerting (best-effort, never fatal) ─────────────────────────────────────
# "Loud, locally": always logged (launchd captures stdout/stderr to
# ~/.amux/logs/pull-update.{out,err}.log); additionally best-effort a board
# task via the server's own localhost API, and a best-effort Telegram note via
# the server's own bot credentials (~/.amux/telegram.env), if configured.
# Neither of these ever raises — a failed alert must not mask the underlying
# abort, and must not itself crash the runner.
_alert_board() {
  local subject="$1" body="$2"
  local tokfile="$AMUX_STATE_DIR/write_token"
  [[ -r "$tokfile" ]] || { log "alert: no write_token at $tokfile — skipping board task"; return 0; }
  local token payload
  token="$(cat "$tokfile" 2>/dev/null || true)"
  [[ -n "$token" ]] || { log "alert: empty write_token — skipping board task"; return 0; }
  payload="$(TITLE="amux-pull-update: $subject" BODY="$body" python3 -c '
import json, os
print(json.dumps({"title": os.environ["TITLE"], "desc": os.environ["BODY"], "status": "todo"}))
' 2>/dev/null || true)"
  [[ -n "$payload" ]] || { log "alert: failed to build board payload — skipping"; return 0; }
  if curl -sk -m 10 -X POST -H 'Content-Type: application/json' \
      -H "X-Amux-Write-Token: $token" -d "$payload" \
      "https://localhost:8822/api/board" >/dev/null 2>&1; then
    log "alert: board task filed"
  else
    log "alert: board task POST failed (server down?) — logged locally only"
  fi
  return 0
}

_alert_telegram() {
  local subject="$1" body="$2"
  local envfile="$AMUX_STATE_DIR/telegram.env"
  [[ -r "$envfile" ]] || return 0
  local tg_token tg_chat
  tg_token="$(grep -E '^TG_BOT_TOKEN=' "$envfile" 2>/dev/null | tail -1 | cut -d= -f2-)"
  tg_chat="$(grep -E '^TG_CHAT_ID='  "$envfile" 2>/dev/null | tail -1 | cut -d= -f2-)"
  [[ -n "$tg_token" && -n "$tg_chat" ]] || return 0
  if curl -sk -m 10 -X POST "https://api.telegram.org/bot${tg_token}/sendMessage" \
      --data-urlencode "chat_id=${tg_chat}" \
      --data-urlencode "text=amux-pull-update: ${subject}$( [[ -n "$body" ]] && printf ' — %s' "$body" )" \
      >/dev/null 2>&1; then
    log "alert: Telegram note sent"
  else
    log "alert: Telegram send failed — logged locally only"
  fi
  return 0
}

alert() {
  local subject="$1" body="${2:-}"
  log "ALERT: $subject${body:+ -- $body}"
  _alert_board "$subject" "$body" || true
  _alert_telegram "$subject" "$body" || true
}

# ── Load the allowlist into a bash array, stripping blanks/comments ─────────
# This filtering happens here in bash via plain string comparison — no glob
# expansion is ever performed on these lines. Each surviving line is later
# handed to git as either a single --pathspec-from-file entry (reconcile) or
# a literal array element (status pathspecs) — never re-interpreted by the
# shell as a filesystem glob.
PATTERNS=()
if [[ -r "$ALLOWLIST" ]]; then
  while IFS= read -r _line || [[ -n "$_line" ]]; do
    _line="${_line#"${_line%%[![:space:]]*}"}"   # trim leading whitespace
    _line="${_line%"${_line##*[![:space:]]}"}"   # trim trailing whitespace
    [[ -z "$_line" ]] && continue
    [[ "$_line" == \#* ]] && continue
    PATTERNS+=("$_line")
  done < "$ALLOWLIST"
fi
log "loaded ${#PATTERNS[@]} allowlist pattern(s) from $ALLOWLIST"

# ── Step 1: pre-pull reconcile ──────────────────────────────────────────────
# Hard-reset each allowlisted pathspec to HEAD (index + worktree), one pattern
# per git invocation. This is deliberately NOT a single
# `git checkout HEAD --pathspec-from-file=<wholefile>` call: empirically (see
# the sandbox test in this directory), git's own --pathspec-from-file is
# all-or-nothing — if ANY one line fails to match (a stale/leaky allowlist
# entry), the ENTIRE call fails and reconciles NOTHING, including every other
# still-valid pattern. Per-pattern invocation isolates a stale entry to a
# logged warning instead of blocking reconciliation of everything else — this
# is what makes the plan's own "allowlist can go stale/leaky, mitigated by the
# loud residual-dirt alert" claim actually true in practice rather than an
# all-or-nothing outage on the first stale line.
if [[ ${#PATTERNS[@]} -gt 0 ]]; then
  for _p in "${PATTERNS[@]}"; do
    if ! git checkout HEAD --pathspec-from-file=<(printf '%s\n' "$_p") >/dev/null 2>"$ERR_TMP"; then
      log "reconcile: pattern '$_p' did not apply ($(tr -d '\n' < "$ERR_TMP" | head -c 200)) — likely stale allowlist entry, continuing"
    fi
  done
fi

# ── Step 2: tripwire (E5 regression guard, mandatory executor note 1) ───────
# Assert the allowlisted paths are actually clean post-reconcile. git status
# has no --pathspec-from-file flag (verified empirically — it errors "unknown
# option"), so the equivalent-safety form here is a literal bash array of
# pathspec words passed as positional args after `--`: each pattern is one
# argv element, never shell-glob-expanded, which is the actual hazard the
# mandatory note guards against (not the specific flag name).
if [[ ${#PATTERNS[@]} -gt 0 ]]; then
  TRIPWIRE_DIRT="$(git status --porcelain --untracked-files=no -- "${PATTERNS[@]}")"
  if [[ -n "$TRIPWIRE_DIRT" ]]; then
    log "tripwire dirt:"
    printf '%s\n' "$TRIPWIRE_DIRT" | while IFS= read -r _l; do log "  $_l"; done
    alert "tripwire FAILED — allowlisted paths still dirty after reconcile" "$(printf '%s' "$TRIPWIRE_DIRT" | tr '\n' ';')"
    exit 1
  fi
  log "tripwire OK — allowlisted paths clean after reconcile"
fi

# ── Step 3: residual-dirt guard (outside the allowlist) ─────────────────────
# Any tracked (non-untracked) dirt NOT covered by an allowlist pattern aborts
# here, before fetch/pull are even attempted. Built from the same PATTERNS
# array via git's `:(exclude)` pathspec magic (verified empirically to honor
# glob patterns the same way --pathspec-from-file does) — never force-resolved.
EXCLUDES=(".")
if [[ ${#PATTERNS[@]} -gt 0 ]]; then
  for _p in "${PATTERNS[@]}"; do
    EXCLUDES+=(":(exclude)$_p")
  done
fi
RESIDUAL_DIRT="$(git status --porcelain --untracked-files=no -- "${EXCLUDES[@]}")"
if [[ -n "$RESIDUAL_DIRT" ]]; then
  log "residual dirt outside allowlist:"
  printf '%s\n' "$RESIDUAL_DIRT" | while IFS= read -r _l; do log "  $_l"; done
  alert "outside-allowlist divergence — pull ABORTED, tree left untouched" "$(printf '%s' "$RESIDUAL_DIRT" | tr '\n' ';')"
  exit 1
fi
log "residual-dirt guard OK — no tracked dirt outside the allowlist"

# ── Step 4: fetch + divergence check (informational) ───────────────────────
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$BRANCH" == "HEAD" ]]; then
  alert "checkout is in detached HEAD state — cannot pull" ""
  exit 1
fi

if ! git fetch origin; then
  alert "git fetch origin failed" ""
  exit 1
fi

if git rev-parse --verify -q "origin/$BRANCH" >/dev/null; then
  AHEAD_BEHIND="$(git rev-list --left-right --count "HEAD...origin/$BRANCH" 2>/dev/null || echo '? ?')"
  log "divergence (local ahead / behind origin/$BRANCH): $AHEAD_BEHIND"
fi

PREV_HEAD="$(git rev-parse HEAD)"

# ── Step 5: pull --ff-only ───────────────────────────────────────────────────
if ! git pull --ff-only origin "$BRANCH"; then
  alert "git pull --ff-only failed (history diverged?)" "local HEAD=$PREV_HEAD"
  exit 1
fi

NEW_HEAD="$(git rev-parse HEAD)"
if [[ "$NEW_HEAD" == "$PREV_HEAD" ]]; then
  log "already up to date at $PREV_HEAD — nothing to do"
  exit 0
fi

log "pulled $PREV_HEAD -> $NEW_HEAD"

# ── Step 6: changed-file -> service map ─────────────────────────────────────
CHANGED="$(git diff --name-only "$PREV_HEAD" "$NEW_HEAD")"
log "changed files:"
printf '%s\n' "$CHANGED" | while IFS= read -r _l; do [[ -n "$_l" ]] && log "  $_l"; done

BOUNCE_TELEGRAM=0
SAW_SERVER=0
while IFS= read -r _f; do
  case "$_f" in
    amux-telegram.py) BOUNCE_TELEGRAM=1 ;;
    amux-server.py) SAW_SERVER=1 ;;
  esac
done <<< "$CHANGED"

if [[ "$SAW_SERVER" -eq 1 ]]; then
  log "amux-server.py changed — no action needed, the server's own mtime watcher (_watch_self) will os.execv-restart it"
fi

if [[ "$BOUNCE_TELEGRAM" -eq 1 ]]; then
  UID_NUM="$(id -u)"
  if launchctl kickstart -k "gui/${UID_NUM}/com.amux.telegram" >"$ERR_TMP" 2>&1; then
    log "amux-telegram.py changed — kickstarted com.amux.telegram"
  else
    log "warn: amux-telegram.py changed but kickstart of com.amux.telegram failed (is it loaded?): $(tr -d '\n' < "$ERR_TMP" | head -c 200)"
  fi
fi

log "done"
exit 0
