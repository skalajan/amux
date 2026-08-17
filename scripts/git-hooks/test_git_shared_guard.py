#!/usr/bin/env python3
"""Test matrix for git-shared-guard.py (MI-4083 extension of the 14-case set).

Axis under test: MENTION vs INVOCATION — a guarded git command merely mentioned
in documentation text (heredoc bodies, quoted strings) must PASS; the same
command actually invoked must BLOCK. Plus the AMUX_AMEND_EXPECT pin protocol
against a real git fixture (bare origin + clone).

Run: python3 ~/.amux/hooks/test_git_shared_guard.py   (exit 0 = all pass)
"""
import json
import os
import subprocess
import sys
import tempfile

HOOK = os.path.join(os.path.dirname(os.path.abspath(__file__)), "git-shared-guard.py")


def run_hook(command, cwd, shared_root):
    env = dict(os.environ, AMUX_SHARED_CHECKOUTS=shared_root)
    p = subprocess.run(
        [sys.executable, HOOK],
        input=json.dumps({"tool_name": "Bash", "tool_input": {"command": command}, "cwd": cwd}),
        capture_output=True, text=True, env=env, timeout=30,
    )
    return p.returncode, p.stderr


def git(cwd, *args):
    return subprocess.run(("git", "-C", cwd) + args, capture_output=True, text=True).stdout.strip()


def main():
    tmp = tempfile.mkdtemp(prefix="guardtest-")
    origin = os.path.join(tmp, "origin.git")
    work = os.path.join(tmp, "work")
    subprocess.run(["git", "init", "--bare", "-q", "-b", "main", origin])
    subprocess.run(["git", "clone", "-q", origin, work], capture_output=True)
    git(work, "config", "user.email", "t@t")
    git(work, "config", "user.name", "t")
    open(os.path.join(work, "f.txt"), "w").write("1\n")
    git(work, "add", "f.txt")
    git(work, "commit", "-q", "-m", "c1")
    git(work, "push", "-q", "origin", "main")  # HEAD c1 = pushed

    cases = []  # (name, command, cwd, expect_block)
    A = lambda n, c, b: cases.append((n, c, work, b))

    # mention-vs-invocation: heredoc bodies (the MI-4083 false-positive class)
    A("heredoc-doc amend mention", "cat >> notes.md <<'EOF'\nthe rule: git commit --amend on a PUSHED commit is never ok\nEOF", False)
    A("heredoc-doc reset mention", "cat >> notes.md <<'EOF'\nnever run git reset --hard on shared trees\nEOF", False)
    A("second-heredoc mention", "cat > a <<'EOF'\nhi\nEOF\ncat > b <<'EOF'\ngit reset --hard is bad\nEOF", False)
    A("unquoted-tag heredoc mention", "tee -a x.md <<DOC\ngit clean -fd wipes everything\nDOC", False)
    # executable heredoc bodies must STILL be scanned
    A("bash heredoc real reset", "bash <<'EOF'\ngit reset --hard HEAD~1\nEOF", True),
    A("sh heredoc real clean", "sh <<EOF\ngit clean -fd\nEOF", True),
    # quoted mentions (existing behavior, regression pins)
    A("quoted commit-msg mention", 'git commit -m "never git reset --hard again" -- f.txt', False)
    A("echo quoted amend mention", 'echo "recipe: git commit --amend needs a pin"', False)
    # real invocations still block (regression pins)
    A("real reset --hard", "git reset --hard HEAD~1", True)
    A("real mixed reset", "git reset HEAD~2", True)
    A("path-scoped reset ok", "git reset -- f.txt", False)
    A("real clean -fd", "git clean -fd", True)
    A("stash drop", "git stash drop", True)
    A("commit -a", 'git commit -a -m "x"', True)
    A("path-scoped commit ok", 'git commit -m "x" -- f.txt', False)
    # amend pin protocol (real fixture): HEAD c1 is PUSHED
    A("amend on pushed HEAD", 'git commit --amend --no-edit', True)

    # CASE 21 family (2026-07-07 fleet reset incident — mvs-infra): bare/
    # un-scoped stash internally reset --hards the WHOLE shared tree. Only
    # pathspec-scoped pushes + non-destructive subcommands pass.
    A("bare stash", "git stash", True)
    A("stash -q", "git stash -q", True)
    A("stash push unscoped", "git stash push", True)
    A("stash push -m unscoped", 'git stash push -m wip', True)
    A("stash save", "git stash save wip", True)
    # Target the test's OWN isolated tmp, not the literal /tmp: on macOS /tmp is
    # /private/tmp, which the fleet fills with per-session scratchpads, so it has
    # live cotenants and the guard (correctly) blocks a tree-wide stash there.
    # `tmp` is a fresh mkdtemp with no cotenants — line 90 already relies on that —
    # so it exercises the real property (target-scoping exempts an outside dir)
    # without a machine-dependent /tmp.
    A("stash -C outside-checkout ok (guard is target-scoped)", f"git -C {tmp} stash", False)
    A("stash -C shared-checkout", f"git -C {work} stash", True)
    A("stash push pathspec ok", "git stash push -- notes/", False)
    A("stash push untracked pathspec ok", "git stash push --include-untracked -- notes/", False)
    A("stash pop ok", "git stash pop", False)
    A("stash apply ok", "git stash apply", False)
    A("stash list ok", "git stash list", False)
    A("stash show ok", "git stash show -p", False)
    A("reset --hard no-target", "git reset --hard", True)
    # scoping: outside the shared root nothing blocks
    cases.append(("non-shared cwd reset", "git reset --hard", tmp, False))

    failures = []
    for name, cmd, cwd, expect_block in cases:
        code, err = run_hook(cmd, cwd, work)
        blocked = code == 2
        if blocked != expect_block:
            failures.append(f"{name}: expected {'BLOCK' if expect_block else 'PASS'}, got {'BLOCK' if blocked else 'PASS'}\n  {err.strip()[:200]}")

    # unpushed-HEAD amend trio (needs a fresh unpushed commit)
    open(os.path.join(work, "f.txt"), "a").write("2\n")
    git(work, "add", "f.txt")
    git(work, "commit", "-q", "-m", "c2")  # unpushed
    head = git(work, "rev-parse", "HEAD")
    trio = [
        ("amend unpushed unpinned", "git commit --amend --no-edit", True),
        ("amend unpushed pinned-match", f"AMUX_AMEND_EXPECT={head} git commit --amend --no-edit", False),
        ("amend unpushed stale pin", "AMUX_AMEND_EXPECT=deadbeefdead git commit --amend --no-edit", True),
    ]
    for name, cmd, expect_block in trio:
        code, err = run_hook(cmd, work, work)
        blocked = code == 2
        if blocked != expect_block:
            failures.append(f"{name}: expected {'BLOCK' if expect_block else 'PASS'}, got {'BLOCK' if blocked else 'PASS'}\n  {err.strip()[:200]}")

    total = len(cases) + len(trio)
    if failures:
        print(f"FAIL {len(failures)}/{total}:")
        for f in failures:
            print(" -", f)
        return 1
    print(f"ALL {total} PASS")
    return 0




if __name__ == "__main__":
    sys.exit(main())
