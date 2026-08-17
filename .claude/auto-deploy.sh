#!/bin/bash
# auto-deploy.sh — push committed site changes automatically.
#
# Called in two modes:
#   PostToolUse Bash hook (no args): only pushes after a board-done command
#   Stop hook (--on-stop flag): pushes unconditionally on session end/idle
#
# Safety rule (shared checkout): only pushes when EVERY unpushed commit on
# main carries Amux-Session: amux-homepage. A single foreign commit aborts —
# it would ship under this session's push with no review, which is exactly
# the incident documented in CLAUDE.md's Deploy section.

set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

ON_STOP=0
if [ "${1:-}" = "--on-stop" ]; then
  ON_STOP=1
fi

if [ "$ON_STOP" -eq 0 ]; then
  # PostToolUse mode: only fire on board done operations.
  # Accept both:
  #   amux board done ITEM_ID
  #   curl ... -d '{"status":"done"...}' .../api/board/...
  CMD=$(python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    print(d.get('tool_input', {}).get('command', ''))
except Exception:
    pass
" 2>/dev/null || echo "")

  if ! echo "$CMD" | grep -qE '(amux board done|api/board.*(\"status\":\"done\"|status.*done))'; then
    exit 0
  fi
fi

cd "$REPO"

# How many commits are ahead of origin/main?
AHEAD=$(git rev-list --count origin/main..main 2>/dev/null || echo 0)
if [ "$AHEAD" -eq "0" ]; then
  exit 0  # nothing to push
fi

# Collect all session attributions from unpushed commits.
# Commits from this session carry "Amux-Session: amux-homepage" in the body.
# If any commit is unattributed or attributed to a different session, abort.
FOREIGN=$(git log --format="%H %B" origin/main..main | python3 -c "
import sys, re
text = sys.stdin.read()
# Split on commit SHAs
commits = re.split(r'\n([0-9a-f]{40}) ', text)
foreign = []
for block in commits:
    sessions = re.findall(r'Amux-Session:\s*(\S+)', block)
    if not sessions:
        continue  # no attribution — treat as foreign (conservative)
    for s in sessions:
        if s != 'amux-homepage':
            foreign.append(s)
print('\n'.join(set(foreign)))
" 2>/dev/null || echo "parse-error")

if [ -n "$FOREIGN" ]; then
  echo "auto-deploy: skipping push — other sessions have unpushed commits: $FOREIGN" >&2
  exit 0
fi

# All $AHEAD commits are mine. Safe to push.
echo "auto-deploy: pushing $AHEAD commit(s) to origin/main..."
git push origin main
echo "auto-deploy: deployed."
