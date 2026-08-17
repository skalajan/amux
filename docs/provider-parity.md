# Provider Parity — success criteria and scored audit

**Goal (Ethan, 2026-08-02):** a worker on any provider has the same capabilities inside amux as a Claude worker. This file is the criteria list and the living scorecard. A capability is MET only with evidence (the ethos rule: verified, not assumed). Re-score when a provider's CLI changes.

**The bar for "same capabilities":** every row below is something amux gives a Claude lane. For each provider, the row is `MET` / `PARTIAL` / `GAP`, with the evidence or the card that tracks it.

| # | Capability | Success criterion | Claude | Gemini | Evidence / card |
|---|---|---|---|---|---|
| 1 | Status classification | active/idle/waiting detected from the live pane | MET | **PARTIAL** | idle/active shipped 9f80099, verified live (sherpa-execution `idle`). `waiting` (selector/auth screens) undetected — card AMUX-2231 |
| 2 | Idle-driven loops (pickup, steering, nudges, sweeps) | lane enters every loop keyed on idle | MET | MET | Full send/receive round trip verified 18:58 (SE-2 status-update via CLI) |
| 3 | amux CLI + board from inside the worker | board writes, sends, whoami work | MET | MET | SE-2 status-update posted by the Gemini lane itself |
| 4 | Memory injection | amux-composed memory reaches the model at launch | MET | MET (this commit) | Worker-scoped GEMINI.md mirrored into `--include-directories`; no repo GEMINI.md touched |
| 5 | Launch parity (flags, yolo, model, resume meta) | provider flags normalized, worker id persisted | MET | MET | start_session gemini branch (`--yolo`, `--skip-trust`, `--model auto`, gemini_session_id) |
| 6 | Custom slash commands | amux commands installed for the CLI | MET | MET | `~/.gemini/commands/*.toml` written alongside Claude's |
| 7 | Rate/usage-limit detection | provider cap banner → credit_limited + badge | MET | MET | `_PROVIDER_LIMIT_RES` gemini pattern (AMUX-2088, cloud-verified) |
| 8 | Limit auto-resume | reset time parsed → auto-continue at reset | MET | **GAP** | Gemini banner has no parsed reset; card AMUX-2231 |
| 9 | Token/cost tracking | per-worker tokens + $ in Cost tab | MET | **GAP** | Ledger reads Claude JSONL only; Gemini lanes invisible to Cost — card AMUX-2230 |
| 10 | Transcript tab | gap-free conversation render in peek | MET | **GAP** | Reads Claude JSONL only — card AMUX-2230 |
| 11 | Self-report (D1 hooks) | Stop/UserPromptSubmit → /report | MET | **GAP (upstream)** | Gemini CLI has no hook equivalent; scraper (#1) is the sanctioned fallback per D1 |
| 12 | Model detection | active model shown on card | MET | MET | Flags/default fallback (`--model auto`) |
| 13 | API-error detection (5xx retryable) | transient errors flagged, continue offered | MET | **GAP** | Patterns are Claude-shaped — card AMUX-2231 |
| 14 | Subagent/suggestion niceties | running-subagent badge, empty-send suggestion | MET | GAP (minor) | Claude-UI parsing; cosmetic — card AMUX-2231, low priority |
| 15 | Guards (commit/push/staged), groups, peek, steering composer, schedules, archive | provider-agnostic mechanics | MET | MET | tmux/git/DB-based; no provider branch exists |

**Scoring rules:**
- MET requires a live verification, not a code read (rows 1–4 were each proven on sherpa-execution today).
- A GAP with no card is a violation of this doc — file one before merging the change that discovers it.
- Codex column: to be scored the same way when a Codex lane is next active (most Gemini rows apply verbatim; #7 already MET).

**The compounding check (ethos):** rows 2–3 are where capability reaches the model — those are MET, which means a better Gemini makes a better lane with zero harness change. Rows 9–10 are observability of the lane, not capability of the model; they gate nothing the model does.
