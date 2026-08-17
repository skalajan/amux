-- 0004_commands.sql — Durable per-worker command queue (Phase 4 seed,
-- Invariant 34). New `_amux_` table, invisible to the Python server.
--
-- One row per queued WorkerCommand. `state` is the JSON CommandState from
-- amux-core (queued/dispatched/delivered/confirmed/failed/dead_lettered);
-- `attempts` counts recorded failures, mirroring QueuedCommand. The
-- idempotency key is UNIQUE per worker: re-enqueueing the same key returns
-- the existing command instead of double-queueing (Invariant 9).

CREATE TABLE IF NOT EXISTS _amux_commands (
    id              TEXT PRIMARY KEY,          -- cmd_<ULID>
    worker_id       TEXT NOT NULL,
    command         TEXT NOT NULL,             -- JSON WorkerCommand
    state           TEXT NOT NULL,             -- JSON CommandState
    idempotency_key TEXT NOT NULL,
    queued_at       TEXT NOT NULL,             -- RFC3339
    attempts        INTEGER NOT NULL DEFAULT 0,
    timing          TEXT NOT NULL,             -- JSON DeliveryTiming
    precondition    TEXT,                      -- JSON CommandPrecondition, NULL = none
    UNIQUE (worker_id, idempotency_key)
);
CREATE INDEX IF NOT EXISTS idx_amux_commands_worker ON _amux_commands(worker_id, queued_at);
