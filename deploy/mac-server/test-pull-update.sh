#!/usr/bin/env bash
# test-pull-update.sh — sandbox test harness for amux-pull-update.sh (Decision
# D1-A in .omc/plans/mac-server-deploy.md). Self-contained: builds a throwaway
# bare "origin" + a "mac-brain" clone (pushes) + a "mac-server" clone (runs the
# real, current deploy/mac-server/amux-pull-update.sh from this checkout) under
# a temp dir, exercises the required scenarios, and cleans up on exit. Installs
# nothing on this machine and never touches the real ~/.amux or the real repo's
# git state.
#
# Run: deploy/mac-server/test-pull-update.sh
#
# Scenarios (per docs/mac-server-deploy.md "Auto-update" verification / plan P6
# AC-2/AC-3):
#   1. Both dirt classes (unstaged edit + staged deletion) on ALLOWLISTED paths,
#      plus an incoming commit touching those same paths -> pull lands clean,
#      tripwire empty, no alert.
#   2. A dirty tracked path OUTSIDE the allowlist -> abort + alert, tree left
#      untouched; cleaning it lets the next cadence recover.
#   3. Untracked ("??") noise -> never aborts.
#   4. The script never pushes (both an indirect check — origin's ref only ever
#      moves via this test's own explicit pushes — and a direct unit check of
#      the git() wrapper's push-refusal).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REAL_SCRIPT="$HERE/amux-pull-update.sh"
[[ -f "$REAL_SCRIPT" ]] || { echo "FATAL: $REAL_SCRIPT not found"; exit 1; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/amux-pull-update-test.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

ORIGIN="$WORK/origin.git"
BRAIN="$WORK/mac-brain"
SERVER="$WORK/mac-server"
FAKE_HOME="$WORK/fake-home"
mkdir -p "$FAKE_HOME"

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS: $1"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL: $1"; }

# ── scaffold ─────────────────────────────────────────────────────────────────
git init -q --bare --initial-branch=main "$ORIGIN"
git clone -q "$ORIGIN" "$BRAIN"
(
  cd "$BRAIN"
  git config user.email test@test.local
  git config user.name  "test"
  mkdir -p .claude/commands
  echo "amux v1"    > .claude/commands/amux.md
  echo '{"v":1}'    > .claude/settings.json
  echo "server v1"  > amux-server.py
  echo "telegram v1" > amux-telegram.py
  echo "doc v1"     > README.md
  git add -A
  git commit -q -m init
  git push -q origin main
)

git clone -q "$ORIGIN" "$SERVER"
mkdir -p "$SERVER/deploy/mac-server"
cp "$REAL_SCRIPT" "$SERVER/deploy/mac-server/amux-pull-update.sh"
chmod +x "$SERVER/deploy/mac-server/amux-pull-update.sh"
cat > "$SERVER/deploy/mac-server/pull-reconcile-allowlist.txt" <<'EOF'
# test allowlist — mirrors the real seed candidates' shape
.claude/commands/*.md
.claude/settings.json
EOF
(cd "$SERVER" && git config user.email test@test.local && git config user.name test)

run_pull_update() {
  # Runs the real script against the SERVER clone with an isolated fake HOME
  # (so ~/.amux write_token / telegram.env on THIS machine are never touched —
  # they simply won't exist under $FAKE_HOME, so alerting no-ops as designed).
  set +e
  OUT="$(cd "$SERVER/deploy/mac-server" && HOME="$FAKE_HOME" CC_HOME="$FAKE_HOME/.amux" ./amux-pull-update.sh 2>&1)"
  RC=$?
  set -e
  LAST_OUT="$OUT"
  LAST_RC=$RC
}

echo "=== Scenario 1: both dirt classes on allowlisted paths + incoming commit on same paths ==="
(
  cd "$SERVER"
  echo "local edit" >> .claude/commands/amux.md   # unstaged edit
  git rm --cached -q .claude/settings.json         # staged deletion
)
(
  cd "$BRAIN"
  echo "amux v2 from brain" > .claude/commands/amux.md
  echo '{"v":2}'           > .claude/settings.json
  git commit -qam "brain: update commands + settings"
  git push -q origin main
)
ORIGIN_BEFORE_S1="$(git -C "$ORIGIN" rev-parse main)"
run_pull_update
echo "$LAST_OUT" | sed 's/^/    /'
if [[ "$LAST_RC" -eq 0 ]]; then ok "scenario 1: exit 0"; else bad "scenario 1: expected exit 0, got $LAST_RC"; fi
if grep -q "tripwire OK" <<<"$LAST_OUT"; then ok "scenario 1: tripwire asserted clean"; else bad "scenario 1: tripwire OK line missing"; fi
if grep -qi "ALERT" <<<"$LAST_OUT"; then bad "scenario 1: unexpected ALERT in output"; else ok "scenario 1: no alert fired"; fi
if [[ "$(cat "$SERVER/.claude/commands/amux.md")" == "amux v2 from brain" ]]; then ok "scenario 1: brain's commands/amux.md landed"; else bad "scenario 1: commands/amux.md not reconciled to brain's version"; fi
if [[ "$(cat "$SERVER/.claude/settings.json")" == '{"v":2}' ]]; then ok "scenario 1: brain's settings.json landed"; else bad "scenario 1: settings.json not reconciled to brain's version"; fi
if [[ -z "$(git -C "$SERVER" status --porcelain --untracked-files=no)" ]]; then ok "scenario 1: server tree fully clean post-pull"; else bad "scenario 1: server tree still dirty: $(git -C "$SERVER" status --porcelain --untracked-files=no)"; fi
if grep -q "amux-telegram.py changed" <<<"$LAST_OUT"; then bad "scenario 1: unexpected telegram bounce (not changed this round)"; else ok "scenario 1: no spurious telegram bounce"; fi

echo "=== Scenario 2: dirty tracked path OUTSIDE the allowlist -> abort + alert ==="
(cd "$SERVER" && echo "unrelated local edit" >> README.md)
(
  cd "$BRAIN"
  echo "doc v2" >> README.md
  git commit -qam "brain: doc update"
  git push -q origin main
)
HEAD_BEFORE_S2="$(git -C "$SERVER" rev-parse HEAD)"
ORIGIN_BEFORE_S2="$(git -C "$ORIGIN" rev-parse main)"
run_pull_update
echo "$LAST_OUT" | sed 's/^/    /'
if [[ "$LAST_RC" -ne 0 ]]; then ok "scenario 2: exit non-zero (aborted)"; else bad "scenario 2: expected non-zero exit, got 0"; fi
if grep -q "outside-allowlist divergence" <<<"$LAST_OUT"; then ok "scenario 2: alert path taken"; else bad "scenario 2: expected outside-allowlist alert message"; fi
if [[ "$(git -C "$SERVER" rev-parse HEAD)" == "$HEAD_BEFORE_S2" ]]; then ok "scenario 2: server HEAD untouched (no partial pull)"; else bad "scenario 2: server HEAD moved despite abort"; fi
if grep -q "unrelated local edit" "$SERVER/README.md"; then ok "scenario 2: dirty README.md left untouched (never force-resolved)"; else bad "scenario 2: local edit to README.md was clobbered"; fi

echo "--- scenario 2 recovery: clean the offending path, expect next cadence to land the pending commit ---"
(cd "$SERVER" && git checkout -q -- README.md)
run_pull_update
echo "$LAST_OUT" | sed 's/^/    /'
if [[ "$LAST_RC" -eq 0 ]]; then ok "scenario 2 recovery: exit 0 after cleaning"; else bad "scenario 2 recovery: expected exit 0, got $LAST_RC"; fi
if grep -q "doc v2" "$SERVER/README.md"; then ok "scenario 2 recovery: pending brain commit landed"; else bad "scenario 2 recovery: brain's doc v2 commit did not land"; fi

echo "=== Scenario 3: untracked noise never aborts ==="
echo "scratch/noise, not tracked, not in allowlist" > "$SERVER/.DS_Store_test_noise"
(
  cd "$BRAIN"
  echo "doc v3" >> README.md
  git commit -qam "brain: doc update 2"
  git push -q origin main
)
run_pull_update
echo "$LAST_OUT" | sed 's/^/    /'
if [[ "$LAST_RC" -eq 0 ]]; then ok "scenario 3: exit 0 despite untracked noise"; else bad "scenario 3: expected exit 0, got $LAST_RC"; fi
if grep -q "doc v3" "$SERVER/README.md"; then ok "scenario 3: pending commit landed despite untracked noise"; else bad "scenario 3: commit did not land"; fi
if [[ -f "$SERVER/.DS_Store_test_noise" ]]; then ok "scenario 3: untracked noise file left alone"; else bad "scenario 3: untracked noise file was removed"; fi

echo "=== Scenario 4a: script never pushes (indirect — origin ref only moves via this test's explicit pushes) ==="
ORIGIN_HEAD_NOW="$(git -C "$ORIGIN" rev-parse main)"
BRAIN_HEAD_NOW="$(git -C "$BRAIN" rev-parse main)"
if [[ "$ORIGIN_HEAD_NOW" == "$BRAIN_HEAD_NOW" ]]; then
  ok "scenario 4a: origin/main matches brain's last explicit push (nothing pushed the pull-update side)"
else
  bad "scenario 4a: origin/main ($ORIGIN_HEAD_NOW) diverged from brain's last push ($BRAIN_HEAD_NOW)"
fi

echo "=== Scenario 4b: script never pushes (direct — unit-test the git() wrapper) ==="
TESTREPO="$WORK/push-guard-repo"
git init -q --initial-branch=main "$TESTREPO"
(
  cd "$TESTREPO"
  git config user.email test@test.local; git config user.name test
  echo hi > f.txt; git add f.txt; git commit -qm init
)
GIT_FN_FILE="$WORK/git-fn-extract.sh"
sed -n '/^git() {/,/^}/p' "$REAL_SCRIPT" > "$GIT_FN_FILE"
set +e
GUARD_OUT="$(
  cd "$TESTREPO"
  log() { printf '[%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"; }
  # shellcheck disable=SC1091
  source "$GIT_FN_FILE"
  git push 2>&1
)"
GUARD_RC=$?
set -e
if [[ "$GUARD_RC" -ne 0 ]]; then ok "scenario 4b: git() wrapper refuses push (exit $GUARD_RC)"; else bad "scenario 4b: git() wrapper did NOT refuse push"; fi
if grep -qi "REFUSING" <<<"$GUARD_OUT"; then ok "scenario 4b: refusal is logged"; else bad "scenario 4b: no refusal message"; fi

echo ""
echo "=== Summary: $PASS passed, $FAIL failed ==="
[[ "$FAIL" -eq 0 ]]
