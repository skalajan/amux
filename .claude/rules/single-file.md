---
description: amux-server.py is being retired — no fork-owned application file to split anymore
globs: ["amux-server.py", "crates/**"]
---

This rule used to say: `amux-server.py` is one file containing the Python server plus
inline HTML/CSS/JS, don't split it into modules. That premise is going away. Upstream
(`mixpeek/amux`) deleted its own `amux-server.py` (78,565 lines) at commit `792ce1f4` on
2026-08-09 — swept in accidentally under an unrelated CSS-fix commit message — and has not
touched the Python line since. Upstream is now a Rust workspace, 183 `.rs` files under
`crates/` (`amux-core`, `amux-server`, `amux-cli`, `amux-dashboard`), serving on port 8824
only (`crates/amux-server/src/lib.rs:533`: "THE LEGACY 8822 BIND IS GONE"). This fork is
migrating onto it — see `.omc/plans/rust-migration.md` for the phased plan and status.

**As of this writing (2026-08-17), `amux-server.py` still exists in this repo and still
runs in production on mac-brain (port 8822); cutover is plan phase P4 and has not landed.
After cutover, it stays in the repo — deliberately.** It is dead upstream (nothing has
touched it since `792ce1f4`) and it stops being the live server, but this fork keeps it as
a frozen reference implementation: it is the rollback path, and it is the only surviving
Python oracle the recovered parity harness (`e2e/parity-tasks.mjs`) can ever run against —
upstream deleted their own copy in the same commit that killed the file, so they can never
regenerate one. Upstream's own cutover runbook said to keep the Python server 30 days as
the reference implementation and then didn't, by accident; this fork is doing what they
intended. Treat it as frozen, not as a place to build:

- **Do not add new capability to `amux-server.py`, and don't add it directly into the Rust
  workspace either.** New fork-local functionality goes to a sidecar or another external
  file per [`extend-via-sidecar.md`](extend-via-sidecar.md). For anything touching
  `crates/`, the old tier-3 fallback ("in-file change, only when 1–2 genuinely can't work")
  is gone — see that file for why. The `amux` bash CLI is the one holdover: it survives
  upstream as a legacy client, is still a single tracked script, and still takes grafted
  in-file deltas via the sentinel+registry convention.
- **Do not recreate a "single file" anywhere in the new architecture.** The Rust workspace
  is inherently multi-file; there's no equivalent constraint worth preserving, and no
  reason to try to collapse fork-local code back down to one file the way `amux-server.py`
  was.

## The session chat tab: no longer a documented exception

This file used to carry a whole section on the session chat tab as a deliberate, approved
*exception* to the single-file rule — `chat.js`/`chat.css` lived at repo root, with a small
sentinel-wrapped footprint (`# AMUX-LOCAL:session-chat`) grafted inline into
`amux-server.py`, because the feature needed to be first-class dashboard UI and a pure
sidecar/iframe couldn't deliver that.

Per the migration plan (phases P3 and P8), that dashboard tab is being **deleted, not
ported** — it was never the front-end Jan actually uses; Telegram is. The capture core
underneath it (turn extraction, reply population) is being extracted into a standalone
sidecar, `amux-chat.py`, so Telegram delivery keeps working across the cutover — see
[`extend-via-sidecar.md`](extend-via-sidecar.md). Once that lands, there is no more "the
app is one file, except for this one thing" case to document, because there is no longer
an app file to be an exception to. `docs/session-chat.md` records the original design
history; it is not being kept current against the new architecture.
