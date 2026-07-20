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
4. **Resolve conflicts.** Known local-feature areas (keep local behavior, graft
   upstream refactors around it):
   - Account routing / multi-home: `_pick_claude_config_dir`, `_session_claude_config_dir`,
     `_claude_config_homes`, `_claude_project_dir` — keep local one-liners like
     `project_dir = _claude_project_dir(...)` over upstream's `CLAUDE_HOME / "projects" / ...`.
   - Token stats: adopt upstream's logic, but iterate all config homes.
   - `start_session()` cmd build: keep the `CLAUDE_CONFIG_DIR=` env prefix wrapped
     around whatever base command upstream uses.
   - POST /api/sessions: keep the `config_dir` ("work"/"personal"/path) block.
   - `amux` CLI: keep the MODIFICATIONS.md hunks (config-dir flags, yolo default,
     default model, remote-control) AND upstream's new commands — both sides.
   - `AMUX_COMMIT_STAMP` toggle in the commit-stamp hook.

   A conflict **outside** these areas that isn't trivially mechanical → abort:
   remove the worktree, report what conflicted, and file a board task. Do not guess.
5. **Verify in the worktree.** No `<<<<<<<`/`>>>>>>>` markers anywhere;
   `python3 -c "import ast; ast.parse(open('amux-server.py').read())"`; `bash -n amux`.
6. **Commit.** `AMUX_COMMIT_STAMP=0 git commit -m "chore: merge upstream/main (<version>)"`
   — single line, no trailers.
7. **Land it.** From the main checkout: `git merge --ff-only tmp-upstream-merge`.
   The server auto-restarts. Wait ~5 s, then verify
   `curl -sk https://localhost:8822/api/sessions` returns 200 and
   `curl -sk https://localhost:8822/` returns 200.
   **If the server is broken:** `git reset --hard $PREV` (restores a known-good
   file, server restarts back), then report the failure and file a board task.
8. **Push + clean up.** `git push origin main`; `git worktree remove <scratch>/merge-wt`;
   `git branch -d tmp-upstream-merge`.
9. **Report.** One-paragraph summary: commits merged, conflicts resolved, verification
   results. On any abort/failure, add a board task titled "upstream sync failed: <reason>".

## Notes

- GitHub Actions is intentionally disabled on the fork — upstream workflow files
  merge in but never run.
- After a sync, skim upstream's diff for anything that writes outside the repo
  (e.g. `_ensure_no_native_artifacts()` mutates `~/.claude/settings.json`) and
  mention it in the report.
