# Daily log sweep — the contract (AMUX-2605)

This is a contract for a MODEL, not automation: the sweep is an amux **scheduler**
entry that prompts a session to run these queries, judge the results, and file
board cards. Substrate: `_amux_request_log` (migration 0010), written by
`api/request_log.rs` for every `/api/*` request (exclusions: `/api/events`,
`/api/debug/*`, non-API static paths). `worker` is path-derived
(`/api/sessions/{name}/*`, `/api/workers/{id}/*`), so worker logs are a filter,
never a second log. Retention: `AMUX_REQLOG_RETAIN_DAYS` (default 14).

All queries are GET against the rust origin. `SINCE=$(( $(date +%s) - 86400 ))`.
`total_matched` is the pre-limit count — use it for volumes; never infer volume
from a capped `events` page (limit max 2000).

**COVERAGE (changed 2026-08-09, AF-36 — read this before trusting an all-clear.)**
The table now carries BOTH origins. It used to hold only rust-served requests, and
on 2026-08-09 that was 1,494 rows against python's 129,940 in the same window —
1.1% of traffic — so the sweep reported "0 5xx, 0 auth failures, no latency
outliers" while 52 x 400, 3 x 401, 5 x 403 and a 3.3s board GET sat unseen on the
other origin. Every sweep below was correctly specified and structurally blind.
Discriminate with `answered_by`: `native` = rust, `python` = python origin,
`python-proxy` = proxied through. If a sweep ever returns zero 400s across a whole
day again, check `SELECT answered_by, COUNT(*)` before believing it — a
single-origin result is the tell that coverage regressed.

**Steps 1 and 2 are single calls to deterministic endpoints (AMUX-2610).** They
exist because the old shape of these steps — pull 2000 raw rows, group them,
compute percentiles, then grep mod.rs and read handlers to explain a 404/405 —
was a model burning tokens on pure computation (a real 405 diagnosis cost a
grep-the-router session; Ethan: "we need deterministic tools in the api").
Ethos rule 2: spend the model on JUDGING the numbers, never on producing them.
The raw `/api/logs` queries in steps 3-5 and below remain the deep-dive
fallback when a finding needs row-level inspection.

## The six sweeps, in order

1. **Errors: one call.** `GET /api/logs/analyze?since_h=24`
   Pre-grouped error rows (status >= 400) by (status, method, family,
   normalized target — ids collapsed, `/api/board/AMUX-123` ->
   `/api/board/{id}`), each with count / first / last / distinct_clients and
   one full sample row incl. `error_body`. 404/405 groups carry
   `routed_methods` (what IS mounted at that path, from the ROUTE_TABLE) and
   404s carry `nearest_routes`; the response ends with `verdicts` — a computed
   one-liner per 405 group that already states the conclusion (not routed /
   unknown path answered by the GET-only catch-all / routed NOW so the rows
   predate the running build). The judgment call is unchanged: 401/404 probe
   noise is not a finding; a 500 is always a finding; a verdict naming a
   missing route is a finding. Deep-dive fallback:
   `GET /api/logs?since=$SINCE&min_status=400&limit=2000` (raw rows).

   **Known-benign 404s — do not file these.** Some 404s are the product working
   as designed, and re-investigating them every day is a slow tax on this sweep:

   - `/api/stripe/status` (~3/day, dashboard UA). CLOUD-ONLY feature detection,
     not dead code. `_loadCloudPlan()` and `loadBillingSection()` in app.js both
     fetch it and hide their section on `!r.ok`, which is exactly what a
     self-hosted install should do — the comment above the first call says so.
     A 404 here means "self-hosted", not "broken".

   Add to this list rather than re-deriving it. AF-32 was filed on this endpoint
   after checking only that it 404s on both origins — a true fact that supported
   the wrong conclusion, because the discriminator is not "does any origin serve
   it" but "does the CLIENT handle the 404 deliberately". Check the call site
   before filing a 404 whose caller is our own dashboard.

2. **Latency: one call.** `GET /api/logs/stats?since_h=24`
   Per family: count, p50_ms / p95_ms / max_ms (nearest-rank percentiles over
   the window's sorted latencies; the method is named in `percentile_method`),
   `actual_window_h` + `scan_truncated` — READ THESE BEFORE TRUSTING THE NORM,
   error_count, error_rate, proxy_count (strictly `python-proxy`), `origins`
   (per-`answered_by` counts — the coverage tell above, precomputed),
   distinct_workers, distinct_clients, plus `slow_outliers` (rows > 5x their
   family p50, capped 20) and `totals`. For the trailing norm, call it again
   with `since_h=192` and compare. Finding = today's p95 > ~2x trailing p95
   (use judgment on low-volume families; never conclude from n < 20 requests).

   **The trailing norm is capped, and the cap lies about its size (AR-134).**
   `stats` scans at most 200,000 rows. Asking for `since_h=192` on a busy day
   returns `scan_truncated: true` and an `actual_window_h` of ~35, not 192 —
   identical counts come back for 192h, 336h and 720h, because all three hit the
   same cap. So "the 8-day norm" can silently be 1.5 days.

   Judge it on `actual_window_h`, NOT by comparing `totals.count` between the two
   calls. Comparing counts was the first version of this guard and it gives the
   WRONG answer in both directions: equal counts read as "vacuous" when the log
   genuinely spans one day, and DIFFERENT counts read as "usable" when the only
   reason they differ is the cap. On 2026-08-11 that guard reported the norm
   usable over what was really a 35h window.

   Say the real window in the summary — "p95 vs a 35h norm, not 8 days" — rather
   than reporting a comparison the reader will assume covers a week.
   Deep-dive fallback: `GET /api/logs?since=$SINCE&family=/api/board&limit=2000`.

   Routing questions along the way ("is PATCH mounted at X?") are answered by
   `GET /api/debug/routes` — the ROUTE_TABLE as JSON — never by a grep.

3. **Proxy volume — now zero, so ANY row is a finding.**
   `GET /api/logs?since=$SINCE&answered_by=python-proxy&limit=1` -> `total_matched`.
   `GET /api/debug/boundary` -> `proxied` is `[]` (49 native families) since the
   Python retirement, and there is no Python process left to proxy TO. So the
   finding is simply: **any `python-proxy` row timestamped after the cutover.**
   Cross-check the family against `proxied` — a row naming a family that is not
   even on that list is the same regression seen from the other side.

   Rewritten 2026-08-09 (AF-33) because the old condition — "record the number,
   finding = it ROSE VS YESTERDAY" — could never fire. Nothing persisted
   yesterday's number: the sweep is a scheduler prompt to a fresh session with no
   memory of the prior run, `/api/logs` takes only `since` and has no upper bound,
   so the comparison had no left-hand side. A check that cannot fail, sitting
   inside the sweep whose whole job is catching regressions (ethos rule 7).

   Note the fix is NOT "persist a baseline". The cutover made the baseline
   unnecessary: the expected value is now a constant zero, and a rule with a
   constant needs no history. Building a store to compare against would have been
   machinery in service of a question that had stopped being the right one.

   **Reading the residue correctly.** Rows can be pre-cutover and harmless. When
   this was written, 10 rows existed in the window, all `/api/scope`, timestamped
   19:07–19:36 against a cutover at 20:43 — historical, and they age out with
   `AMUX_REQLOG_RETAIN_DAYS`. Compare each row's `ts` against the cutover before
   calling it a regression; "10 proxy rows" alone is not a finding, and reporting
   it as one is how this sweep loses credibility.

4. **401/403 spikes by client IP.**
   `GET /api/logs?since=$SINCE&min_status=401&limit=2000`, keep status 401/403,
   group by `ip`. Finding = any non-loopback IP with a burst (>20/day), or a
   loopback caller failing auth repeatedly (a broken token on a lane).

   **403 is not only an auth code here.** amux uses it for POLICY refusals too, so
   a 403 is usually a guard working. All three in the 2026-08-10 window were:
   `cannot delete pinned session — unpin first`, the same for archive, and a
   cross-lane `session 'X' may not report for 'Y'` refusing a deliberate probe.
   Read `error_body` before counting a 403 as an auth failure — the spike shape
   (one IP, many, fast) is the signal, not the status on its own.

5. **Worker traffic with no board trace.** Collect distinct `worker` values from
   `GET /api/logs?since=$SINCE&limit=2000`, keeping only rows whose `method` is
   POST/PATCH/PUT/DELETE; cross-check
   `GET /api/board?done_limit=100000` for cards with that session
   updated in the window. Finding = a worker doing MUTATING work whose board
   shows nothing in `doing`/updated — silent work (task-ledger rule violation),
   or a runaway loop hammering the API.

   Three qualifications, each from a false positive this rule produced on
   2026-08-09 (AF-34). It accused a peer, and the accusation is the expensive
   kind — you cannot un-say "you are working off-ledger":

   - **Mutating methods only.** `amux-homepage` was flagged on 105 requests with
     0 cards. All 105 were `GET/POST /api/sessions` — 103 GETs, every one a 200:
     polling and messaging, not work. Reading the board and peeking at lanes is
     not silent work, and under the old wording every idle observer looked guilty.
   - **No cards on this board at all = UNKNOWN, not silent.** Boards are
     per-instance. `amux-homepage`'s card (AH-70) is not on this board; it was
     `verified` on their own instance the same day. A worker with zero cards here
     is unmeasurable from here, and "unmeasurable" must not render as "violating".
     Same cross-instance root as AF-28 (`CARD:` ids are not one namespace).
   - **The board GET must be uncapped — but do NOT add `&archived=1`.**
     Plain `GET /api/board` caps done items (1,441 of 4,689 when measured), so the
     cross-check silently misses cards and invents silence; a related audit
     reported 48 missing cards that way when the real number was 1. `done_limit`
     alone fixes it: that response is already the union (4,766 rows).
     `archived` is a FILTER, not an include-flag — `archived=1` returns ONLY
     archived cards (2,553 rows, zero belonging to an active lane), which makes
     the cross-check worse than the bug it was meant to fix. This is written down
     because the first draft of THIS fix added `&archived=1` "to be thorough" and
     its own control caught it: the author's lane came back with 0 cards while
     holding 36. Adding a filter and removing one are each wrong half the time
     (ethos rule 1) — copy the predicate from something that works, do not
     re-derive it from what sounds careful.

   Before filing, confirm the worker has *any* card here. If it does not, the
   honest finding is "cannot evaluate from this board", not a violation.

   **Two more, both of which produced a false accusation on 2026-08-16 and were
   caught only by re-querying the store.** Neither is hypothetical; the first is
   this file's own rule, violated by someone who had just read it.

   - **Attribute on `amux_session` ONLY. Never fall back to `worker`.** `worker`
     is PATH-derived (`/api/sessions/{name}/*`), so an UNATTRIBUTED report *about*
     lane X is tagged `worker=X` and reads as a mutation *by* X. That is what the
     header of this file means by "worker logs are a filter, never a second log".
     Using it as a fallback flagged `mixpeek-security` as working off-ledger on 5
     requests it never made — the store showed ZERO mutating rows for it. With
     7,708 unattributed reports/day (AF-67) this fallback manufactures a silent
     worker for every busy lane on the board.
   - **Test `max(created, updated)`, not `updated`.** A worker whose only board
     action was CREATING a card has `updated` unset, so an `updated >= since`
     check reads it as silent — exactly backwards, since minting a card IS the
     ledger working. And a card created FOR another session (delegation, which is
     normal and is how `amux` seeds work) is attributed to the target's `session`,
     not the creator's, so the creator looks idle on both fields. Check the
     request log for `POST /api/board` before calling anyone silent.

   The sample is also capped: `limit=2000` is the max, and on a busy day that is
   ~2.7 hours of a 24h window, taken from one end. Say the real span in the
   summary, or read the store directly for the full window.

6. **Status truth: does the card agree with the pane?** (AMUX-2646)
   `GET /api/health/invariants` (detail: `GET /api/debug/invariants`) — read the
   `status.agrees_with_pane` results. A `fail`
   names a lane whose card says `idle` while its pane is unambiguously mid-turn,
   with the report's state/age/source/origin in the evidence blob.

   This sweep exists because the incident it is named for was caught by a human
   looking at a terminal, and nothing else in amux could have caught it: a
   fabricated `idle` self-report (`source: stop-hook-test`, written onto a live
   working lane) outranked every other signal, and an `idle` report does not
   decay for 24h. There was no query that would have shown it — the report store
   was healthy, the derivation was healthy, the pane was healthy, and only the
   SEAM between them was wrong.

   Read the direction of the check before acting on it. Only `idle`-over-a-
   working-pane is a contradiction; `active` over a quiet pane is normal (a long
   tool call, a subagent) and is deliberately not flagged. If it ever starts
   firing on many lanes at once, suspect the DETECTOR (a Claude Code UI change),
   not the fleet — confirm against one pane by eye before filing per-lane cards.

   Same check, read-only, without the server:
   ```
   CARGO_TARGET_DIR=/tmp/amux-status-target cargo test -p amux-server \
     sessions_legacy::status_truth::live_fleet -- --ignored --nocapture
   ```
   It prints the fleet size, how many lanes painted inside the probe window, how
   many of those are mid-turn, the disagreement count, and the full status
   histogram. Read the histogram too: a disagreement count of 0 is also what a
   fleet that has flipped entirely to `active` would report.

Also skim `GET /api/logs/raw?lines=500` for `sources:"server_log"` lines matching
ERROR/WARN — the tracing tail carries failures that never became a request row.

## Triage rule (mandatory)

Every finding becomes ONE board card (`amux board add --stdin`), containing:
the finding in one line, the exact query that found it (verbatim URL), the
numbers (count/p95/IP), and the suspected family/worker. No umbrella cards.
Nothing found = no cards and no message; do not file "sweep ran" noise.
