"""Deterministic tests for the yolo-guardrail delta (AMUX perms Phase A.2).

Loads the REAL pure helper `_yolo_is_guarded_prompt`, the REAL
`_YOLO_GUARD_DESTRUCTIVE` matcher, and the REAL `_YOLO_PROMPTS` table out of
amux-server.py via AST — no server needed, mirroring tests/test_chat_core.py.

The guardrail teaches `_yolo_auto_respond` to STAND DOWN on an explicit
permission-rule confirmation (an `ask` rule fired) or a destructive command in a
proceed prompt, so the session stays `waiting` for a human decision instead of
being auto-approved with "1". Covers:
  - fixture-1 ask-rule prompt (live-captured under --dangerously-skip-permissions)
    -> guarded (skip), even though _YOLO_PROMPTS would otherwise match it
  - the $() command-substitution proceed prompt (no "Permission rule" line,
    benign command) -> NOT guarded AND still matched by _YOLO_PROMPTS (no
    regression of the yolo convenience auto-answer)
  - the AskUserQuestion dialog -> matches nothing (unchanged) and is not guarded
  - the secondary destructive-command belt: a proceed prompt WITHOUT the
    permission-rule line but WITH a destructive command -> guarded; a benign
    command in the same shape -> not guarded
"""
import ast, os, re

SERVER = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      "amux-server.py")

# ── AST-load the real guard helper + matcher + the _YOLO_PROMPTS table ─────────
ns: dict = {"re": re}
tree = ast.parse(open(SERVER, encoding="utf-8").read())
_want_func = {"_yolo_is_guarded_prompt"}
_want_assign = {"_YOLO_GUARD_DESTRUCTIVE", "_YOLO_PROMPTS"}
for node in tree.body:
    if isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name) \
            and node.targets[0].id in _want_assign:
        exec(compile(ast.Module(body=[node], type_ignores=[]), SERVER, "exec"), ns)
    if isinstance(node, ast.FunctionDef) and node.name in _want_func:
        exec(compile(ast.Module(body=[node], type_ignores=[]), SERVER, "exec"), ns)

guarded = ns["_yolo_is_guarded_prompt"]
DESTRUCTIVE = ns["_YOLO_GUARD_DESTRUCTIVE"]
YOLO_PROMPTS = ns["_YOLO_PROMPTS"]
assert callable(guarded), "_yolo_is_guarded_prompt not extracted from amux-server.py"
assert hasattr(DESTRUCTIVE, "search"), "_YOLO_GUARD_DESTRUCTIVE not extracted"
assert len(YOLO_PROMPTS) >= 1, "_YOLO_PROMPTS not extracted"


def yolo_matches(clean):
    """Mirror the loop in _yolo_auto_respond: does any _YOLO_PROMPTS pattern hit?"""
    return any(pat.search(clean) for pat, _resp in YOLO_PROMPTS)


# ── fixtures (fixture 1 & 2 are the A.0 live captures, v2.1.220) ───────────────
FIX_ASK_RULE = """ Bash command
   sudo -n true
   Check if sudo can run without password prompt
 Permission rule Bash(sudo *) requires confirmation for this command.
 /permissions to update rules
 Do you want to proceed?
 ❯ 1. Yes
   2. No
 Esc to cancel · Tab to amend · ctrl+e to explain
"""

FIX_ASKUSERQUESTION = """ ☐ Color preference
Which color do you prefer?
❯ 1. Red
     A warm, bold color
  2. Blue
     A cool, calm color
  3. Green
     A natural, refreshing color
  4. Type something.
────
  5. Chat about this
Enter to select · ↑/↓ to navigate · Esc to cancel
"""

# A benign internal safety prompt WITHOUT any permission-rule line — the yolo
# convenience case that must keep being auto-answered.
FIX_CMD_SUBST = """ Bash command
   echo $(date)
 Command contains $() command substitution which could execute arbitrary code
 Do you want to proceed?
 ❯ 1. Yes
   2. No
 Esc to cancel · Tab to amend · ctrl+e to explain
"""


# ── (1) ask-rule prompt -> guarded, even though _YOLO_PROMPTS would match ──────
assert yolo_matches(FIX_ASK_RULE), \
    "fixture-1 must match a _YOLO_PROMPTS pattern (else the guard would be moot)"
assert guarded(FIX_ASK_RULE) is True, \
    "fixture-1 ask-rule prompt must be guarded (stand down, leave waiting)"
print("case-1 ok — live ask-rule prompt is guarded despite matching a yolo pattern")

# ── (2) command-substitution proceed prompt -> NOT guarded, still auto-answered ─
assert guarded(FIX_CMD_SUBST) is False, \
    "benign $() proceed prompt (no permission-rule line) must NOT be guarded"
assert yolo_matches(FIX_CMD_SUBST), \
    "benign $() proceed prompt must still be auto-answered (no yolo regression)"
print("case-2 ok — benign proceed prompt stays auto-answered, not guarded")

# ── (3) AskUserQuestion -> matches nothing, not guarded (unchanged) ────────────
assert yolo_matches(FIX_ASKUSERQUESTION) is False, \
    "AskUserQuestion must not match any _YOLO_PROMPTS pattern (open-ended question)"
assert guarded(FIX_ASKUSERQUESTION) is False, \
    "AskUserQuestion is an open-ended question, not a permission prompt — not guarded"
print("case-3 ok — AskUserQuestion matches nothing and is not guarded (unchanged)")

# ── (4) secondary destructive-command belt ────────────────────────────────────
def proceed(cmd):
    """A proceed prompt with no permission-rule line, so only the belt can fire."""
    return (f" Bash command\n   {cmd}\n Do you want to proceed?\n"
            f" ❯ 1. Yes\n   2. No\n Esc to cancel\n")

# destructive commands in a bare proceed prompt -> guarded via the belt
for cmd in ["rm -rf ~/project", "rm -fr /tmp/x", "sudo rm foo", "git push --force",
            "git push -f origin main", "dd if=/dev/zero of=/dev/sda",
            "mkfs.ext4 /dev/sdb1", "git clean -fdx", "git clean -xfd"]:
    assert guarded(proceed(cmd)) is True, f"destructive belt must guard: {cmd!r}"

# benign commands in the SAME shape must NOT be guarded (no false positives)
for cmd in ["rm notes.txt", "rm -f build.log", "git status", "ls -la",
            "git commit -m done", "echo hello"]:
    assert guarded(proceed(cmd)) is False, f"benign command must NOT be guarded: {cmd!r}"

# the belt only fires inside a proceed prompt — a destructive token loose in
# scrollback (no "do you want to proceed") must NOT be guarded
assert guarded(" running: sudo apt update\n done, exit 0\n") is False, \
    "a destructive token in plain scrollback (no proceed prompt) must not be guarded"
print("case-4 ok — destructive belt guards the destructive set, benign commands pass, "
      "scrollback tokens ignored")

print("\nALL YOLO-GUARDRAIL CHECKS PASSED")


def test_yolo_guardrail():
    """The scenarios above execute at import time and raise on any regression;
    reaching here means the guard stands down on ask-rule + destructive prompts
    while leaving the benign yolo auto-answer path (and open-ended questions)
    untouched."""
    assert True
