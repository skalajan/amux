# OpenCode Provider Spike Results (RR-0028e)

## Summary

3 of 4 providers expose structured lifecycle events sufficient for
`OpenCodeAdapter` to serve as the primary agent protocol. The written
branch does NOT fire. `TerminalAdapter` remains the fallback path, not
a peer of `OpenCodeAdapter`. No Phase 1+4 re-estimate required
(RR-0028l condition not met).

## Provider-by-Provider Findings

### Claude Code (v2.1.226) -- COVERED

Structured output via `--output-format stream-json` provides typed
lifecycle events. `--include-hook-events` adds Stop/UserPromptSubmit/
PreToolUse/PostToolUse hook events to the stream. `--include-partial-messages`
adds streaming progress chunks.

Lifecycle coverage:
- Session start: `--print -p` or `--input-format stream-json`
- Turn boundaries: stream-json events (turn start/end)
- Tool use: PostToolUse hook events in stream
- Rate limit: `rate_limit_event` with `rate_limit_info.{status,resetsAt,rateLimitType}` (live evidence: events.rs fixtures, RR-0030). Interactive rate-limit menu remains terminal-only.
- Graceful shutdown: SIGINT, `--max-turns`

### Gemini CLI (v0.53.1) -- COVERED

`--output-format stream-json` provides structured output but with a FLAT
event shape (`init`, `message`, `tool_use`, `tool_result`, `result` with
`delta: true` chunks) — does NOT mirror Claude Code's nested shape
(live evidence: events.rs fixtures, RR-0030). Gemini CLI also has a hooks
system (`gemini hooks`).

Lifecycle coverage:
- Session start: `-p` prompt mode
- Turn boundaries: stream-json events
- Progress/tool use: partial coverage via stream-json
- Rate limit: NOT exposed structurally (terminal scrape only)
- Graceful shutdown: SIGINT

### Codex CLI (v0.141.0) -- COVERED

`codex exec --json` prints events to stdout as JSONL, providing typed
lifecycle events. Also supports hooks (with `--dangerously-bypass-hook-trust`
for automation).

Lifecycle coverage:
- Session start: `exec` subcommand
- Turn boundaries: JSONL events
- Tool use: JSONL tool events
- Rate limit: `error` + `turn.failed` JSONL events for usage limits (live evidence: captured against exhausted quota, events.rs fixtures, RR-0030). Limit-shaped messages become `RateLimited`.
- Graceful shutdown: SIGINT

### Ollama (v0.20.5) -- NOT COVERED

Ollama is a raw LLM model server, not a coding agent CLI. It exposes a
REST API (`/api/chat`, `/api/generate`) with streaming JSON responses,
but provides none of the agent-level features:

- No file editing or tool use
- No hooks system
- No structured lifecycle events beyond HTTP stream chunks
- No session/turn concept
- Rate limiting exposed only as HTTP 429

Ollama serves as a model BACKEND for other agent CLIs (e.g., Codex's
`--oss --local-provider ollama`), not as a standalone agent. When used
through another CLI, that CLI's structured events apply.

## Written Branch Decision

Condition: `OpenCode coverage < 3 of 4 providers for core lifecycle events`

Result: **3 of 4 covered** (Claude Code, Gemini CLI, Codex CLI).
Ollama is not an agent CLI and is not expected to provide agent lifecycle events.

Decision: **Written branch does NOT fire.** `TerminalAdapter` remains the
fallback for rate-limit detection and providers without structured output.
`OpenCodeAdapter` is the primary protocol path.

No Phase 1+4 re-estimate required (RR-0028l).

## Remaining Gap: Rate Limits

**Updated by RR-0030 live evidence** (crates/amux-server/src/opencode/events.rs):

Claude Code and Codex CLI both expose rate limits structurally — the spike
incorrectly recorded these as terminal-scrape-only:
- Claude Code: `rate_limit_event` with `rate_limit_info.{status,resetsAt,rateLimitType}` in stream-json. The interactive rate-limit MENU remains terminal-only.
- Codex CLI: `error` + `turn.failed` JSONL events with limit-shaped messages (captured live against exhausted quota).
- Gemini CLI: still NOT exposed structurally (terminal scrape only).

The terminal adapter's rate-limit detection remains load-bearing for Gemini
and for Claude Code's interactive menu. The D1 exit (report endpoint replaces
scrapers) is further along than the spike originally recorded: 2 of 3
providers now have a structured rate-limit path.

## Conversation Continuity (AMUX-2613, measured 2026-08-09)

Headless continuation per provider, measured live on this machine
(claude 2.1.226, gemini 0.54.4, codex 0.141.0). Implemented in
`crates/amux-server/src/opencode/structured.rs`; refs persist in
`_amux_conversations` (migration 0011) and hydrate at registration.

| Provider | Conversation id source | Resume argv | Live verification |
|---|---|---|---|
| Claude Code | `system/init` `session_id` | `--print --resume <id>` | VERIFIED end-to-end: same session id continues (no fork); memory confirmed (pomegranate probe, haiku) |
| Gemini CLI | `init` `session_id` | `-p --resume <id>` | VERIFIED end-to-end; memory confirmed. Top-level help documents only index/"latest", but UUID works and the bad-id error text names `--resume {uuid}` as supported |
| Codex CLI | `thread.started` `thread_id` | `exec resume <id> <prompt>` (with `--json --skip-git-repo-check`) | PARTIAL: argv accepted, `thread.started` re-emits the SAME thread id, turn starts — then fails on this machine's exhausted quota (resets 2026-08-27). Memory continuity rests on the CLI's documented resume contract until then |
| Ollama | n/a | n/a | Not an agent CLI (above); no session concept to resume |

Dead-target shapes (why refs are captured ONLY from init-shaped lines):

- claude `--resume <vanished id>`: exit 1, a single error `result` line that
  ECHOES the dead session id, no init. Capturing session_id from arbitrary
  lines would re-arm the dead ref on every failure.
- gemini `--resume <vanished id>`: exit 42, empty stream, stderr explains.

The protocol drops a ref whose resumed run failed without re-emitting init,
so a vanished conversation degrades to one failed turn + a fresh start, not
a permanently wedged worker.

## herdr Agent Detection

herdr (v0.8.0) already has agent-detection profiles for claude, codex, and
gemini in `~/.local/state/herdr/agent-detection/remote/`. These detect
terminal UI patterns (working/blocked/permission states) via regex rules.
The detection profiles confirm the terminal adapter's role as the rate-limit
and TUI-state fallback.
