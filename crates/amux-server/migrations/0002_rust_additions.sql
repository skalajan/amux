-- 0002_rust_additions.sql — Rust-server schema ADDITIONS on top of 0001_baseline.
--
-- Everything here is additive and must leave the DB fully readable/writable by the
-- Python server (Phase 11: same DB file, both directions). New tables use the
-- `_amux_` prefix so they can never collide with a Python table or a user `wb_*`
-- workbench table.
--
-- ── Column additions: NO raw ALTER statements in this file ──
-- SQLite's `ALTER TABLE ... ADD COLUMN` has no IF NOT EXISTS form, and the Python
-- server may itself grow columns on these tables at any time. The Rust migration
-- runner MUST therefore:
--   1. parse the `-- ADDCOL: <table> <column> <decl>` lines below,
--   2. for each, run `PRAGMA table_info(<table>)`,
--   3. apply `ALTER TABLE <table> ADD COLUMN <column> <decl>` ONLY when the column
--      is absent,
--   4. treat "already present" as success (idempotent re-runs).
-- Declarations are constant-default NOT NULL so ALTER ADD COLUMN is legal on
-- populated tables and existing rows get a well-defined value the Python server
-- simply ignores.
--
-- Entity tables receiving a `version` column (per-row optimistic-versioning for the
-- Rust API; distinct from issues.rev which the Python board API already bumps):
--   - issues          — board cards
--   - schedules       — scheduler entries
--   - tasks           — per-session todo items
--   - steering_queue  — queued inter-session/steering messages
-- Not versioned, with reasons:
--   - workers/sessions registry: no such table exists in amux.db (sessions live in
--     tmux + ~/.amux files; session_events is append-only, versioning is meaningless)
--   - memories: no table (memories are files on disk)
--   - steering_history, session_events, *_log, *_runs, token_ledger: append-only
--
-- ADDCOL: issues version INTEGER NOT NULL DEFAULT 0
-- ADDCOL: schedules version INTEGER NOT NULL DEFAULT 0
-- ADDCOL: tasks version INTEGER NOT NULL DEFAULT 0
-- ADDCOL: steering_queue version INTEGER NOT NULL DEFAULT 0

-- Global monotonic revision counter (single row, id=1). Bumped by the Rust server
-- on any mutating write; lets clients cheaply ask "did anything change?".
CREATE TABLE IF NOT EXISTS _amux_rev (
    id  INTEGER PRIMARY KEY CHECK (id = 1),
    rev INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO _amux_rev (id, rev) VALUES (1, 0);

-- Migration ledger for the Rust migration runner. The runner records each applied
-- migration version here (0001 baseline is recorded as version 1 even when it was
-- a no-op against an already-populated Python DB).
CREATE TABLE IF NOT EXISTS _amux_migrations (
    version    INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);

-- Revisioned StateEvent journal (Invariant 35). Every applied write appends
-- its events here under the revision it committed at; /api/sync?since_rev=N
-- and SSE gap-recovery read from it. Append-only; pruning (retention window)
-- is a later phase and must keep the "oldest retained rev" queryable so
-- delta sync can detect an unbridgeable gap and demand a full sync.
CREATE TABLE IF NOT EXISTS _amux_state_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    rev         INTEGER NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    mutation    TEXT NOT NULL,       -- JSON MutationKind
    at          TEXT NOT NULL        -- RFC3339
);
CREATE INDEX IF NOT EXISTS idx_amux_state_events_rev ON _amux_state_events(rev);
