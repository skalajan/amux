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
- **Post-cutover (once plan phase P4 lands):** there is no more `amux-server.py` to merge,
  and this fork is designed to carry effectively zero in-tree Rust deltas (phase P3's
  gate). The weekly job stops merging application source entirely. Use **Part B** below —
  a re-baseline onto a newer upstream Rust commit, gated by config-surface and parity
  checks instead of textual grep landmarks.

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
  `amux-server.py` gets overwritten from) and is retired along with that file at
  cutover — see [`../.claude/rules/extend-via-sidecar.md`](../.claude/rules/extend-via-sidecar.md)
  for what, if anything, replaces it.

---

## Part B — post-cutover: re-baseline onto Rust

Once `amux-server.py` is gone, "syncing with upstream" stops meaning "3-way-merge one
file's text" and starts meaning "adopt a newer upstream commit/release and re-verify the
fork's config-and-sidecar layer still holds against it." There is a structural reason the
old merge machinery doesn't carry over: by design (phase P3's gate), this fork's surviving
deltas live **outside** anything upstream tracks — config flags/env vars and standalone
sidecar scripts — so there is no fork-owned text inside `crates/` to merge in the first
place. As of 2026-08-17, this fork's Python-era base (`d42a8233`) sits **1,237 commits**
behind `upstream/main` (`6ebef136`), moving at roughly 620 commits/week — a number that
only grows and was never intended to be merged commit-by-commit.

1. **Preflight.** `git fetch --all` (unchanged from Part A — this fix already landed and
   still applies to any git remote, regardless of what's being fetched).
2. **Adopt.** Pull the target upstream commit or release. Rebuild (or fetch a signed
   binary — mac-server's deploy model for a compiled binary is still an open decision, plan
   phase P5) and redeploy to a non-live port first if at all practical, the same "measure
   before committing" posture the migration itself used (plan phase P2).
3. **Freshness gate — replaces the old Tier-1/Tier-2 grep-landmark checks.** The old gate
   asked "does the local delta's own inserted text still exist at a stable position in the
   merged file?" That question doesn't apply anymore because there's no inserted text.
   The equivalent question moves up a layer, from *text* to *behavior*:
   - **Config-surface check (BLOCKING, replaces Tier 1).** For each of this fork's
     permanent config-only deltas — write-auth's `AMUX_RS_NO_LOOPBACK_BYPASS`,
     account-routing's worker-scoped `CLAUDE_CONFIG_DIR` env injection via `bootstrap.rs`,
     and whatever the commit-stamp/yolo-guardrail open items (`MODIFICATIONS.md`) land on
     — grep the new upstream source for the flag/var name and smoke-test that it still
     does what this fork depends on. If it's gone or its semantics changed, that's the
     blocking case: stop, don't guess, escalate. Same severity as old Tier 1, different
     instrument.
   - **Parity/E2E check (WARN-then-log, replaces Tier 2).** Re-run the recovered harness
     (`e2e/parity-tasks.mjs`, recovered from `792ce1f4^` — see plan phase P0) against the
     newly adopted build. New divergences versus the last dated report
     (`docs/rust-migration/ux-parity-report.md`) get logged in the sync report for manual
     triage — blocking (data-shaped: board/session facts) vs. cosmetic (additive
     Rust-only fields), the same distinction plan phase P2 used.
   - **Sidecar check (new — this tier didn't exist before).** Run each sidecar's own test
     suite (`amux-telegram.py`'s `test_telegram_*`, `amux-chat.py`'s tests once it exists)
     plus a live Telegram round-trip probe. Sidecars are now the only thing this fork
     carries that can break silently against an upstream API change — a Python 3-way merge
     used to catch a renamed function at merge time; a sidecar calling a reshaped HTTP
     endpoint instead fails quietly at runtime. This check exists because that failure
     mode is new, not because the old one got harder.
4. **Report.** Dated summary: upstream commits absorbed (count + range), config-surface
   gate result, parity divergences (new vs. carried-over), sidecar test + Telegram
   round-trip results. On any blocking failure: stop, don't deploy, file a board task —
   same posture as Part A.

### Notes (Part B)

- There is no `AMUX_AUTO_UPDATE_REPO` equivalent yet. Don't assume the Rust server has any
  self-update behavior until it's confirmed by reading `crates/amux-server` directly.
- mac-server's deploy model (rebuild-on-pull vs. shipping a signed binary) is undecided —
  plan phase P5. Don't write a specific mechanism into automation until that's resolved.

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
