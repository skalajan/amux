-- 0012: invariant results + diagnostic incidents (AMUX-2622).
--
-- The spine for "amux continuously proves to itself that its subsystems agree".
-- ADDITIVE ONLY (shared live DB).
--
-- TWO TABLES, NOT ONE, and the split is the whole design:
--
--   _amux_invariant_result   every EVALUATION. Append-only, retained briefly.
--                            Answers "was this checked, and when?" — which is
--                            the question a silent monitor cannot answer. A
--                            check that stops running must look different from
--                            a check that runs and passes, or the most
--                            dangerous failure (the observer died) reads as
--                            health. This is the ethos rule-4 lesson: a skip
--                            that leaves no trace is indistinguishable from a
--                            scan that found nothing.
--
--   _amux_invariant_incident  one row per (invariant_id, entity_key) that is
--                            currently or was recently FAILING. Updated in
--                            place — occurrences++ and last_seen moves — so a
--                            check failing every 30s for a day is ONE incident
--                            with 2880 occurrences, not 2880 incidents. The
--                            spec's "recurring failure should create/update one
--                            diagnostic incident, not spam duplicates".
--
-- WHY entity_key IS PART OF THE IDENTITY: "worker X has no backend process" and
-- "worker Y has no backend process" are two incidents, not one flapping one.
-- Collapsing on invariant_id alone would hide the second worker entirely.

CREATE TABLE IF NOT EXISTS _amux_invariant_result (
    id            INTEGER PRIMARY KEY,
    ts            REAL    NOT NULL,          -- unix seconds, evaluation time
    invariant_id  TEXT    NOT NULL,          -- stable slug, e.g. "route.canonical_verbs_mounted"
    status        TEXT    NOT NULL,          -- pass | fail | unknown | skipped
    entity_key    TEXT    NOT NULL DEFAULT '',-- '' for fleet-wide checks
    -- expected/observed are the DISCRIMINATOR. A failure that records only a
    -- message forces the next person to re-derive what was compared; these two
    -- make the contradiction readable without re-running anything.
    expected      TEXT    NOT NULL DEFAULT '',
    observed      TEXT    NOT NULL DEFAULT '',
    evidence      TEXT    NOT NULL DEFAULT '',-- JSON: the causal slice
    duration_ms   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_inv_result_ts   ON _amux_invariant_result(ts DESC);
CREATE INDEX IF NOT EXISTS idx_inv_result_id   ON _amux_invariant_result(invariant_id, ts DESC);

CREATE TABLE IF NOT EXISTS _amux_invariant_incident (
    id            INTEGER PRIMARY KEY,
    invariant_id  TEXT    NOT NULL,
    entity_key    TEXT    NOT NULL DEFAULT '',
    status        TEXT    NOT NULL,          -- fail | unknown  (pass closes it)
    first_seen    REAL    NOT NULL,
    last_seen     REAL    NOT NULL,
    occurrences   INTEGER NOT NULL DEFAULT 1,
    expected      TEXT    NOT NULL DEFAULT '',
    observed      TEXT    NOT NULL DEFAULT '',
    evidence      TEXT    NOT NULL DEFAULT '',
    -- resolved_at is NULL while live. Kept rather than deleted so "this broke,
    -- then healed" is visible — a self-healing flap is a real signal and
    -- deleting the row erases it.
    resolved_at   REAL,
    -- board_issue is set when a card was filed for this incident, so the
    -- filing is idempotent: one incident -> at most one card, forever.
    board_issue   TEXT    NOT NULL DEFAULT '',
    UNIQUE(invariant_id, entity_key)
);
CREATE INDEX IF NOT EXISTS idx_inv_incident_live ON _amux_invariant_incident(resolved_at, last_seen DESC);
