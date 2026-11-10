# Fork sync: upstream → origin, with the egress gate

Run by the weekly schedule "Sync fork with upstream" (worker `amux-helper` on brain). Repo: `/Users/janskala/Projects/personal/amux`, remotes `origin` = skalajan/amux, `upstream` = mixpeek/amux.

The owner's rule, and the reason this gate exists: **nothing leaves the machines unless the owner configured it, and nothing opens a way in.** Any doubt stops the merge.

## Steps

1. **Precheck.** `git status --short --branch` shows `main...origin/main` with no changes, and `git fetch origin upstream` succeeds. Otherwise stop and report what is in the way.
   - Why: a merge on a dirty or diverged tree mixes someone's work into the sync commit.
2. **Anything new?** If `git rev-list --count HEAD..upstream/main` is 0, report "nothing to sync" and stop.
3. **Static audit.** Run `fork/sync-audit.sh` and read every hit.
   - Why: it lists only what upstream changed, in runtime code. Tests, docs, the site and comments are filtered out.
   - A hit is harmless only when it is plainly test data, a string or a log message.
   - **Do not merge, and report each one with file:line**, if a hit:
     - adds a request to a new host;
     - turns on an existing outbound feature by default;
     - adds telemetry, analytics or reporting;
     - adds a listener, a public route or a relay or tunnel;
     - downloads or runs remote code.
   - **A NEW dependency:** check what it is, for example `cargo tree -i <crate>` and its description. An HTTP client, a telemetry SDK or a networking crate counts as a hit.
4. **Watched files.** Also read upstream's diff (`git diff HEAD...upstream/main -- <path>`) of every file in "Watched surfaces" below.
   - Why: these files already hold egress or exposure code, so a one-word default flip there never matches a pattern.
5. **Merge** with `git merge --no-ff --no-commit upstream/main`.
   - Fork deltas that must survive:
     - `amux contribute` is gone: `grep -c 'cmd_contribute' amux` = 0;
     - `amux here` and `amux attach` with no name are present;
     - this `fork/` folder is unchanged;
     - the exposure controls are wired: `crates/amux-server/src/api/fork_lockdown.rs` (listen address `AMUX_RS_BIND`, owner token on the diagnostics upstream serves publicly) and `crates/amux-server/src/api/fork_cdn.rs` (CDN libraries served from the binary). Check `grep -c 'fork_lockdown\|fork_cdn' crates/amux-server/src/api/mod.rs` = 6 and `grep -c fork_lockdown crates/amux-server/src/lib.rs` = 1.
   - Conflict in `crates/amux-dashboard/static/state/control-registry.json` only: run `git checkout --theirs` on it, then `npm run -s build:state`.
   - Any other conflict: `git merge --abort` and report.
6. **Verify:**
   - `fork/cdn-vendor.py --check` — every CDN file the dashboard names is vendored. If not, run `fork/cdn-vendor.py` (it downloads only the new URLs and pins them in `crates/amux-server/fork-cdn/lock.tsv`) and commit the files with the merge. A URL it cannot vendor (a new host or prefix) is a blocker for the owner.
   - `cargo test -p amux-server --lib fork_`
   - `node scripts/build-state.mjs --check`
   - `python3 tests/cli_help.py`
   - `bash scripts/test-cli-sourceable.sh`
7. **Commit** with the one-line message `chore: merge upstream/main` and no body or trailers.
   - If the hook refuses mass deletions, check that each deleted file is gone in `upstream/main` too. Only then repeat the commit with `AMUX_ALLOW_MASS_DELETE=1`.
   - Push: `git push origin main`. The hooks run cargo check and the tests, which takes minutes.
8. **Deploy.** Run `ssh mac-server 'cd ~/Projects/personal/amux && git pull --ff-only'`.
   - Wait until `~/.amux/logs/rust-auto-build.log` shows `ACTIVATION HEALTHY sha=<merge sha>` on both brain and mac-server, followed by "signed as 'amux-dev'".
9. **Live egress check** on both hosts:
   - brain: `fork/egress-check.sh 120`
   - mac-server: `ssh mac-server 'cd ~/Projects/personal/amux && fork/egress-check.sh 120'`
   - **Any VIOLATION:** report it to the owner first, before anything else.
   - Never extend `fork/egress-allowlist` yourself. Only the owner adds entries.
10. **Report** in at most 5 lines: commits merged, audit verdict (hits judged harmless or a blocker), deploy result and egress verdict on both hosts.

## Watched surfaces

- `crates/amux-server/src/lib.rs` — listeners and bind addresses.
- `crates/amux-server/src/api/auth.rs`, `crates/amux-server/src/api/mod.rs` — public paths, and routes outside `require_bearer`.
- `crates/amux-server/src/runtime_jobs/tunnel.rs` — relay to cloud.amux.io.
- `crates/amux-server/src/runtime_jobs/registry.rs` — jobs runnable through `/api/system-jobs/{id}/run`.
- `crates/amux-server/src/api/static_files/` — apps origin, per-app ports, public links, the PostHog snippet.
- `crates/amux-server/src/push/`, `crates/amux-server/src/telegram_poll.rs`, `crates/amux-server/src/bridge/` — outbound messaging.
- `crates/amux-server/src/provider/live_catalog.rs`, `crates/amux-server/src/api/location.rs` — background fetches.
- `crates/amux-dashboard/static/` — any new third-party host the browser would load from. Only cdn.jsdelivr.net, unpkg.com, cdnjs.cloudflare.com and esm.sh are rewritten to the vendored copies; the page CSP refuses scripts from anywhere else.
- `scripts/rust-auto-build-authority.sh` — CI gate against api.github.com (`AMUX_DEPLOY_CI_GATE=0` must stay honoured).
- `desktop/amux-update.swift` — updater pointing at mixpeek/amux.
- `amux` — any `curl`, `gh` or `https://` added to the CLI.

## Settings that keep egress off (`~/.amux/server.env`, both hosts)

These settings must stay in place. A sync that renames or ignores any of them is a blocker.

- `AMUX_DEPLOY_CI_GATE=0`
- `AMUX_MODEL_CATALOG_REFRESH_SECS=0`
- `AMUX_RS_NO_LOOPBACK_BYPASS=1`
- `AMUX_RS_BIND=127.0.0.1` (the dashboard is reached through Tailscale Serve on each host) and `AMUX_APPS_PORT=0`
- no `POSTHOG_KEY`, no `AMUX_TUNNEL_TOKEN`, no `AMUX_MIXPEEK_OPS_URL`

The legacy Python server (`com.amux.serve`) and `com.amux.start-all` were disabled on 2026-11-10 because the Python server forwarded unauthenticated LAN requests to any local port. They must not come back.
