-- 0003_workers.sql — Worker/session/lease entities for the Rust server
-- (Phase 1: RR-0034, RR-0029, Invariant 1).
--
-- The Python server has NO workers table: sessions live as tmux state plus
-- ~/.amux/sessions/*.env files. The Rust system makes workers durable
-- entities (Invariant 1: Worker != Session != Backend). All tables are new,
-- `_amux_`-prefixed, and invisible to the Python server — additive both
-- directions (Phase 11 rollback holds).

CREATE TABLE IF NOT EXISTS _amux_workers (
    id           TEXT PRIMARY KEY,             -- wrk_<ULID>, immutable
    display_name TEXT NOT NULL,
    name_aliases TEXT NOT NULL DEFAULT '[]',   -- JSON array; renames append here (Inv 17/43)
    cwd          TEXT NOT NULL DEFAULT '',
    provider     TEXT NOT NULL DEFAULT 'claude',
    model        TEXT,
    backend      TEXT NOT NULL DEFAULT 'herdr',
    environment  TEXT NOT NULL DEFAULT '{}',   -- JSON object, worker-scope env layer
    permissions  TEXT NOT NULL DEFAULT '[]',   -- JSON array
    group_id     TEXT,                          -- grp_<ULID>, nullable
    state        TEXT NOT NULL DEFAULT '{"state":"stopped"}',  -- JSON WorkerState
    version      INTEGER NOT NULL DEFAULT 0,   -- optimistic concurrency (Inv 35)
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_amux_workers_name ON _amux_workers(display_name);

CREATE TABLE IF NOT EXISTS _amux_sessions (
    id          TEXT PRIMARY KEY,              -- ses_<ULID>
    worker_id   TEXT NOT NULL REFERENCES _amux_workers(id),
    backend     TEXT NOT NULL,
    backend_ref TEXT NOT NULL,                 -- amux-<worker_id>
    pid         INTEGER,
    started_at  TEXT NOT NULL,
    ended_at    TEXT,
    exit_reason TEXT                            -- JSON ExitReason, NULL while live
);
CREATE INDEX IF NOT EXISTS idx_amux_sessions_worker ON _amux_sessions(worker_id);

-- Task leases (Invariant 9): claiming is an atomic INSERT-or-conflict;
-- expiry makes the task runnable again with a bumped generation so a dead
-- worker's late writes are recognizably stale.
CREATE TABLE IF NOT EXISTS _amux_leases (
    task_id     TEXT PRIMARY KEY,              -- one lease per task
    worker_id   TEXT NOT NULL,
    acquired_at TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    generation  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_amux_leases_worker ON _amux_leases(worker_id);

-- Attempt records (Invariant 49): every failed attempt's reason rides into
-- the next attempt's context — retries are not re-rolls.
CREATE TABLE IF NOT EXISTS _amux_attempts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id     TEXT NOT NULL,
    worker_id   TEXT NOT NULL,
    attempt     INTEGER NOT NULL,
    record      TEXT NOT NULL,                 -- JSON AttemptRecord
    at          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_amux_attempts_task ON _amux_attempts(task_id);
