#!/bin/bash
# PostToolUse hook: validate dashboard client JS after every edit.
#
# History: this hook used to ast.parse amux-server.py and node --check its inline
# <script> blocks. The Python server was removed 2026-08-09; the client now lives
# as real static files under crates/amux-dashboard/static/. Rust edits are NOT
# checked here — `cargo check` on every single Edit would outlast the hook timeout
# and stack up on the shared target dir; the builder + CI (rust.yml) own that gate,
# and .claude/rules/single-file.md tells sessions to run `cargo check` themselves.
set -euo pipefail

# The repo root is wherever THIS script lives (<repo>/.claude/check-and-commit.sh),
# never a hardcoded path. A hardcoded checkout path matches on exactly one machine;
# everywhere else the path comparison below fails and the script exits 0 — reporting
# success while checking nothing. That is ethos rule 7 ("can your check actually
# fail?") in its worst form, because the silence looks like a pass.
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
STATIC_REAL=$(python3 -c "import os, sys; print(os.path.realpath(sys.argv[1]))" "$REPO/crates/amux-dashboard/static")

# Read the edited path from hook input, resolved, so a symlinked checkout or a
# relative path still matches the directory we gate on.
FILE_PATH=$(cat | python3 -c "
import sys, json, os
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
p = d.get('tool_input', {}).get('file_path', '')
print(os.path.realpath(p) if p else '')
" 2>/dev/null || echo "")

# Only gate client JS under the dashboard's static dir.
case "$FILE_PATH" in
  "$STATIC_REAL"/*.js|"$STATIC_REAL"/*.mjs) ;;
  *) exit 0 ;;
esac

# node --check proves the script PARSES, not that every name it calls exists
# (the closePeek() lesson, ethos rule 7) — but a parse error is the failure mode
# that bricks the whole SPA for every client at once, so it blocks immediately.
if ! node --check "$FILE_PATH"; then
  echo "JS syntax error in $FILE_PATH (see above)" >&2
  exit 2  # blocks the action
fi

exit 0
