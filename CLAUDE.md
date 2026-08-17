# amux

This fork is migrating from a single-file Python server (`amux-server.py`, Python + inline
HTML/CSS/JS dashboard) onto upstream's Rust workspace — see
[`.omc/plans/rust-migration.md`](.omc/plans/rust-migration.md) for the phased plan and
current status. As of this writing (2026-08-17), `amux-server.py` is still what runs in
production (port 8822) and cutover has not landed, but it is deprecated: dead upstream
since `792ce1f4`, and not where new work goes. See
[`.claude/rules/single-file.md`](.claude/rules/single-file.md) and
[`.claude/rules/extend-via-sidecar.md`](.claude/rules/extend-via-sidecar.md) for what that
means for where fork-local code belongs — new capability goes to a sidecar, not into that
file, and never into the Rust workspace this fork doesn't own.

## The ethos — read before building anything

**The harness gets better as the models get better. Get out of the model's way.**

Every new feature or enhancement gets gut-checked against
[`.claude/rules/ethos.md`](.claude/rules/ethos.md) before you build it and again before
you call it done. Eight questions, each one there because it was violated in this repo
and cost something real: capability that never reached a session, model calls spent on
string manipulation, gates that could not be satisfied honestly, instruments that could
not express the discriminator, automation that accumulated instead of deciding, an audit
trail that was claimed but not implemented, checks that could not fail, and an agent
deciding something that was the user's to decide.

The question underneath all of them: when the next model is meaningfully better than
this one, does this feature get better with it, or does it become the ceiling?

The bottom of that file is a **Known deviations** ledger — live places where amux
still fights the ethos, each with a status and an exit condition. Check it before
touching state detection, auto-answers, helper-model calls, observation caps, or
auto-compact; move the deviation toward its exit rather than deepening it.

## You are dogfooding — fix it at the root

You run *inside* amux. Every rough edge you hit while working is a rough edge a user
hits, and you are the one person positioned to see it from the inside. So when
something in amux gets in your way — a peek that hides output, a board mutation that
bounces silently, a browser profile that saves to one place and loads from another, an
error message that sends you chasing the wrong cause — **treat it as a product defect
and fix it at its root**, not as an obstacle to route around in your own task.

- Do not paper over it with a workaround in your script, a manual step, or a note to
  yourself. If you needed the workaround, so will a user who has no idea it exists.
- Fix the source, not the symptom: the generator rather than the generated file, the
  API rather than the one caller, the instrument that could not express the failure
  rather than the one conclusion it misled you into.
- Instrumentation counts as product. If a failure was hard to diagnose because output
  was truncated, logs went to `/dev/null`, or a check could not fail, that is itself the
  bug worth fixing — the next person debugging it is a user.
- Then leave the comment explaining *why*, so the fix does not get undone by someone who
  only sees the shape of the code.

The bar: after you are done, could someone hit the same problem again? If yes, you fixed
your task, not the platform.

## Structure

- `amux-server.py` — the current production server + dashboard (single file); deprecated,
  not where new work goes (see `.claude/rules/single-file.md`).
- `amux` — the bash CLI; survives the migration as a legacy client (`MODIFICATIONS.md`).
- Fork-local sidecars (`amux-telegram.py`, `amux-chat.py`, `amux-drain-restart.py`, ...) —
  talk to whichever server is live over its HTTP/SSE API. New fork-local functionality goes
  here, per `.claude/rules/extend-via-sidecar.md`.
- `mcp.json` — centralized MCP server config (shared by local and cloud)
- `cloud/` — GCP VM provisioning (Terraform + setup script)

## Workflow

- **Commit after every completed task.** When you finish a piece of work (bug fix, feature, refactor), immediately `git add <changed files> && git commit` with a concise message. Don't batch multiple tasks into one commit.
- Pre-cutover, `amux-server.py` auto-restarts on file save (watches its own mtime) — true today, but not a reason to build new capability there; see `.claude/rules/single-file.md`.
- Verify syntax after edits, by file type:
  - Python (`amux-server.py`, sidecars): `python3 -c "import ast; ast.parse(open('<file>').read())"`
  - `amux` (bash CLI): `bash -n amux`
  - Anything under `crates/`: it shouldn't exist as a fork-local edit — see `.claude/rules/extend-via-sidecar.md` before writing there.

## Deploy

When the user says **"deploy"**, run the full pipeline:
1. `git add` changed files (typically sidecars, docs, or rules — see `.claude/rules/single-file.md` before adding new work to `amux-server.py`)
2. `git commit` with a concise message
3. `git push origin main`

## Single-codebase rule (CRITICAL)

This rule used to gate `amux-server.py` directly, since this fork owned and edited that
file end to end. Post-migration this fork doesn't edit `crates/` at all (see
`.claude/rules/single-file.md` / `.claude/rules/extend-via-sidecar.md`), so the concern
moves from "don't fork `amux-server.py`'s behavior by build target" to "don't fork
*sidecar* behavior by build target" — the underlying rule is unchanged, just aimed at
different files now:

- Never add cloud-only or OSS-only code branches (no `if IS_CLOUD`, no
  `if os.environ.get('CLOUD')`) in any sidecar or fork-local script.
- Features that differ between environments must be driven by headers/env vars injected by
  the gateway (e.g., `X-Amux-User-Email`) or by presence/absence of configuration, not by
  build-time flags.
- Whatever replaces `cloud/docker/amux-server.py` for a compiled binary (mac-server's
  deploy model is still undecided — `.omc/plans/rust-migration.md` phase P5) must never be
  committed — generate it at deploy time, same as before, and keep it in `.gitignore`.

## Server config — `~/.amux/server.env`

Persistent env vars for the server. Loaded at startup via `os.environ.setdefault` so process-level env always wins. Survives `os.execv` auto-restarts.

Example `~/.amux/server.env`:
```
AMUX_S3_BUCKET=ethan-personal
AMUX_S3_KEY=amux/cal-<random-token>.ics
AMUX_S3_REGION=us-east-2
```

After creating/editing server.env, `touch amux-server.py` to trigger a reload.

## iCal / Google Calendar sync

The iCal feed exports amux **calendar events** only (`cal_events` table) — NOT
schedules or board issues. Those are in-app-only calendar layers. Timed events are
emitted as UTC (`DTSTART:...Z`) so Google shows the correct local time; all-day use
`VALUE=DATE`.
- Local: `GET /api/calendar.ics`
- Public S3 (for Google/Apple Calendar subscriptions): set `AMUX_S3_BUCKET` in `server.env`

S3 bucket config (on `ethan-personal`):
- Public access block: `BlockPublicAcls=true, IgnorePublicAcls=true, BlockPublicPolicy=false, RestrictPublicBuckets=false`
- Bucket policy grants public `s3:GetObject` on `arn:aws:s3:::ethan-personal/amux/*.ics` (widened from the single `calendar.ics` key so cache-busting keys work). Bucket LISTING is denied (403), so a random key is not discoverable.
- Current key: a **random token** (`amux/cal-<32hex>.ics`) that lives ONLY in `~/.amux/server.env` — **NEVER commit the actual key/URL to this repo: the repo is public, and a committed feed URL is how the old guessable key leaked.** Read it with `grep AMUX_S3_KEY ~/.amux/server.env`; the dashboard's Subscribe button shows the full URL.

**Google caches ICS feeds by URL, hard.** There is no reliable way to force a refresh — Google refetches on its own cadence (hours). If you edit the feed's *content shape* and need Google to see it now, publish to a NEW random key and re-subscribe; the old URL keeps serving Google's stale cache. `AMUX_S3_KEY` is read at startup via `os.environ.setdefault`, and execv reloads inherit the env, so changing it needs a real restart: `launchctl kickstart -k gui/$(id -u)/com.amux.server`.

The feed auto-uploads to S3 on every event write. The dashboard's calendar subscription button shows the S3 URL directly when configured.

## Browser Automation

Use `/chrome-cdp` for browser tasks. It connects directly to the user's live Chrome via CDP — real tabs, real cookies, no fresh browser.

```bash
node skills/chrome-cdp/scripts/cdp.mjs list           # list open tabs
node skills/chrome-cdp/scripts/cdp.mjs snap <target>   # accessibility tree
node skills/chrome-cdp/scripts/cdp.mjs shot <target>   # screenshot
node skills/chrome-cdp/scripts/cdp.mjs click <target> <selector>
node skills/chrome-cdp/scripts/cdp.mjs type <target> <text>
node skills/chrome-cdp/scripts/cdp.mjs eval <target> <js>
node skills/chrome-cdp/scripts/cdp.mjs nav <target> <url>
```

Requires Chrome remote debugging enabled (`chrome://inspect/#remote-debugging`) and Node.js 22+.

Claude Code, the amux server, and Chrome all run on the same desktop machine. Use
`$AMUX_URL` for amux dashboard URLs — defaults to `https://localhost:8822` today; moves to
`:8824` after the Rust cutover (see `.omc/plans/rust-migration.md` phase P3b, "8822 is
GONE").
