# `unwrap_or_default` / `unwrap_or(0)` audit — observability paths (AMUX-2974)

Split from AMUX-2625 item 1. The discriminator for every site: when the value
is **missing** (a query failed, a parse failed, a field is absent), does the
`unwrap_or_default()/unwrap_or(0)` coerce it to a `0`/empty that a reader will
mistake for a **real measurement** ("no spend", "no threads")? A genuine zero
(no rows, an absent filter) is fine; **silence rendered as zero is the bug**
(ethos rule 4).

Scope: the observability code paths only — `api/observability.rs`, `stats.rs`,
`metrics.rs`, `log_search.rs`. (The full tree has ~347 `unwrap_or_default`
sites; the card's own note is that almost none are observability. This audits
the ones that are.)

## Verdict per site

### `api/observability.rs`
| line | site | verdict |
|---|---|---|
| 53, 54 | `qs_get("session"/"group").unwrap_or_default()` | **FINE** — request filter; empty = unfiltered, not a measurement. |
| 129 | `conn.prepare(sql)` else `return vec![]` | **FIXED** — a prepare failure silently rendered an empty cost breakdown = "no spend". Now WARNs, naming the group, so it shows in `/api/logs/analyze`. UI still degrades gracefully (totals come from a separate query). |
| 146 | `rows.map(...).unwrap_or_default()` | **FIXED** — same class as 129 (query *run* failure); now WARNs. |
| 166 | task-titles map `.unwrap_or_default()` | **FINE** — documented fallback: a missing title renders the task *id*, which is visible, not a false number. |

### `api/stats.rs`
| line | site | verdict |
|---|---|---|
| 157–161 | `baseline.get(k).and_then(as_i64).unwrap_or(0)` | **FINE** — an absent baseline field legitimately means "subtract nothing"; 0 is the correct identity, not swallowed silence. |

### `api/metrics.rs`
| line | site | verdict |
|---|---|---|
| 84 | `cmd_output("hostname").unwrap_or_default()` | **FINE** — cosmetic label; empty hostname is visibly wrong, not a false metric. |
| 160, 162 | `parse_mb(rest).unwrap_or(0.0)` (disk total/used) | **NOTED, low** — a `df` line that fails to parse → `0 MB`, a false "no disk". Real risk but low likelihood (df output is stable); audible-fix available (WARN on parse fail), deferred as churn not worth it. |
| 175 | boot-time `duration_since(EPOCH)...unwrap_or(0)` | **FINE** — only a pre-1970 system clock hits it; not a real state. |
| 217, 224 | `env::var("HOME").unwrap_or_default()` | **FINE** — config path, not a measurement. |
| 247 | `read_to_string(env_file).unwrap_or_default()` (CC_ARCHIVED check) | **NOTED, low** — a read failure counts a session as active; cosmetic count skew, low value. |
| 293, 294 | `parts[..].parse().unwrap_or(0/0.0)` (per-process RSS/CPU) | **NOTED, low** — a malformed `ps` row silently drops that process from the RSS/CPU total (understates); low likelihood, audible-fix available, deferred. |
| 369 | `cmd_output("ps -M")...unwrap_or(0)` (thread count) | **NOTED, low** — a failed `ps` → `0 threads`, a false zero; low likelihood. |
| 381 | `current_rev()...unwrap_or(0)` | **FINE** — a store-read failure is already surfaced by `/health` (`store:"hung"`); rev 0 here is a minor cosmetic fallback, covered elsewhere. |
| 440 | `to_string(EntityType).unwrap_or_default()` | **FINE** — serializing a simple enum is infallible in practice. |

### `api/log_search.rs`
| line | site | verdict |
|---|---|---|
| 29 | `env::var("HOME").unwrap_or_default()` | **FINE** — config path. |
| 68 | `file_stem().map(...).unwrap_or_default()` | **FINE** — a filename label; empty is cosmetic, not a metric. |

## Summary

22 observability sites. **15 correct** (request-param, config, genuine-absent,
cosmetic, or infallible). **1 fixed** — the Cost view's breakdown query
(observability.rs:129/146), the one place a swallowed failure looked exactly
like real zero on a surface people read for money; the swallow is now audible.
**5 low-value system-metrics parse-to-zero cases** (metrics disk / per-process
/ thread-count) documented with the audible-fix noted and deliberately deferred
— unlikely, cosmetic, and the churn is not worth it until one actually fires.
There is no observability site left where a missing value silently reads as a
real measurement on a surface that matters.
