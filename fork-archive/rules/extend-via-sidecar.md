Keep fork-local code out of anything upstream tracks. This used to mean one ~40k-line
Python file (`amux-server.py`) that the in-app auto-updater could overwrite wholesale. It
now means a 183-`.rs`-file Rust workspace under `crates/` (`amux-core`, `amux-server`,
`amux-cli`, `amux-dashboard`) moving at roughly 620 commits/week, with **upstreaming
permanently declined by the owner** — nothing from this fork goes back to `mixpeek/amux`,
so every line carried inside `crates/` is carried forever against a target that never stops
moving. See [`single-file.md`](single-file.md) for the migration background and
`.omc/plans/rust-migration.md` for the phased plan.

**The sidecar-first ordering gets stronger with this migration, not weaker.** Two reasons:

1. **A bad graft in Python limped; a bad graft in Rust doesn't compile.** An error in one
   function of `amux-server.py` broke that function — the other 78,000 lines kept running.
   An in-tree edit to `crates/` that doesn't type-check breaks `cargo build` for the whole
   workspace, for every subsequent `git pull`, until someone notices and fixes it. There is
   no such thing as a small, contained mistake inside a compiled workspace the way there was
   inside a Python file.
2. **Upstreaming is closed, permanently.** The old version of this file said "consider
   filing a PR to `mixpeek/amux`... the smallest durable delta is the one that no longer
   exists." That escape hatch is gone — the owner has declined upstreaming outright, for
   both code and defect reports. A delta that lands inside `crates/` today is a delta this
   fork maintains, alone, against 620 commits/week, indefinitely. Minimising carried surface
   is now the dominant design driver for anything touching the Rust tree (Principle 1 in
   `.omc/plans/rust-migration.md`).

## Order of preference for new fork-local functionality

0. **Check for a config knob first.** Before reaching for a sidecar, check whether upstream
   already exposes what you need as an env var or config flag — cheaper than any delta,
   including a sidecar, because it costs zero carried code and survives every upstream pull
   unmodified. Two of this fork's oldest deltas resolved this way at the Rust cutover:
   account routing (`CLAUDE_CONFIG_DIR` as a worker-scoped env var — `bootstrap.rs` injects
   worker env into the spawn) and localhost write-auth (`AMUX_RS_NO_LOOPBACK_BYPASS=1`).
   Neither needs a single line inside `crates/`. See `MODIFICATIONS.md` for both, including
   write-auth's caveat — the flag is blunter than the delta it replaces.
1. **Sidecar process / standalone script** — a separate file (e.g. `amux-telegram.py`,
   `amux-chat.py`) that talks to the running server over its HTTP + SSE API. This is the
   default for any new integration, and the only tier that scales against a
   620-commit/week upstream. **The auth model changed at the Rust cutover**: the Python
   server's `_check_auth` always bypassed for localhost peers, so no token was needed. The
   Rust server's bypass is itself a config flag (`auth.rs` rule 2, "localhost peers always
   pass" — on by default), and **this fork sets `AMUX_RS_NO_LOOPBACK_BYPASS=1`**, which
   removes the bypass for reads as well as writes. Every sidecar now sends
   `Authorization: Bearer <token>` on every request, not just writes — see the write-auth
   row in `MODIFICATIONS.md`.
2. **External addon file** — a separate script the server is *configured* to invoke, never
   inlined. Carried over from the Python era on the assumption it still applies; it has not
   been verified against the Rust server's actual config surface. Don't assume this tier
   exists until someone confirms it — treat tier 1 as the default until then.
3. **In-tree change inside `crates/`** — **not viable in practice, not just discouraged.**
   The migration plan's own delta-placement gate (phase P3) requires *zero* in-file Rust
   deltas before cutover; if a placement genuinely cannot avoid one, that is a material
   change to the cost of the whole migration and stops for the user's decision — it is not a
   default path an agent takes the way tier 3 used to be for `amux-server.py`. The one place
   this tier still legitimately exists is the `amux` bash CLI, which survives upstream as a
   single tracked legacy-client script (same shape as before) — see below.

## The sentinel + registry convention now applies to exactly one file: `amux`

The `# AMUX-LOCAL:<feature>` sentinel convention and the Local Delta Registry in
[`MODIFICATIONS.md`](../../MODIFICATIONS.md) were designed for grafting local diffs into one
big tracked Python file via 3-way merge. That mechanism still works, unchanged, for the
`amux` bash CLI, because it's still exactly that shape: one tracked file this fork carries
deltas inside.

It does not extend to `crates/`. There is no single insertion point to sentinel-wrap across
183 files that upstream itself reorganizes at 620 commits/week, and per tier 3 above there
shouldn't be a delta there to wrap in the first place. If a `crates/` delta is ever
unavoidable, the honest move is to say so explicitly in `MODIFICATIONS.md` and treat it as a
standing risk to flag on every sync — not to force it through the old sentinel pattern.

If clauses 0–2 genuinely can't work for a `crates/`-touching feature:

1. **Escalate, don't graft.** Stop and get the user's decision before writing to `crates/` —
   see phase P3's gate in `.omc/plans/rust-migration.md`.
2. If the user accepts an in-tree delta anyway, it still needs a `MODIFICATIONS.md` row —
   grep landmarks, resolution note, `Upstreamable?` column (marked inert; see that file for
   why) — in the same commit, same as any tracked-file delta always has.

For `amux` specifically, the existing checklist is unchanged:

1. **Sentinel-wrap it.** `# AMUX-LOCAL:<feature>` … `# /AMUX-LOCAL:<feature>`. Never the
   house style `# ── … ──` — that collides with a divider convention the file may still use.
2. **Register it** in the [Local Delta Registry](../../MODIFICATIONS.md#local-delta-registry)
   in the same commit. A delta with no registry row isn't real.
3. **Put the resolution note in the registry row**, not here.
4. **Assess upstreamability** and mark the row's `Upstreamable?` column — inert for now (see
   `MODIFICATIONS.md`'s header note on why), but still worth recording what *could* have gone
   up, in case that decision is ever revisited.

**Commit conventions (unchanged):**
- One commit per completed task; single-line commit message, no body, no trailers, no
  co-author line.
- `AMUX_COMMIT_STAMP=0` for merge/re-baseline commits (see `upstream-sync.md`).
- Never edit a live-watched file in place during a merge. This applied to `amux-server.py`
  because the running server watched its mtime and re-executed on save — conflict markers
  would have broken it live. The Rust server doesn't hot-reload from source the same way,
  but the underlying rule — merge in a scratch worktree, never in the checkout something is
  actively running from — still holds. See `upstream-sync.md`.

**Verification, per tier:**
- `amux` (bash CLI): `bash -n amux` — unchanged.
- Sidecars: whatever that sidecar's own test suite is (e.g. `amux-telegram.py`'s
  `test_telegram_*`, `amux-chat.py`'s `test_chat_core` once it exists) plus a live smoke
  check against the running server — `curl -sk $AMUX_URL/api/sessions`, expect 200.
  `$AMUX_URL` defaults to `https://localhost:8822` before cutover, `:8824` after (see
  `.omc/plans/rust-migration.md` phase P3b).
- `amux-server.py`: no longer a target for new verification — it isn't taking new changes.

**Author-time advisory check, `amux`-scoped.** Run this manually before committing any
staged change to `amux`:

```bash
git diff --cached -- amux | grep -q '^+.*AMUX-LOCAL:' \
  && ! git diff --cached --name-only | grep -qx 'MODIFICATIONS.md' \
  && echo "WARNING: staged AMUX-LOCAL sentinel change with no MODIFICATIONS.md update — add/update its Local Delta Registry row first" \
  || true
```

(This used to also cover `amux-server.py`. `amux-server.py` itself isn't going anywhere —
this fork keeps it deliberately as a frozen rollback path and parity oracle, see
`single-file.md` — but it stops being a target for *new* changes at cutover, so this check
no longer needs to watch it.)

**The `AMUX_AUTO_UPDATE_REPO` guardrail stops mattering once `amux-server.py` stops being
the live server — the file is retained, but the mechanism it guarded isn't.**
`_auto_update_check` was a Python-server feature: an env var told it which repo's raw
`amux-server.py` to pull and overwrite the local file with on every restart check, and this
guardrail existed to make sure that var never pointed at `upstream` (which would have let
the auto-updater self-clobber every local delta). At cutover, `amux-server.py` becomes a
frozen artifact nothing restarts or self-updates against, so `_auto_update_check` has
nothing left to call. **What replaces it is not yet decided.** The Rust-side equivalent of
"stay current with upstream" is a real `git merge upstream/main`
([`upstream-sync.md`](../../docs/upstream-sync.md) Part B — history-preserving, not a
reset), not an in-process auto-updater, and mac-server's own deploy model (rebuild-on-pull
vs. shipping a signed binary) is an open decision (`.omc/plans/rust-migration.md` phase
P5). Don't assume any particular auto-update behavior exists on the Rust side until it's
confirmed by reading `crates/amux-server` directly — upstream's docs are leads, not
evidence (see `upstream-sync.md`'s standing principle on this).

Related: [`single-file.md`](single-file.md) covers the retirement of `amux-server.py` itself
and why there's no fork-owned application file to split anymore; this rule covers where new
fork-local code goes instead.
