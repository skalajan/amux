#!/usr/bin/env bash
# Auto-build for the Rust server (the "server adopts every change" seam).
#
# Run by com.amux.server-rs-builder every 60s: when the committed Rust
# source has moved since the last successful build, rebuild release and
# install the binary; the running server notices its own binary changed and
# exits for launchd to relaunch (self-adoption in amux-server/src/lib.rs).
#
# COMMITTED source only — building the working tree would ship half-typed
# code from any session on this shared checkout. A commit is the unit of
# "there is a change to adopt", mirroring how the Python server's file-save
# reload is bounded by whole-file saves.
set -euo pipefail

# launchd does NOT inherit the shell PATH (the restic lesson in ~/Dev/CLAUDE.md
# — same class, same fix): name the toolchain absolutely.
export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin"

# The repo is wherever this script lives (scripts/ under the checkout), so a
# clone installed via ./install.sh builds ITSELF rather than a hardcoded
# developer path. Env overrides exist for the temp-prefix install self-test.
REPO="${AMUX_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
INSTALL="${AMUX_RS_INSTALL:-$HOME/.local/bin/amux-server-rs}"
STAMP="${AMUX_RS_BUILD_STAMP:-$HOME/.amux/rust-build-stamp}"
LOG="${AMUX_RS_BUILD_LOG:-$HOME/.amux/logs/rust-auto-build.log}"
mkdir -p "$(dirname "$LOG")" "$(dirname "$INSTALL")"

LOCK="${AMUX_RS_BUILD_LOCK:-$HOME/.amux/rust-build.lock}"

# ONE cleanup handler, set once. A second `trap ... EXIT` REPLACES the first
# rather than adding to it, so the worktree trap that used to live inside the
# block below would have silently discarded any lock release installed before
# it — the lock would leak on every run and the second invocation would be
# blocked forever. Everything that needs unwinding goes here.
cleanup() {
  if [ -n "${WORK:-}" ]; then
    git -C "$REPO" worktree remove --force "$WORK" 2>/dev/null || true
    rm -rf "$WORK"
  fi
  [ -n "${BUILD_OUT:-}" ] && rm -f "$BUILD_OUT"
  [ -n "${LOCK_HELD:-}" ] && rm -rf "$LOCK"
  return 0   # a falsey last test must not make the trap itself fail under set -e
}
trap cleanup EXIT

head=$(git -C "$REPO" log -1 --format=%H -- crates/ Cargo.toml Cargo.lock 2>/dev/null || echo none)
last=$(cat "$STAMP" 2>/dev/null || echo "")
[ "$head" = "$last" ] && exit 0

# ── SINGLE-INSTANCE LOCK (AMUX-2927) ────────────────────────────────────────
# Two invocations — the 60s launchd cycle and a human running this by hand —
# built 68d7114 simultaneously and one died with E0432 on a half-evicted
# artifact. Cargo's own build lock is NOT what was missing: it already
# serialises concurrent `cargo build`s (measured in CLAUDE.md: two
# concurrent incremental builds finish in 1.65s vs 1.48s alone, because the
# second waits and then finds the work done). What is outside that lock is the
# DISK GUARD's `rm -rf` of the shared target dir — one invocation can delete
# the tree the other is mid-build against, which is exactly what an
# unresolved-import error on a vanished rlib looks like.
#
# So the lock must cover the guard and the build TOGETHER, which is why it
# wraps the whole block rather than just the cargo call.
#
# mkdir, not flock: flock is a Linux utility and does not exist on macOS, where
# this runs. mkdir is atomic on POSIX and needs no helper binary.
mkdir -p "$(dirname "$LOCK")"
if ! mkdir "$LOCK" 2>/dev/null; then
  owner=$(cat "$LOCK/pid" 2>/dev/null || echo "")
  if [ -n "$owner" ] && kill -0 "$owner" 2>/dev/null; then
    # Log the contention rather than exiting silently: a skip that leaves no
    # trace is indistinguishable from a cycle that found nothing to do, and
    # THAT ambiguity is what made this bug take three occurrences to spot.
    echo "== $(date '+%F %T') SKIP $head — build already running (pid $owner)" >> "$LOG"
    exit 0
  fi
  echo "== $(date '+%F %T') breaking stale lock (pid ${owner:-unknown} is gone)" >> "$LOG"
  rm -rf "$LOCK"
  mkdir "$LOCK" 2>/dev/null || { echo "== $(date '+%F %T') SKIP $head — lost the lock race" >> "$LOG"; exit 0; }
fi
LOCK_HELD=1
echo $$ > "$LOCK/pid"

# RE-READ THE STAMP NOW THAT WE HOLD THE LOCK. The check above ran before the
# wait, so a build we queued behind may have just installed this very sha —
# rebuilding it is the wasted-cycle half of the reported bug ("each next SOLO
# cycle built the identical sha fine").
last=$(cat "$STAMP" 2>/dev/null || echo "")
[ "$head" = "$last" ] && exit 0

{
  echo "== $(date '+%F %T') building $head (was: ${last:-none})"
  # Build from a clean, committed snapshot: a worktree of HEAD, so nobody's
  # uncommitted edits (or a mid-edit broken tree) can poison the deploy.
  WORK=$(mktemp -d /tmp/amux-rs-build.XXXXXX)
  git -C "$REPO" worktree add --detach "$WORK" "$(git -C "$REPO" rev-parse HEAD)" >/dev/null
  # DISK GUARD (AMUX-2754). The shared target dir has no GC — cargo never
  # reclaims — so it grows without bound, and on 2026-08-10 the volume hit
  # 741MB free with a 50-session fleet and writes failing with ENOSPC.
  #
  # The trigger is FREE DISK, deliberately not target-dir size. Disk-full is
  # the thing that actually broke the fleet; dir size is a proxy for it whose
  # threshold would be a guess, and the same 28GB is fine on this volume and
  # fatal on a smaller one. Free space is the condition that is absent in the
  # healthy state, which is the signal worth tripping on.
  #
  # Clearing the cache costs one cold build (~3min, once). That is the cheap
  # side of this trade by a wide margin: the expensive side is every lane
  # failing to write.
  FREE_GB=$(df -g "$HOME" | awk 'NR==2{print $4}')
  if [ "${FREE_GB:-999}" -lt "${AMUX_BUILD_MIN_FREE_GB:-25}" ]; then
    TGT_GB=$(du -sg "$HOME/.amux/rust-build-target" 2>/dev/null | awk '{print $1}')
    echo "== DISK LOW: ${FREE_GB}GB free (< ${AMUX_BUILD_MIN_FREE_GB:-25}GB). Clearing the ${TGT_GB:-?}GB shared target dir; next build is cold."
    rm -rf "$HOME/.amux/rust-build-target"
  fi
  # Shared target dir: incremental rebuilds (~15s) instead of cold ones
  # (~3min) — the worktree isolates SOURCE, the cache is content-keyed.
  # CAPTURE THE WHOLE BUILD, THEN DECIDE WHAT TO KEEP (AMUX-2927).
  #
  # This was `cargo build ... 2>&1 | tail -3`, and on a FAILURE cargo's last
  # three lines are the summary — "error: could not compile ... due to N
  # previous errors" and a warning — while the line that names the actual
  # problem (`error[E0432]: unresolved import ...`, with its file and line) is
  # thousands of lines earlier and was thrown away. Every build failure was
  # therefore undiagnosable from the log it wrote, which is ethos rule 4: the
  # instrument could not express the discriminator.
  #
  # Success still logs three lines — terseness there is the point, and this
  # runs every 60s. Only the failing path pays for detail, which is the path
  # that needs it.
  BUILD_OUT=$(mktemp /tmp/amux-rs-buildout.XXXXXX)
  if (cd "$WORK" && CARGO_TARGET_DIR="$HOME/.amux/rust-build-target" cargo build --release -p amux-server) > "$BUILD_OUT" 2>&1; then
    tail -3 "$BUILD_OUT"
    install -m 0755 "$HOME/.amux/rust-build-target/release/amux-server" "$INSTALL"
    echo "$head" > "$STAMP"
    echo "== installed; running server will self-adopt within 5s"
  else
    echo "== BUILD FAILED for $head — running server keeps the last good build"
    echo "-- diagnostics (every error, with context) ---------------------------"
    grep -nE '^error(\[E[0-9]+\])?:|^error: ' -A 8 "$BUILD_OUT" | head -200 || true
    echo "-- last 20 lines of cargo output -------------------------------------"
    tail -20 "$BUILD_OUT"
    echo "-- end diagnostics ($(wc -l < "$BUILD_OUT" | tr -d ' ') lines total) ---"
    # Stamp is NOT updated: the next cycle retries. A failed build never
    # takes the fleet down (the AC-309 class: a bad save must not crash-loop
    # the server).
  fi
} >> "$LOG" 2>&1
