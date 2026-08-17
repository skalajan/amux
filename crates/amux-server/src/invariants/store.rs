//! Durable persistence for invariant results and incidents (AMUX-2622).
//!
//! The dedupe rule lives here rather than in the caller so it cannot be
//! forgotten by a future check: recording a failure for an
//! (invariant_id, entity_key) that is already failing UPDATES that incident
//! (occurrences++, last_seen moves) instead of inserting a second one, and
//! recording a pass RESOLVES it. A check firing every 30s for a day is one
//! incident with 2880 occurrences.

use super::{InvariantResult, Status};
use crate::db::{SharedStore, WriteOutcome};
use serde_json::json;

/// Retention for the append-only evaluation log. Short by design: its purpose
/// is "is this check still running / when did it last pass", not history.
/// Incidents are the durable record and are never trimmed here.
const RESULT_RETAIN_SECS: f64 = 7.0 * 86400.0;

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Persist a batch of results and reconcile incidents in ONE write.
///
/// Batched deliberately: the store is single-writer, and a per-result write
/// would put a full checker pass (dozens of results) into as many write-lock
/// acquisitions as it has checks — the shape that produced log-lock contention
/// before. Returns the number of incidents newly OPENED, which is the signal a
/// caller acts on.
pub async fn record(store: &SharedStore, results: Vec<InvariantResult>, duration_ms: i64) -> usize {
    if results.is_empty() {
        return 0;
    }
    let opened = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let opened_w = opened.clone();
    let _ = store
        .write_async(move |conn| {
            let ts = now();
            for r in &results {
                conn.execute(
                    "INSERT INTO _amux_invariant_result
                       (ts, invariant_id, status, entity_key, expected, observed, evidence, duration_ms)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    rusqlite::params![
                        ts,
                        r.invariant_id,
                        r.status.as_str(),
                        r.entity_key,
                        r.expected,
                        r.observed,
                        r.evidence.to_string(),
                        duration_ms,
                    ],
                )?;

                if r.status.opens_incident() {
                    // UPSERT: one incident per (invariant, entity), forever.
                    // first_seen is preserved by the DO UPDATE (it is simply
                    // not assigned), which is what makes "this has been broken
                    // since 09:14" answerable after 2880 occurrences.
                    let changed = conn.execute(
                        "INSERT INTO _amux_invariant_incident
                           (invariant_id, entity_key, status, first_seen, last_seen,
                            occurrences, expected, observed, evidence)
                         VALUES (?1,?2,?3,?4,?4,1,?5,?6,?7)
                         ON CONFLICT(invariant_id, entity_key) DO UPDATE SET
                           status      = excluded.status,
                           last_seen   = excluded.last_seen,
                           occurrences = _amux_invariant_incident.occurrences + 1,
                           expected    = excluded.expected,
                           observed    = excluded.observed,
                           evidence    = excluded.evidence,
                           -- A previously-resolved incident that fails again
                           -- REOPENS rather than staying closed, so a flap is
                           -- visible as one incident with a resolved_at that
                           -- went back to NULL.
                           resolved_at = NULL",
                        rusqlite::params![
                            r.invariant_id,
                            r.entity_key,
                            r.status.as_str(),
                            ts,
                            r.expected,
                            r.observed,
                            r.evidence.to_string(),
                        ],
                    )?;
                    // rows_changed==1 on a fresh INSERT, 1 on UPDATE too, so
                    // discriminate on occurrences instead.
                    let occ: i64 = conn
                        .query_row(
                            "SELECT occurrences FROM _amux_invariant_incident
                             WHERE invariant_id=?1 AND entity_key=?2",
                            rusqlite::params![r.invariant_id, r.entity_key],
                            |row| row.get(0),
                        )
                        .unwrap_or(1);
                    if changed > 0 && occ == 1 {
                        opened_w.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                } else if r.status == Status::Pass {
                    // A pass RESOLVES the matching live incident. Deliberately
                    // not a DELETE: "broke at 09:14, healed at 09:51" is a real
                    // signal, and deleting the row erases the flap.
                    conn.execute(
                        "UPDATE _amux_invariant_incident
                            SET resolved_at = ?3, status = 'pass'
                          WHERE invariant_id = ?1 AND entity_key = ?2
                            AND resolved_at IS NULL",
                        rusqlite::params![r.invariant_id, r.entity_key, ts],
                    )?;
                }
            }
            // Opportunistic trim of the evaluation log only.
            let _ = conn.execute(
                "DELETE FROM _amux_invariant_result WHERE ts < ?1",
                [ts - RESULT_RETAIN_SECS],
            );
            Ok(WriteOutcome { applied: true, events: vec![] })
        })
        .await;
    opened.load(std::sync::atomic::Ordering::Relaxed)
}

/// Live (unresolved) incidents, worst-and-freshest first.
pub fn live_incidents(store: &SharedStore) -> anyhow::Result<Vec<serde_json::Value>> {
    let conn = store.read()?;
    let mut stmt = conn.prepare(
        "SELECT invariant_id, entity_key, status, first_seen, last_seen, occurrences,
                expected, observed, evidence, board_issue
           FROM _amux_invariant_incident
          WHERE resolved_at IS NULL
          ORDER BY last_seen DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({
                "invariant_id": r.get::<_, String>(0)?,
                "entity":       r.get::<_, String>(1)?,
                "status":       r.get::<_, String>(2)?,
                "first_seen":   r.get::<_, f64>(3)?,
                "last_seen":    r.get::<_, f64>(4)?,
                "occurrences":  r.get::<_, i64>(5)?,
                "expected":     r.get::<_, String>(6)?,
                "observed":     r.get::<_, String>(7)?,
                "evidence":     serde_json::from_str::<serde_json::Value>(
                                    &r.get::<_, String>(8)?).unwrap_or(serde_json::Value::Null),
                "board_issue":  r.get::<_, String>(9)?,
            }))
        })?
        .flatten()
        .collect();
    Ok(rows)
}

/// Latest evaluation per invariant — the "is this check still running?" view.
///
/// `age_s` is the point of this query. A check whose newest row is hours old
/// has stopped running, and that must be visible as staleness rather than as
/// its last (stale) verdict — evidence freshness, spec §25.
pub fn latest_per_invariant(store: &SharedStore) -> anyhow::Result<Vec<serde_json::Value>> {
    let conn = store.read()?;
    let mut stmt = conn.prepare(
        "SELECT r.invariant_id, r.status, r.entity_key, r.expected, r.observed, r.ts
           FROM _amux_invariant_result r
           JOIN (SELECT invariant_id, MAX(ts) mt
                   FROM _amux_invariant_result GROUP BY invariant_id) m
             ON m.invariant_id = r.invariant_id AND m.mt = r.ts
          ORDER BY r.invariant_id",
    )?;
    let now = now();
    let rows = stmt
        .query_map([], |r| {
            let ts: f64 = r.get(5)?;
            Ok(json!({
                "invariant_id": r.get::<_, String>(0)?,
                "status":       r.get::<_, String>(1)?,
                "entity":       r.get::<_, String>(2)?,
                "expected":     r.get::<_, String>(3)?,
                "observed":     r.get::<_, String>(4)?,
                "checked_at":   ts,
                "age_s":        (now - ts).max(0.0),
            }))
        })?
        .flatten()
        .collect();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invariants::InvariantResult;

    fn store() -> (SharedStore, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let s = crate::db::Store::open(&d.path().join("t.db")).unwrap();
        (std::sync::Arc::new(s), d)
    }

    /// THE dedupe property: the same failure 100 times is ONE incident.
    /// Without this the health surface becomes a scrolling wall and the second
    /// distinct failure is invisible — which defeats the point of having it.
    #[tokio::test]
    async fn repeated_failure_is_one_incident_with_a_count() {
        let (s, _d) = store();
        for _ in 0..100 {
            record(
                &s,
                vec![InvariantResult::fail("x.check", "a", "b").entity("w1")],
                1,
            )
            .await;
        }
        let inc = live_incidents(&s).unwrap();
        assert_eq!(inc.len(), 1, "100 identical failures must be ONE incident");
        assert_eq!(inc[0]["occurrences"], 100);
    }

    /// Two entities failing the same check are two incidents — collapsing them
    /// on invariant_id alone would hide the second worker completely.
    #[tokio::test]
    async fn distinct_entities_are_distinct_incidents() {
        let (s, _d) = store();
        record(&s, vec![InvariantResult::fail("x", "a", "b").entity("w1")], 1).await;
        record(&s, vec![InvariantResult::fail("x", "a", "b").entity("w2")], 1).await;
        assert_eq!(live_incidents(&s).unwrap().len(), 2);
    }

    /// A pass closes the incident, and a later failure REOPENS the same row so
    /// the flap is one story rather than two unrelated ones.
    #[tokio::test]
    async fn pass_resolves_and_a_later_failure_reopens() {
        let (s, _d) = store();
        record(&s, vec![InvariantResult::fail("x", "a", "b").entity("w1")], 1).await;
        assert_eq!(live_incidents(&s).unwrap().len(), 1);

        record(&s, vec![InvariantResult::pass("x").entity("w1")], 1).await;
        assert_eq!(live_incidents(&s).unwrap().len(), 0, "pass must resolve it");

        record(&s, vec![InvariantResult::fail("x", "a", "b").entity("w1")], 1).await;
        let inc = live_incidents(&s).unwrap();
        assert_eq!(inc.len(), 1, "a re-failure must reopen, not stay closed");
        assert_eq!(inc[0]["occurrences"], 2, "history is preserved across the flap");
    }

    /// Unknown must NOT open an incident (it is a gap in observation, not a
    /// proven contradiction) but must still be recorded so it is visible.
    #[tokio::test]
    async fn unknown_records_but_does_not_page() {
        let (s, _d) = store();
        record(&s, vec![InvariantResult::unknown("x", "probe down").entity("w1")], 1).await;
        assert_eq!(live_incidents(&s).unwrap().len(), 0, "unknown must not open an incident");
        let latest = latest_per_invariant(&s).unwrap();
        assert_eq!(latest.len(), 1, "...but it must still be recorded");
        assert_eq!(latest[0]["status"], "unknown");
    }
}
