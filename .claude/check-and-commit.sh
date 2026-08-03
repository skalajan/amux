#!/bin/bash
# PostToolUse hook: validate amux-server.py syntax after every edit
# Receives JSON on stdin with tool_input.file_path
set -euo pipefail

# The repo root is wherever THIS script lives (<repo>/.claude/check-and-commit.sh),
# never a hardcoded path. A hardcoded checkout path matches on exactly one machine;
# everywhere else the path comparison below fails and the script exits 0 — reporting
# success while checking nothing. That is ethos rule 7 ("can your check actually
# fail?") in its worst form, because the silence looks like a pass.
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
SERVER="$REPO/amux-server.py"

# Read the edited path from hook input, resolved, so a symlinked checkout or a
# relative path still matches the file we are about to check.
FILE_PATH=$(cat | python3 -c "
import sys, json, os
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
p = d.get('tool_input', {}).get('file_path', '')
print(os.path.realpath(p) if p else '')
" 2>/dev/null || echo "")

SERVER_REAL=$(python3 -c "import os, sys; print(os.path.realpath(sys.argv[1]))" "$SERVER")

# Only check if amux-server.py was edited
if [ "$FILE_PATH" != "$SERVER_REAL" ]; then
  exit 0
fi

# Check Python syntax. The path goes in as argv, not interpolated into the source,
# so a checkout path containing a quote can't break (or rewrite) the check.
if ! python3 -c "
import ast, sys
ast.parse(open(sys.argv[1]).read())
" "$SERVER"; then
  echo "Python syntax error in amux-server.py (see traceback above)" >&2
  exit 2  # blocks the action
fi

# Extract JS and check with node.
# This runs under `if !` rather than a post-hoc `$?` test: with `set -e` the script
# aborts the moment the checker returns non-zero, so a trailing `[ $? -ne 0 ]` is
# unreachable — and the abort carries the checker's exit code (1), which Claude Code
# treats as a non-blocking error. Only exit 2 blocks, so a JS syntax error used to
# sail straight through the one gate meant to stop it.
if ! python3 -c "
import re, subprocess, sys, tempfile, os
content = open(sys.argv[1]).read()
m = re.search(r'<script>\s*\n(.*?)</script>', content, re.DOTALL)
if not m:
    print('WARNING: no <script> block found in amux-server.py', file=sys.stderr)
    sys.exit(0)
fd, path = tempfile.mkstemp(suffix='.js')
os.write(fd, m.group(1).encode())
os.close(fd)
r = subprocess.run(['node', '--check', path], capture_output=True, text=True)
os.unlink(path)
if r.returncode != 0:
    print('JS syntax error in amux-server.py:', file=sys.stderr)
    print(r.stderr, file=sys.stderr)
    sys.exit(1)
" "$SERVER"; then
  exit 2  # blocks the action
fi

exit 0
