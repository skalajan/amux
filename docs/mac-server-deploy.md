# mac-server deploy runbook

Deploys amux to a second Mac ("mac-server"), sourced from this fork
(`skalajan/amux`), with changes made here auto-propagating to mac-server on a
cadence via a pull-only git updater. Companion artifacts live in
[`../deploy/mac-server/`](../deploy/mac-server/). Full design rationale,
decision drivers, and rejected alternatives: `.omc/plans/mac-server-deploy.md`
(this doc is the operational runbook; that plan is the design record — read it
if something here seems surprising).

**Scope of this doc:** Phases 3-6 (everything that needs hands on mac-server).
Phases 1-2 (the deploy kit itself: `install.sh`, plist templates,
`amux-pull-update.sh`, the reconcile allowlist, `.env.example` files) are
already built and live in `deploy/mac-server/` — nothing in this doc modifies
`amux-server.py`, the `amux` CLI, or any other existing tracked file.

**Execution mode (Decision 3, still open):** every step below can be run
by hand on mac-server's own keyboard, or scripted over SSH from mac-brain.
`install.sh` is fully non-interactive-capable (flags/env for every value, a
single confirmation prompt skippable with `--yes`) so either mode works
unmodified. **Full Disk Access (Phase 4) is a manual GUI step under EITHER
mode** — it cannot be scripted or done over SSH; the runbook halts there for a
human regardless.

---

## What NOT to run here

- **Do not** set `AMUX_AUTO_UPDATE_REPO` to anything, on mac-server. Propagation
  is the git-pull updater (`amux-pull-update.sh`), never the built-in
  `_auto_update_check` — the two are mutually exclusive on a real git checkout
  (the built-in one `write_bytes`-overwrites `amux-server.py` wholesale, which
  desyncs the checkout from git and breaks `git pull --ff-only` from then on).
  Leave it commented/unset in `~/.amux/server.env` (see `server.env.example`).
- **Do not** register the SCHED-1 "sync fork with upstream" schedule on
  mac-server — mac-brain owns that (it's the only host that pushes to origin).
- **Do not** register any work session (Fidoo, paymentapp, or anything
  client/production-adjacent) on mac-server — personal dev sessions only (see
  "Initial fleet" below).
- **Do not** bootstrap/enable `com.amux.pull-update` before Phase 3.b is done.
  `install.sh` refuses to do this itself, under any flag — see Phase 6.
- **Do not** touch the host's separate "agents" system (its own bot, its own
  launchd jobs, its own `claude -p` usage) — it's a hard boundary, not
  something this deploy shares state or launchd labels with. See the Preflight
  disjointness proof below.

---

## Preflight (Phase 3) — needs mac-server access

Fill in this table on mac-server before running `install.sh`:

| Item | How to check | Recorded value |
|---|---|---|
| Account username | `whoami` — drives every path substitution below | |
| macOS version | `sw_vers` | |
| `python3` version (need >=3.10) | `python3 --version` | |
| `python3` path | `command -v python3` (plists default to `/usr/local/bin/python3`) | |
| `git` present | `command -v git` | |
| `tmux` present | `command -v tmux` | |
| `node` >=22 (only if the chrome-cdp skill is wanted — not required for core) | `node --version` | |
| `claude` CLI present | `command -v claude` | |
| Personal-account config dir(s) logged in | see Phase 5 | |
| Full Disk Access state for the server's python | System Settings -> Privacy & Security -> Full Disk Access (manual inspection — see Phase 4) | |
| Port 8822 free | `lsof -i :8822` | |
| Tailscale up; hostname/IP | `tailscale status` | |

**Agents-system disjointness proof (required, not optional):** this host
already runs a separate "agents" system (own bot, own launchd jobs, own
`claude -p` usage) that must remain untouched. Run:

```bash
launchctl list | grep -i amux      # should be empty before install
launchctl list                     # eyeball the agents-system's own labels
lsof -i :8822                      # should be empty before install
```

Write down the agents system's actual launchd labels and port(s), then state
explicitly in your preflight notes: *"agents system uses labels `<X>` / port(s)
`<Y>`; amux uses `com.amux.*` / port 8822; disjoint."* This sentence is the
Phase 3 AC — a table with no labels recorded doesn't satisfy it.

**Phase 3.b — evidence-based allowlist (blocking for Phase 6, do not skip):**
`deploy/mac-server/pull-reconcile-allowlist.txt` ships with two **seed
candidates only** (`.claude/commands/*.md`, `.claude/settings.json`, carried
over from dirt observed on mac-brain at plan time) — they are a starting
hypothesis, not verified evidence for mac-server. After Phase 4's checkout
exists and the server has run for at least one full cadence:

```bash
cd <checkout> && for i in $(seq 1 20); do git status --porcelain --untracked-files=no; sleep 15; done
```

Watch which *tracked* paths repeatedly show up dirty across that window,
confirm or replace the seed candidates with what you actually observed, and
edit `pull-reconcile-allowlist.txt` accordingly (one git pathspec per line;
blank lines and `#` comments are fine — `amux-pull-update.sh` strips them
before handing anything to git). **Do not bootstrap `com.amux.pull-update`
until this file reflects observed evidence** — see Phase 6.

---

## Phase 4 — Install core

From the checkout's `deploy/mac-server/` (either copied over or already
present if you cloned the repo by hand first):

```bash
./install.sh \
  --home "$HOME" \
  --checkout "$HOME/Desktop/Projects/amux" \
  --repo https://github.com/skalajan/amux.git \
  --branch main \
  --yes
```

Every value has a default (see `install.sh --help`); pass `--checkout` to put
it somewhere other than `~/Desktop/Projects/amux` (Open Question: this doc
assumes mirroring mac-brain's path — the `~/amux` alternative works identically,
just substitute it in every command below).

`install.sh`:
1. Checks prerequisites (tmux, git, python3>=3.10) and prints port-8822 /
   launchd-label info.
2. Clones the repo (or leaves an existing checkout's git state alone).
3. Seeds `~/.amux/server.env` and `~/.amux/defaults.env` from the `.example`
   files **only if they don't already exist** — never clobbers a live config.
4. Symlinks `/usr/local/bin/amux` -> `<checkout>/amux`. **This is the whole
   game — read the callout below before assuming a re-run "just works".**
5. Renders all four LaunchAgent plists from their `.tmpl` (placeholders
   `@@HOME@@` / `@@CHECKOUT@@` / `@@PYTHON@@` / `@@CADENCE_SECONDS@@`) into
   `~/Library/LaunchAgents/`.
6. Bootstraps + enables **only** `com.amux.serve`. The other three
   (`telegram`, `start-all`, `pull-update`) are rendered but never
   bootstrapped by this script — each has a real prerequisite Phase 4 alone
   can't satisfy (see Phases 5-6).
7. Prints the manual steps below.

### install.sh-copy-trap — symlink is REQUIRED, not a copy

`amux serve` resolves its own `script_dir` via `readlink -f "$0"` and execs
`python3 $script_dir/amux-server.py` — so if (and only if)
`/usr/local/bin/amux` is a **symlink into the checkout**, the running server
process *is* the checkout's `amux-server.py`, and a later `git pull` that
changes that file is a hot deploy (the server's own mtime watcher restarts
it). If something instead **copies** `amux` (and `amux-server.py`) into
`/usr/local/bin` — e.g. the repo's *other*, top-level `./install.sh`, which is
a different script for a different use case — the served file is a stale
snapshot and `git pull` on the checkout silently does nothing to production.
`deploy/mac-server/install.sh` always symlinks and warns loudly (refusing to
clobber an unknown existing file without `--force-symlink`) — if you ever see
`/usr/local/bin/amux` as a plain file on mac-server, something bypassed this
kit; fix it with the exact command `install.sh` prints in that case.

### Full Disk Access (manual — required under ANY execution mode)

**This cannot be scripted or done over SSH; the runbook halts here for a
human.** `com.amux.serve` (and the `start-all` exec path, which relies on the
server process to do the actual `exec`) needs Full Disk Access granted to the
python binary `install.sh` printed at the end of its run (typically
`/usr/local/bin/python3`):

1. System Settings -> Privacy & Security -> Full Disk Access.
2. Click **+**, navigate to that exact python binary, add it.
3. Toggle it on.
4. `launchctl kickstart -k gui/$(id -u)/com.amux.serve` (or just wait — it's
   already `RunAtLoad`/`KeepAlive`).

**AC:** `curl -sk https://localhost:8822/api/sessions` returns 200 over
localhost **and** over the Tailscale IP; the dashboard loads; `chat.js`/
`chat.css` are served (200).

---

## Phase 5 — Per-host identity

- **Claude config dirs / account login.** Set `CLAUDE_CONFIG_DIR` per session
  home as needed; also export `USER`/`LOGNAME` for launchd (a known Keychain
  quirk — launchd jobs otherwise can't resolve the right Keychain identity).
  Log in the personal account in whichever config dir(s) sessions on this host
  will use.
- **`⌁` summary marker.** Copy the verbatim block from
  [`reply-summary.md §1`](reply-summary.md#1-the-convention-main-model-side)
  into mac-server's `~/.claude/common.md` (and any other per-config-dir global
  instructions file) — this is what makes the dashboard chat tab and Telegram
  show a collapsed one-line summary instead of a wall of text.
- **Telegram — a SEPARATE bot from both mac-brain's sidecar and this host's
  own "agents" system bot.** Follow
  [`telegram-chat.md §1-5`](telegram-chat.md#1-create-the-bot-botfather) in
  full:
  1. New BotFather bot (privacy off).
  2. New **"Amux Server"** forum supergroup (Topics on), bot added as admin
     with *Manage Topics*.
  3. `~/.amux/telegram.env` (0600) from
     `deploy/mac-server/telegram.env.example` — `TG_BOT_TOKEN`, `TG_OWNER_ID`,
     `TG_CHAT_ID`.
  4. `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.amux.telegram.plist`
     then `launchctl enable gui/$(id -u)/com.amux.telegram`.
- **Verify the symlink again** (cheap, catches regressions):
  `readlink -f $(command -v amux)` must print a path inside the checkout.

**AC:** a `⌁`-marked reply from a test session renders as a collapsed
one-liner in the "Amux Server" group and in the dashboard chat tab; a Telegram
round-trip (send in the group -> session receives it -> reply forwards back)
works in that group.

---

## Phase 6 — Fleet + auto-update

### Fleet registration

Register 2-3 **personal** dev sessions on mac-server (proposal: from the
`seventyy*` / `home-servers` lane — confirm the exact names and that their
repos + SSH keys exist on THIS host before registering; a session whose repo
isn't cloned here will register but fail to start):

```bash
amux register <name> --dir ~/Dev/<repo> --yolo
amux start <name>
```

Duplicates of a session that also exists on mac-brain are **parallel
capacity, not migrated state** — board/scheduler/chat all live in this host's
own `~/.amux/amux.db`, never synced with brain (accepted debt, see the plan's
"Initial fleet definition"). Transcripts don't transfer either.

Then, for reboot wake:

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.amux.start-all.plist
```

**Never** register the SCHED-1 upstream-sync schedule or any work/Fidoo/
paymentapp session here (see "What NOT to run here" above).

### Auto-update — enable LAST, and only after Phase 3.b

This is deliberately the final step. `com.amux.pull-update` runs
`deploy/mac-server/amux-pull-update.sh` on a `StartInterval` cadence (default
300s / 5 min — tune via `install.sh --cadence <seconds>` before rendering, or
hand-edit the rendered plist's `StartInterval` and re-bootstrap). Each cadence:

1. **Pre-pull reconcile** — hard-resets every allowlisted pathspec to HEAD,
   one pattern per `git checkout HEAD --pathspec-from-file=…` call (never a
   shell-expanded glob — see the script's own header comment for why this is
   per-pattern, not a single whole-file call: an empirically-verified git
   quirk where one stale pattern would otherwise sink reconciliation of every
   other pattern too).
2. **Tripwire** — asserts the allowlisted paths are actually clean afterward;
   a failure here means the allowlist or the reconcile logic itself is broken,
   and aborts loudly rather than proceeding.
3. **Residual-dirt guard** — any tracked dirt **outside** the allowlist aborts
   the pull entirely (tree left untouched) and raises a loud local alert (a
   board task via the localhost API, best-effort, plus a best-effort Telegram
   note via this host's own `telegram.env` credentials if configured) —
   **never force-resolved**.
4. `git fetch origin` + `git pull --ff-only`.
5. Per-changed-file service bounce: `amux-telegram.py` changed ->
   `launchctl kickstart -k gui/$(id -u)/com.amux.telegram`; `amux-server.py`
   changed -> nothing (the server's own `_watch_self` mtime watcher handles
   its own restart); anything else (`chat.*`, docs, the `amux` CLI, this very
   script) -> nothing needed.

**Do NOT bootstrap this before Phase 3.b's evidence-based allowlist is in
place** — `install.sh` never does this step itself, by design, under any flag.
Once the allowlist reflects observed `git status` evidence:

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.amux.pull-update.plist
launchctl enable    gui/$(id -u)/com.amux.pull-update
```

Sandbox-test the reconcile/tripwire/residual-dirt/never-pushes logic anytime
without touching this host's real checkout or git state:
`deploy/mac-server/test-pull-update.sh` (self-contained; builds a throwaway
origin+clone pair under `mktemp -d` and cleans up after itself).

**Footnote — `chat.js`/`chat.css` need a hard refresh after they propagate.**
These are served fresh per request (no restart needed), but the browser
heuristically caches them — there's no cache-busting query param or
cache-control header on that static route. A soft reload can serve the stale
asset even though the file on disk is current. Hard-refresh (Cmd+Shift+R, or
clear-cache-and-reload) any already-open dashboard tab after a `chat.*`
change lands. This is a manual step, not a bug to fix here.

---

## Verification (per milestone)

| Milestone | Acceptance check |
|---|---|
| Preflight (P3) | Filled table above; explicit agents-vs-amux disjointness sentence recorded. |
| Core install (P4) | `curl -sk https://localhost:8822/api/sessions` -> 200 on localhost **and** Tailscale IP; dashboard + `chat.*` load. |
| Identity (P5) | Telegram round-trip works in "Amux Server"; `⌁` marker collapses correctly in both dashboard and Telegram; `readlink -f $(command -v amux)` resolves into the checkout. |
| Fleet + auto-update (P6) | AC-1 through AC-4 below. |

**AC-1 (end-to-end propagation, clean tree).** On mac-brain, commit a trivial
`docs/` change and push to origin. Within one cadence, mac-server's checkout
shows the new commit (`git log`) with **no** service bounce. Then push a
trivial `amux-telegram.py` change — verify the sidecar gets `kickstart`ed and
reconnects. Then a no-op `amux-server.py` change (or wait for a real one) —
verify the server `os.execv`-restarts (after `_watch_self`'s ~30s debounce)
and serves 200 again.

**AC-2 (propagation SURVIVES routine dirt — the staged-deletion case).** On
mac-server, produce **both** dirt variants on **allowlisted** paths: an
unstaged edit (e.g. `.claude/commands/amux.md`) and a **staged deletion**
(`git rm --cached .claude/settings.json` — the exact class of dirt that
motivated Decision D1). On mac-brain, push a commit touching those same
paths. Within one cadence: the pre-pull reconcile hard-resets both (index +
worktree), `pull --ff-only` lands, brain's version is present, and **no alert
fires**. The script's own tripwire (an internal regression guard, not a
separate manual check) asserts this same thing on every run.

**AC-3 (outside-allowlist loud failure — separate from AC-2).** On
mac-server, dirty a tracked path that is **not** in the allowlist. The
residual-dirt guard aborts the pull, the tree is left untouched, and a board
task (+ optional Telegram note) appears. Clean the path — the next cadence
recovers and lands whatever was pending.

**AC-4 (agents system untouched).** `launchctl list` unchanged for the
agents-system's own labels; its bot and port are never touched by anything in
this deploy.

---

## Rollback

- **Core install / bad plist:** `launchctl bootout gui/$(id -u)/com.amux.serve`
  (or whichever label), fix the rendered plist or re-run `install.sh`, re-bootstrap.
- **Bad pull (shouldn't happen — `--ff-only` never partially applies, and a
  syntax-broken `amux-server.py` is rejected by `_watch_self` before restart):**
  `git -C <checkout> log` to find the last-good commit, `git -C <checkout>
  reset --hard <sha>` by hand if truly needed (the pull-update script itself
  never does this — it only ever fast-forwards or aborts).
- **Allowlist gone stale and blocking every cadence with a false-positive
  residual-dirt abort:** rename it aside
  (`mv pull-reconcile-allowlist.txt pull-reconcile-allowlist.txt.bak`), which
  degrades to "any tracked dirt aborts the pull" — safe, just less convenient
  — then redo the Phase 3.b observation window and restore a corrected file.
- **Remove auto-update entirely:**
  `launchctl bootout gui/$(id -u)/com.amux.pull-update`.
- **Remove the whole install:** bootout all four labels, `rm
  /usr/local/bin/amux` (the symlink only — never touches the checkout), remove
  the four plists from `~/Library/LaunchAgents/`, remove `~/.amux/` if you
  want the host's state gone too.

---

## Open questions (execution-time decisions)

1. **Execution mode (Decision 3):** SSH-scripted from mac-brain, or a human
   at mac-server's own keyboard? Either works unmodified — `install.sh` is
   non-interactive-capable either way. FDA (Phase 4) is manual under both.
2. **Checkout path:** this doc assumes `~/Desktop/Projects/amux` (mirrors
   mac-brain); `~/amux` works identically if preferred — just substitute it
   consistently.
3. **Exact fleet names:** confirm the 2-3 personal session names and that
   their repos/SSH keys actually exist on mac-server before Phase 6.
4. **Cadence:** default 300s (5 min); tune with `install.sh --cadence <n>`.
5. **Failure-surfacing channel:** both a board task and a best-effort
   Telegram note are wired by default (`amux-pull-update.sh` always tries
   both); drop one by not configuring its credentials if you'd rather have
   just the other.
6. **`start-all` on reboot:** wired by default in Phase 6; skip installing
   that one plist if sessions should only ever start on demand.

---

## Cross-references

- [`telegram-chat.md`](telegram-chat.md) — bot setup, forum topics, commands, notification rules.
- [`reply-summary.md`](reply-summary.md) — the `⌁` marker convention and its degradation chain.
- [`upstream-sync.md`](upstream-sync.md) — the (mac-brain-only) weekly upstream merge SOP; not run on mac-server.
- `.omc/plans/mac-server-deploy.md` — full design record: decision drivers, rejected alternatives, the D1 divergence-handling analysis.
