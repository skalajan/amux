#!/bin/bash
# SessionStart hook: say out loud, at the one moment it matters, whether this
# session is about to build on something stale.
#
# Two DIFFERENT staleness axes bit in one session on 2026-08-05, which is why
# this checks both:
#
#   1. The CHECKOUT was ~110 commits behind origin/main. Work got built on a
#      stale base; one fix turned out to duplicate a fix upstream already had,
#      and the rebase that followed conflicted twice.
#   2. The INSTALLED CLI (~/.local/bin/amux) was a Jul-31 copy missing the
#      `status-update` verb. It fell through to help and exited 0, so three of
#      the owner's status requests were silently swallowed (AMUX-2140 shape).
#
# Design constraints, each one deliberate:
#
#   * It FETCHES, never pulls. This is a shared checkout — CLAUDE.md records a
#     peer's `git pull --rebase` replaying another session's unpushed commit
#     onto origin. A hook that rewrites the working tree can destroy in-flight
#     work belonging to a session that is not even running right now. Report
#     and recommend; the human decides (ethos rule 8).
#   * It FAILS OPEN. Offline, no remote, detached HEAD, missing files — every
#     failure path exits 0 silently. A freshness check that blocks a session is
#     worse than the staleness it detects.
#   * It is SILENT when everything is current, so the one time it speaks is
#     signal rather than another banner to scroll past.
set -uo pipefail

[ "${AMUX_SKIP_FRESHNESS:-}" = "1" ] && exit 0

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd -P)" || exit 0
cd "$REPO" 2>/dev/null || exit 0
git rev-parse --git-dir >/dev/null 2>&1 || exit 0

out=""

# ── Axis 1: is the checkout behind its remote? ───────────────────────────────
# Bounded: a hook that hangs on a slow network is a hook that gets deleted.
if git remote get-url origin >/dev/null 2>&1; then
  timeout 10 git fetch -q origin 2>/dev/null
  base="$(git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null || echo origin/main)"
  if git rev-parse --verify -q "$base" >/dev/null 2>&1; then
    behind="$(git rev-list --count "HEAD..$base" 2>/dev/null || echo 0)"
    if [ "${behind:-0}" -gt 0 ]; then
      # Name the files that actually matter here, not just a number: "110
      # commits behind" reads as bookkeeping, "crates/ changed upstream"
      # reads as "your edit is going to conflict".
      #
      # THREE dots, and the distinction is the whole point of this line. In
      # `git diff`, two dots compare the two ENDPOINTS — so on a shared checkout
      # carrying unpushed work it reports OUR OWN files as upstream changes.
      # Measured 2026-08-09 (python era): 1 commit behind touching only the
      # server file, and the two-dot form also named `CLAUDE.md amux`, sending
      # the session to reconcile two files upstream had never touched. Three dots
      # diff from the merge-base, i.e. exactly "what $base added that I lack".
      # Note line 43 is correct as-is: two-dot rev-list already means that.
      # The bug was that one sentence mixed both conventions, so its count and
      # its file list disagreed — and it degraded precisely as the checkout got
      # busier, which is when the warning matters most.
      hot="$(git diff --name-only "HEAD...$base" 2>/dev/null \
             | grep -E '^(crates/|Cargo\.(toml|lock)$|amux$|CLAUDE\.md$)' \
             | head -6 | tr '\n' ' ')"
      out+="  - checkout is ${behind} commit(s) behind ${base}"
      [ -n "$hot" ] && out+=" — including: ${hot}"
      out+=$'\n'
      out+=$'    git pull --rebase origin main   (review first: this checkout is SHARED)\n'
    fi
  fi
fi

# ── Axis 2: does what is INSTALLED match this checkout? ──────────────────────
# The repo copy is the source; install.sh copies it. Editing the repo alone
# changes nothing that a session or the dashboard actually executes.
live_cli="$(command -v amux 2>/dev/null || true)"
if [ -n "$live_cli" ] && [ -f "$REPO/amux" ]; then
  if ! diff -q "$REPO/amux" "$live_cli" >/dev/null 2>&1; then
    out+="  - installed CLI differs from this checkout: ${live_cli}"$'\n'
    out+="    an unknown verb there may print help and exit 0 — a silent no-op"$'\n'
    out+="    cp \"$REPO/amux\" \"$live_cli\""$'\n'
  fi
fi

# The RUNNING server's freshness is the builder's job, not this hook's:
# com.amux.server-rs-builder rebuilds COMMITTED rust source every 60s and the
# server self-adopts the new binary. A file diff cannot compare a binary to a
# source tree, but /health's `build` hash names exactly which build answers —
# so report only when the running server looks stale relative to the checkout.
if command -v curl >/dev/null 2>&1; then
  hs="$(timeout 5 curl -sk https://localhost:8824/health 2>/dev/null || true)"
  if [ -n "$hs" ] && ! printf '%s' "$hs" | grep -q '"server":"amux-rust"'; then
    out+="  - https://localhost:8824/health is answering but not as amux-rust — check com.amux.server-rs"$'\n'
  fi
fi

[ -z "$out" ] && exit 0

printf 'amux freshness — this session may be building on something stale:\n\n%s\n' "$out"
printf 'Reconcile before starting work, or say so in your first message if you are deliberately not.\n'
exit 0
