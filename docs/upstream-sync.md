# Upstream sync procedure

**This fork is mid-migration from a Python server (`amux-server.py`) to upstream's Rust
workspace — see [`../.omc/plans/rust-migration.md`](../.omc/plans/rust-migration.md) for
the full plan and current phase.** Two different procedures apply depending on where that
migration stands, and whoever runs the recurring weekly **"Sync fork with upstream"**
scheduler entry (fires into the `amux-helper` session) needs to check the plan's status
before picking one:

- **Pre-cutover (still true as of 2026-08-17):** this fork still ships `amux-server.py` in
  production on mac-brain (port 8822), and the weekly job still 3-way-merges it against
  upstream's Python history exactly as it always has. Upstream stopped touching that file
  at `792ce1f4` (2026-08-09), so in practice these merges have been no-ops since — but the
  mechanism itself is unchanged and still correct. Use **Part A** below.
- **Post-cutover (once plan phase P4 lands):** this fork keeps `amux-server.py` in the repo
  deliberately — frozen rollback path and the only surviving Python parity oracle — so the
  job still runs a real `git merge upstream/main`, history-preserving, not a reset. What
  changes is the shape of the conflicts: this fork is designed to carry effectively zero
  in-tree Rust deltas (phase P3's gate), so `crates/` merges in cleanly every time, and the
  handful of files that do conflict are a known, mostly-stable set — see **Part B** below.

This doc doesn't track cutover status itself; the plan does. Don't run Part B until the
plan says P4 has landed, and don't keep running Part A after it has.

---

## Part A — pre-cutover: merge `amux-server.py` (current procedure)

SOP for the recurring scheduler entry while this fork still runs the Python server. Keeps
`skalajan/amux` (origin) up to date with `mixpeek/amux` (upstream) while preserving the
local modifications documented in [`../MODIFICATIONS.md`](../MODIFICATIONS.md).

1. **Preflight.** `git -C ~/Desktop/Projects/amux fetch --all` and check
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
   `curl -sk $AMUX_URL/api/sessions` returns 200 and
   `curl -sk $AMUX_URL/` returns 200.
   **If the server is broken:** `git reset --hard $PREV` (restores a known-good
   file, server restarts back), then report the failure and file a board task.
9. **Push + clean up.** `git push origin main`; `git worktree remove <scratch>/merge-wt`;
   `git branch -d tmp-upstream-merge`.
10. **Report.** One-paragraph summary: commits merged, conflicts resolved,
    verification results, and any Tier-2 warnings from step 4. On any
    abort/failure, add a board task titled "upstream sync failed: <reason>".

### Notes (Part A)

- GitHub Actions is intentionally disabled on the fork — upstream workflow files
  merge in but never run.
- After a sync, skim upstream's diff for anything that writes outside the repo
  (e.g. `_ensure_no_native_artifacts()` mutates `~/.claude/settings.json`) and
  mention it in the report.
- `AMUX_AUTO_UPDATE_REPO` must stay **unset** on this host, or point at
  `origin` (`skalajan/amux`) — **never** `upstream` (`mixpeek/amux`). This is a
  Python-server-only mechanism (`_auto_update_check`, controlling what repo
  `amux-server.py` gets overwritten from). It stops mattering once `amux-server.py`
  stops being the *live* server at cutover (it's retained afterward, but frozen, so
  nothing calls `_auto_update_check` against it anymore) — see
  [`../.claude/rules/extend-via-sidecar.md`](../.claude/rules/extend-via-sidecar.md)
  for what, if anything, replaces it.

---

## Part B — post-cutover: `git merge upstream/main` (still a real merge, now smaller)

**Correction worth stating plainly, because an earlier draft of this section got it wrong:
this is a normal, history-preserving `git merge`, not a reset-and-replay re-baseline.**
Tested directly against this fork's post-cutover tree: `git merge upstream/main` touches
682 files and produces exactly **10 conflicts** — everything else, including the entire
`crates/` tree, merges automatically. History is preserved, so a later `git pull upstream`
keeps working the ordinary way. The reason the conflict count is small despite 682 files
and 1,237+ commits moving is structural, not luck: by design (phase P3's gate), this fork
carries **zero in-file Rust deltas** — every surviving delta is config, a sidecar, or the
unchanged bash CLI (see "Target placement" in [`../MODIFICATIONS.md`](../MODIFICATIONS.md))
— so `crates/` has no fork-owned text for upstream's changes to collide with. The 10
conflicts are exactly the files this fork keeps genuinely modifying in parallel with
upstream. Verified against `upstream/main` directly (not against upstream's docs, per the
standing principle below): all 10 paths exist upstream too, so every one is a real
modify/modify or modify/delete conflict, not an artifact of the check.

The procedure mirrors Part A's shape — preflight, worktree, resolve, verify, land — with a
different, now much shorter, conflict list in place of the old registry-row freshness gate:

1. **Preflight.** `git fetch --all` (same fix as Part A — applies to any remote).
2. **Record the rollback point.** `PREV=$(git rev-parse main)`.
3. **Merge in a temporary worktree — never in place**, same reasoning as Part A: don't let
   a live process see conflict markers mid-resolution. `git worktree add <scratch>/merge-wt
   -b tmp-upstream-merge main`, then `git merge upstream/main` inside it.
4. **Resolve the known conflict set.** Two categories, because they behave differently on
   repeat syncs:

   **One-time conflicts** — already resolved once, at the first post-cutover merge; they
   shouldn't recur, because the merge-base moves past the resolution:
   - `amux-server.py` — modify/delete (upstream deleted it at `792ce1f4`; this fork keeps
     modifying/retaining it). **Resolve: keep ours.** It's the frozen rollback + parity
     oracle — see `.claude/rules/single-file.md`. This is a deliberate, permanent keep, not
     a conflict to re-litigate each sync.
   - `.claude/settings.json` — modify/delete the other way (upstream keeps adding their own
     hook automation — `session-freshness.sh`, `auto-deploy.sh`, etc.; this fork's history
     deliberately deleted its own copy). **Resolve: keep ours (deleted).** Upstream's hooks
     are wired to their own workflow; don't resurrect them by accident during a merge.

   **Recurring conflicts** — both sides keep independently editing these paths, expect to
   resolve them again on every future sync:
   - `.claude/rules/single-file.md` — upstream's own copy at the same path now describes
     *their* post-migration shape (`crates/amux-dashboard` static files, `cargo check
     --workspace`); ours describes this fork's specific migration story and the retained
     Python oracle. **Resolve: keep ours**, permanently — the two files serve different
     forks' realities and will never converge.
   - `.claude/commands/aissue.md`, `.claude/commands/amux-board.md`, `skills/aissue.md`,
     `skills/amux-board.md`, `skills/amux.md` — living command/skill docs both sides
     actively maintain. **Resolve: real reconciliation, not a blanket side.** Keep this
     fork's `$AMUX_URL`/account-routing-specific edits; fold in upstream's non-overlapping
     improvements. Diff for substance each time.
   - `amux` (bash CLI) — unchanged mechanism, per the existing
     [Local Delta Registry](../MODIFICATIONS.md#local-delta-registry): keep the
     MODIFICATIONS.md hunks (config-dir flags, yolo default, default model, remote-control)
     **and** upstream's new commands — both sides.
   - `docs/rust-migration/ux-parity-report.md` — a generated report; upstream happens to
     produce one at the same conventional path from their own migration testing.
     **Resolve: keep ours**, and regenerate it fresh by re-running
     `e2e/parity-tasks.mjs` after the merge lands rather than trusting either committed
     copy — it's a measurement, not a document.

   A conflict **outside** this list that isn't trivially mechanical → abort: remove the
   worktree, report what conflicted, and file a board task. Do not guess. If `crates/`
   itself ever conflicts, that's a signal this fork accidentally picked up an in-tree Rust
   delta somewhere — treat it as a Tier-1-style blocking failure and escalate; it should
   never happen by design.
5. **Verify in the worktree.** No `<<<<<<<`/`>>>>>>>` markers anywhere;
   `cargo check --workspace` (upstream's own gate — confirmed present in their
   `.claude/rules/single-file.md`); `python3 -c "import ast; ast.parse(open('amux-server.py').read())"`
   (still worth checking the retained oracle wasn't corrupted by the merge); `bash -n amux`;
   each sidecar's own test suite (`amux-telegram.py`'s `test_telegram_*`, `amux-chat.py`'s
   tests once it exists); re-run `e2e/parity-tasks.mjs` and diff against the last dated
   report — new divergences get logged for manual triage (blocking: data-shaped, e.g. board
   or session facts; cosmetic: additive Rust-only fields — the same distinction plan phase
   P2 used), not silently accepted.
6. **Commit.** `AMUX_COMMIT_STAMP=0 git commit -m "chore: merge upstream/main (<version>)"`
   — single line, no trailers.
7. **Land it.** From the main checkout: `git merge --ff-only tmp-upstream-merge`. Rebuild
   and redeploy the Rust binary (mac-server's deploy model — rebuild-on-pull vs. a signed
   binary — is still undecided, plan phase P5; don't assume one until it's resolved). Wait
   for the new process to come up, then verify `curl -sk $AMUX_URL/api/sessions` returns
   200. Re-verify the Telegram round-trip — continuity through a sync is a release gate,
   not an afterthought (plan pre-mortem S3).
   **If broken:** `git reset --hard $PREV`, redeploy the previous binary, report the
   failure, file a board task.
8. **Push + clean up.** `git push origin main`; `git worktree remove <scratch>/merge-wt`;
   `git branch -d tmp-upstream-merge`.
9. **Report.** One-paragraph summary: commits merged, which of the 10 known conflicts
   appeared and how each resolved, parity divergences (new vs. carried-over), sidecar +
   Telegram verification results. On any abort/failure, add a board task titled "upstream
   sync failed: <reason>".

### Notes (Part B)

- There is no `AMUX_AUTO_UPDATE_REPO` equivalent yet. Don't assume the Rust server has any
  self-update behavior until it's confirmed by reading `crates/amux-server` directly.
- mac-server's deploy model (rebuild-on-pull vs. shipping a signed binary) is undecided —
  plan phase P5. Don't write a specific mechanism into automation until that's resolved.
- If a merge ever produces a conflict in `crates/`, or a conflict outside the 10 known
  paths that persists across syncs, update this list in the same commit — a stale conflict
  list here is the same failure mode Part A's old Tier-1 gate existed to catch.

---

## Standing principle: upstream docs are leads, not evidence

**Verify against `crates/`, never against `docs/`.** Three claims from upstream's own
documentation were measured false or stale during this migration, every one of them caught
only by reading source, not prose:

- The parity test harness (`e2e/parity-tasks.mjs`) was silently deleted in the same commit
  (`792ce1f4`) that removed the Python oracle it was built to test against — nothing in
  upstream's docs flagged this; it was recovered from the parent commit (plan phase P0).
- Upstream's docs claimed rollback needs nothing from the database. False: 12 of
  `0013_search.sql`'s 24 migration triggers attach to shared tables (`issues`, `schedules`,
  `journal_entries`) and survive a server rollback, firing against Python's own writes
  after a "rollback" that supposedly restored Python-only behavior.
- `server-boundary.md` documented port 8822 as surviving with an exit condition.
  `lib.rs:533` removed the compatibility bind the very next day — "THE LEGACY 8822 BIND IS
  GONE."

Every one of these would have gated a destructive step on a false premise if taken from the
docs at face value. Treat every upstream doc — migration guides, README claims, and code
comments describing intent — as a lead to verify against `crates/`, never as a fact to act
on directly. This applies doubly hard to both parts of this sync procedure: don't take a
release note's word for what changed in a given upstream commit — diff the actual config
surface and re-run parity.
