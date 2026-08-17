-- 0007_criteria.sql — Stored acceptance criteria (RR-0048d, Invariant 50).
--
-- One row per board task carrying its acceptance criteria as JSON
-- (amux_core::criteria::AcceptanceCriteria). Keyed by the SEMANTIC board id
-- so both servers can read it, though only the Rust server enforces.
-- Authorship separation is enforced at write time: the executor cannot
-- author its own acceptance.

CREATE TABLE IF NOT EXISTS _amux_criteria (
    task_id     TEXT PRIMARY KEY,              -- semantic board id (AMUX-123)
    criteria    TEXT NOT NULL,                 -- JSON AcceptanceCriteria
    authored_by TEXT NOT NULL,                 -- JSON CriteriaAuthor
    version     INTEGER NOT NULL DEFAULT 1,
    updated_at  TEXT NOT NULL
);
