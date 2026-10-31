# Fork delta record — what this fork had before the 2026-10-31 upstream resync

This repo was re-baselined onto `upstream/main` (`mixpeek/amux`) on **2026-10-31**,
deliberately discarding the fork's own 107 commits rather than merging them. The
decision was "we will probably implement it from scratch" — so this file is the
record of *what existed*, *why*, and *what is still missing from upstream today*.

It is not a changelog. It is the input to deciding what to rebuild.

## Coordinates

| Fact | Value |
|---|---|
| Fork remote (`origin`) | `https://github.com/skalajan/amux.git` |
| Upstream | `https://github.com/mixpeek/amux` |
| Merge base (last common commit) | `a0f762f85` — 2026-09-10 |
| Fork head before resync | `34dbac9cc` — 2026-09-14 |
| Upstream head at resync | `f104203cc` — 2026-10-31 |
| Divergence | **107 fork commits ahead, 3753 upstream commits behind** |
| Size of the discarded delta | 62 files, +79,956 / −906 lines |

### Recovering any of it

Nothing was deleted from git. Every line below is one command away:

```bash
git show fork-pre-upstream-resync-2026-10-31:<path>        # read one file
git checkout fork-pre-upstream-resync-2026-10-31 -- <path> # restore one file
git log fork-pre-upstream-resync-2026-10-31                # full fork history
```

| Ref | Points at |
|---|---|
| `fork-pre-upstream-resync-2026-10-31` (tag) | the fork head `34dbac9cc` |
| `backup/fork-main-2026-10-31` (branch) | same commit, branch form |
| `backup/pre-upstream-sync-2026-09-10` | the previous re-baseline's backup |
| `origin/main` | `35ff724c0` (2026-08-22) — the fork remote was never pushed past this |

Companion files in this directory: `MODIFICATIONS.md` (the fork's own delta
registry, verbatim — the per-hunk reapply instructions live there),
`commits.txt` (all 107 subjects), `diffstat.txt` (per-file line counts),
`docs/` and `rules/` (the fork's design docs).

## Read this first: upstream has moved under three of these features

The 3753 upstream commits are not neutral. Before rebuilding anything, check
what upstream already ships — measured on `upstream/main` at `f104203cc`:

| Fork feature | Upstream today | Verdict |
|---|---|---|
| Telegram bridge | `api/telegram.rs`, `db/telegram.rs`, `runtime_jobs/telegram_poll.rs`, `telegram_relay.rs`, migrations `0050`–`0054` | **Superseded in the core.** Rebuild only the extras (list below). |
| Session chat capture | `api/chat_stream.rs`, `chat_worker.rs`, `chat_context.rs`, `chat_delegate.rs` | **Superseded.** Do not port `amux-chat.py`. |
| CLI write-auth | `amux` reads `$CC_HOME/auth_token` and sends `Authorization: Bearer` from its `_curl` chokepoint | **Superseded.** The graft is dead weight now. |
| Account routing (`CLAUDE_CONFIG_DIR`) | one unrelated use in `api/skills.rs` (skills dir lookup only) | **Still missing.** Highest-value rebuild. |
| Drain-restart | `drain_restart` — zero hits anywhere | **Still missing.** |
| F-5 / F-6 / F-7 Rust deltas | `force_requires_human`, `AMUX_WORKER_ENV_REFUSED` — zero hits | **Still missing.** Cheapest to rebuild (3 small seams). |

## 1. Still missing upstream — rebuild candidates, cheapest first

### 1a. The three in-tree Rust deltas (F-5, F-6, F-7)

Small, surgical, well-specified, and each has tests that were written against it.
Full grep landmarks and anchor descriptions are in `MODIFICATIONS.md` rows F-5/F-6/F-7.

**F-6 — `force` reserved for human actors.** `crates/amux-server/src/api/board.rs`,
in `patch_item`: a `force` from a worker or session actor is refused `403
force_requires_human`. The refusal sits **above** the "force requires attribution"
400, deliberately — checked first, so a worker cannot slip through by omitting its
header (Human is tested before Machine/Anonymous). Tests:
`a_worker_may_not_force_its_own_card_but_a_human_may`.
Why it existed: `force` is the board's override of its own gates; a machine using
it turns an attributed, gated mutation into an unattributed one.

**F-5 — worker `environment` reaches the headless child.**
`crates/amux-server/src/opencode/structured.rs` + `backend/bootstrap.rs`. The worker
`environment` map reached only the interactive tmux spawn, so orchestrator-dispatched
(headless) runs silently ran without it — which also meant the "account routing is
config-only" claim in the old registry was **false** for dispatched work. Adds `env`
to `WorkerConfig` (immediately after `conversation`) and moves the spawn's
program/argv/cwd/stdio/env build into `build_command`. Tests:
`worker_env_reaches_the_headless_child`,
`worker_env_cannot_repoint_the_servers_own_state`.

**F-7 — one env refusal list, on BOTH spawn legs.** Same two files. A single
`env_is_refused` / `spawn_env` helper (in `bootstrap.rs`, above `ProtocolRegistrar`)
filters refused keys, and **both** call sites use `env: spawn_env(&row.environment,
&row.id)`. The headless leg does not re-filter what bootstrap handed it. Tests:
`refused_env_keys_reach_neither_spawn_leg`,
`the_refusal_list_covers_each_family_and_spares_the_rest`,
`the_headless_leg_does_not_re_filter_what_bootstrap_handed_it`.
Why it existed: without it a worker env key could repoint the server's own state.

> These three were the fork's only in-`crates/` deltas, taken deliberately after the
> "zero in-file Rust deltas" gate — each is a seam with no sidecar equivalent: F-5
> adds a field to a struct only upstream constructs, F-6 changes an upstream
> handler's admission decision, F-7 filters an upstream map at the one site upstream
> builds both spawn specs from. If rebuilt, they will be in-tree again. That is the
> honest cost, not an oversight.

### 1b. Account routing / multi-home (`CLAUDE_CONFIG_DIR`)

**The highest-impact loss, and it changes day-to-day behaviour.** Eight
`AMUX-LOCAL:account-routing` sentinel blocks in the `amux` bash CLI selected which
Claude.ai account a session runs as, by path:

| Variable | Default |
|---|---|
| `AMUX_WORK_PATHS` | `$HOME/Desktop/Projects/Fid…` |
| `AMUX_WORK_CONFIG_DIR` | `$HOME/.claude-work` |
| `AMUX_PERSONAL_CONFIG_DIR` | `$HOME/.claude-personal` |

Mechanism: `pick_default_config_dir` helper, three flags in `parse_claude_flags`,
`cmd_register` auto-detect+persist (merging, not truncating, upstream's preserved
`CC_*` keys), and `cmd_start` exporting `CLAUDE_CONFIG_DIR` in `shell_setup`.
Per-hunk reapply instructions: `MODIFICATIONS.md` Hunks 1, 2, 3, 5.

Known gaps carried at the time, worth fixing rather than reproducing:
- `cmd_exec` never persisted `CC_CONFIG_DIR`, so `amux exec` sessions fell through
  to the default account (Hunk 4 — documented but never actually live).
- The OAuth probe read `$HOME/.claude.json`, not `${CC_CONFIG_DIR:-$HOME}/.claude.json`.
- A re-register writes `CC_CONFIG_DIR` twice; the pinned value wins. A wart, not a bug.

### 1c. CLI defaults (`AMUX-LOCAL:cli-defaults`, 2 blocks)

Session default model `claude-opus-5[1m]`, auto-appended `--remote-control`, and
`--yolo` (`--dangerously-skip-permissions`) on by default with `AMUX_NO_YOLO=1` to
opt out. `AMUX_DEFAULT_MODEL` and `AMUX_NO_REMOTE_CONTROL` were the knobs.
Reapply: `MODIFICATIONS.md` Hunk 6. (`--no-yolo` as a *flag* never existed; only
the env var did.)

### 1d. `amux-drain-restart.py` (541 lines + 321 lines of tests)

Lossless fleet restarts: drain sessions, restart, bring them back. No upstream
equivalent at all. Design doc: `docs/drain-restart.md` in this directory.

### 1e. mac-server deploy kit (`deploy/mac-server/`, ~1200 lines)

`install.sh`, `amux-pull-update.sh` (pull-only auto-update runner with a storm guard
so a repeating condition alerts once), four launchd plist templates
(`serve`, `telegram`, `pull-update`, `start-all`), env examples,
`pull-reconcile-allowlist.txt`, and `test-pull-update.sh`. Upstream ships
Linux/systemd + Docker; this was the macOS answer. Runbook: `docs/mac-server-deploy.md`.

### 1f. Telegram extras not covered by upstream's native bridge

Upstream owns the transport now. These were the fork's behaviour on top, and each
is a separate decision to re-make (sources: `amux-telegram.py`, 3031 lines, plus
~2,660 lines of tests in `tests/test_telegram_*.py`):

- **Quiet mode / answer-latch** — questions and failures only; rate limits stopped
  ringing, credit limits still did.
- **Permission-prompt notify + inline approve** — with ANSI stripped before
  classify/render, and suppressed while a tmux client is attached (you are at the CLI).
- **AI reply summaries** — `/mode`, `/last`, a reply-summary marker convention with a
  Haiku fallback summarizer (`docs/reply-summary.md`).
- **Origin-aware routing** for forwarded replies, with `/ring on|off`.
- **Owner-only `/type` and `/keys`** raw session injection.
- **`/quiet` fleet switch**, command logging, and a coverage gate that can fail.
- **Decision funnel + per-class counters**; prompt notify through the router with a
  10s→90s grace and one ring per waiting episode.
- **`tg-observe` live box**, capped at 1200 chars (`TG_LIVE_BODY_MAX`), with growing
  rewrite thresholds so it stops rewriting once a minute.
- Transport hardening: 4096-char `sendMessage` chunking, topic-creation crash fix,
  bounded retry, 8s poll timeout.

## 2. Recorded as obsolete — do NOT rebuild

Each of these was superseded by the architecture, not abandoned:

| Delta | Why it is dead |
|---|---|
| Localhost write-auth (server + CLI) | Replaced by `AMUX_RS_NO_LOOPBACK_BYPASS=1` (config) plus upstream's native bearer-sending `_curl`. The flag is blunter — it removes the bypass for reads too, which is why every `curl` recipe in this repo needs a token. |
| `AMUX_COMMIT_STAMP` toggle | Rust has its own commit-hook machinery (`AMUX_COMMIT_GUARD`). |
| Yolo-guardrail (never auto-approve permission prompts) | Rust never auto-answers; its adapter returns `waiting("permission_prompt", s)` as a typed state. |
| Stub-writer guard | The incident it guarded does not recur in the new architecture. |
| Session chat dashboard tab (`chat.js` / `chat.css`, 621 lines) | Retired by the fork itself — Telegram was always the real front-end. |
| `amux-chat.py` (1063 lines) | Upstream's `chat_stream.rs` / `chat_worker.rs` own this now. |
| `_acurl` CLI wrapper | Superseded by upstream's `_curl` chokepoint. Do not grep for it; it no longer exists. |

## 3. `amux-server.py` — the 65,888-line Python server

Upstream deleted theirs at `792ce1f4` (2026-08-09, swept in under an unrelated CSS
commit message) and never touched Python again. This fork kept it deliberately as
(a) the rollback path and (b) the **only surviving Python oracle** the parity
harness `e2e/parity-tasks.mjs` (402 lines, itself recovered from that deletion)
could ever run against.

It is **not** restored to the tracked tree by this resync — but see §5: it is still
running on this machine, so the file is kept on disk, gitignored. Its history and the parity
harness are both recoverable from the tag.

## 4. Documentation and process that went with it

- `MODIFICATIONS.md` — 487-line Local Delta Registry: the `AMUX-LOCAL:<feature>`
  sentinel convention, per-area grep landmarks, per-hunk reapply recipes, and the
  2026-09-10 merge's resolution notes. **Preserved verbatim next to this file.**
- `docs/upstream-sync.md` — the sync SOP (Part B: a real history-preserving
  `git merge upstream/main` with a 10-file known conflict set). Note that *this*
  resync did not follow it; it discarded instead of merging.
- `.claude/rules/extend-via-sidecar.md` — the sidecar-first ordering (config knob →
  sidecar → external addon → in-tree), and why upstreaming is permanently closed.
- `.claude/rules/single-file.md` — why `amux-server.py` is frozen rather than deleted.
- `docs/session-chat.md`, `docs/telegram-chat.md`, `docs/reply-summary.md`,
  `docs/drain-restart.md`, `docs/mac-server-deploy.md`.
- Smaller grafts: `scripts/hooks/hook-report.sh` bearer-token block (without it every
  report 401s), `integrations/procwarden` write-token, `skills/` + `.claude/commands/`
  auth annotations, `$AMUX_URL` indirection replacing hardcoded `:8822`.

All preserved in this directory or recoverable from the tag.

## 5. What this machine still runs (and why some files survived)

Four launchd jobs execute Python **directly out of this worktree**. A clean reset
would have deleted their program files — the processes would keep running on open
handles and then never start again. So these four files are kept on disk:

| Job | PID at resync | Program |
|---|---|---|
| `com.amux.serve` | 605 | `./amux-server.py` (Python server, :8822) |
| `com.amux.telegram` | 623 | `./amux-telegram.py` |
| `com.amux.chat` | 642 | `./amux-chat.py` |
| `com.amux.server-rs` | 645 | `~/.local/bin/amux-server-rs` (built binary — unaffected) |

`amux-drain-restart.py` is kept alongside them; it is invoked on demand, not by launchd.

All four are **untracked and listed in `.gitignore`**: upstream forbids resurrecting
`amux-server.py` in the tree, and an ignored file survives a routine untracked-file
cleanup (only the `-x` variant removes ignored files). Git's copy of record is the tag;
the files on disk were byte-compared against it at resync time.

Two more live couplings worth knowing:

- **`/usr/local/bin/amux` is a symlink into this worktree.** The CLI you type is
  this repo's `amux` file, so the resync swapped the live CLI from the fork's 5,099-line
  version to upstream's 11,760-line one in one step. Auth survives (upstream sends the
  bearer itself); **account routing and the CLI defaults did not** — see §1b/§1c.
- **The running Rust server was built from fork source** (`/health` reported commit
  `2f967e9fd`). It keeps F-5/F-6/F-7 behaviour until someone rebuilds it. The first
  rebuild from the new tree silently drops all three.

Config-only deltas live in `~/.amux/server.env`, outside git, and were **not** touched:
`AMUX_RS_NO_LOOPBACK_BYPASS`, `AMUX_DEFAULT_MODEL`, `AMUX_PERSONAL_CONFIG_DIR`,
`AMUX_COMMIT_STAMP`.

## 6. If you rebuild, rebuild in this order

1. **F-6, then F-7, then F-5** — small, tested, still absent upstream, and F-5/F-7
   share a seam. Do them before the first server rebuild, or you lose live behaviour
   you currently have.
2. **Account routing** (§1b) — the only loss that changes daily use. Fix the three
   known gaps rather than reproducing them.
3. **CLI defaults** (§1c) — one evening, mostly taste.
4. **Drain-restart** (§1d) — only if lossless restarts still matter.
5. **Telegram extras** (§1f) — on top of upstream's native bridge, one decision at a
   time. Do not port the sidecar wholesale.
6. **Deploy kit** (§1e) — only if this stays a macOS box.

Everything in §2 stays dead.
