# Upstream sync procedure (scheduled)

SOP for the recurring scheduler entry **"Sync fork with upstream"** (fires into the
`amux-helper` session weekly). Keeps `skalajan/amux` (origin) up to date with
`mixpeek/amux` (upstream) while preserving the local modifications documented in
[`../MODIFICATIONS.md`](../MODIFICATIONS.md).

## Procedure

1. **Preflight.** `git -C ~/Desktop/Projects/amux fetch upstream origin` and check
   `git rev-list --left-right --count main...upstream/main`. If not behind upstream,
   push any unpushed local commits to origin and stop — report "already in sync".
   If the working tree has uncommitted changes to files upstream touches, stop and report.
2. **Record the rollback point.** `PREV=$(git rev-parse main)`.
3. **Merge in a temporary worktree — NEVER in place.** The live server watches
   `amux-server.py`'s mtime and re-execs on change; it must never see conflict
   markers. `git worktree add <scratch>/merge-wt -b tmp-upstream-merge main`,
   then `git merge upstream/main` inside it.
4. **Pre-merge freshness gate (blocking).** Before resolving anything, check
   every row of the **[Local Delta Registry](../MODIFICATIONS.md#local-delta-registry)**
   against the tracked files (`amux-server.py`, `amux`) in the merge worktree —
   this is the authoritative, single list of areas; do not consult any other
   enumeration. For each row:
   - **Tier 1 — BLOCKING.** Grep the row's unique-to-local landmarks (and, where
     retrofitted, its `AMUX-LOCAL:<feature>` sentinel). If **any** no longer
     matches anywhere in the file, the local delta's position was disturbed by
     an upstream refactor in a way the SOP doesn't have a playbook for. Treat it
     as an **unknown conflict**: abort, remove the worktree, report what
     vanished, and file a board task. Do not guess at a resolution.
   - **Tier 2 — WARN.** Grep the row's reapply-hunk anchor (the upstream-owned
     symbol/line the delta is grafted around). If the anchor no longer matches
     but all of that row's Tier-1 landmarks still do, the local delta itself is
     intact — only the upstream context it's grafted onto moved. Log it in the
     final report (step 10) so the resolution note can be re-checked by hand,
     but **do not abort** — proceed to step 5.
   - This is the real enforcement backstop for the unattended weekly run: it
     catches a registry that has silently gone stale against an upstream
     refactor, before any manual resolution masks the drift.
5. **Resolve conflicts.** Consult the **Local Delta Registry** in
   [`../MODIFICATIONS.md`](../MODIFICATIONS.md#local-delta-registry) for the
   full area list and each area's resolution note (keep local behavior, graft
   upstream refactors around it) — the registry is the only place these notes
   live. Two representative examples:
   - Account routing / multi-home: keep local one-liners like
     `project_dir = _claude_project_dir(...)` over upstream's
     `CLAUDE_HOME / "projects" / ...`.
   - `amux` CLI: keep the MODIFICATIONS.md hunks (config-dir flags, yolo default,
     default model, remote-control) AND upstream's new commands — both sides.

   A conflict **outside** the registry's areas that isn't trivially mechanical →
   abort: remove the worktree, report what conflicted, and file a board task.
   Do not guess.
6. **Verify in the worktree.** No `<<<<<<<`/`>>>>>>>` markers anywhere;
   `python3 -c "import ast; ast.parse(open('amux-server.py').read())"`; `bash -n amux`.
7. **Commit.** `AMUX_COMMIT_STAMP=0 git commit -m "chore: merge upstream/main (<version>)"`
   — single line, no trailers.
8. **Land it.** From the main checkout: `git merge --ff-only tmp-upstream-merge`.
   The server auto-restarts. Wait ~5 s, then verify
   `curl -sk https://localhost:8822/api/sessions` returns 200 and
   `curl -sk https://localhost:8822/` returns 200.
   **If the server is broken:** `git reset --hard $PREV` (restores a known-good
   file, server restarts back), then report the failure and file a board task.
9. **Push + clean up.** `git push origin main`; `git worktree remove <scratch>/merge-wt`;
   `git branch -d tmp-upstream-merge`.
10. **Report.** One-paragraph summary: commits merged, conflicts resolved,
    verification results, and any Tier-2 warnings from step 4. On any
    abort/failure, add a board task titled "upstream sync failed: <reason>".

## Notes

- GitHub Actions is intentionally disabled on the fork — upstream workflow files
  merge in but never run.
- After a sync, skim upstream's diff for anything that writes outside the repo
  (e.g. `_ensure_no_native_artifacts()` mutates `~/.claude/settings.json`) and
  mention it in the report.
- `AMUX_AUTO_UPDATE_REPO` must stay **unset** on this host, or point at
  `origin` (`skalajan/amux`) — **never** `upstream` (`mixpeek/amux`). This env
  var controls what `_auto_update_check` overwrites `amux-server.py` from
  wholesale; pointing it at `upstream` would let the in-app auto-updater
  self-clobber every local delta outside of this SOP entirely.
