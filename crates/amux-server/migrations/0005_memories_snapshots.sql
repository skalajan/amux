-- 0005_memories_snapshots.sql — Memory entries + immutable context snapshots
-- (Phase 4 seeds: RR-0071 / Invariant 42, RR-0070 / Invariant 27). New
-- `_amux_`-prefixed tables, invisible to the Python server — additive both
-- directions (Phase 11 rollback holds).

-- Canonical memory store (Invariant 42). Everything else (context assembly,
-- MEMORY.md projection, search) derives from these rows and never writes
-- back. Visibility is decided by amux_core::scope via amux_core::memory::
-- visible — the ONE resolver — so `scope` is stored as the serde JSON of
-- core's Scope and re-parsed on read, never re-interpreted in SQL.
CREATE TABLE IF NOT EXISTS _amux_memories (
    id          TEXT PRIMARY KEY,             -- mem_<ULID>
    scope       TEXT NOT NULL,                -- JSON Scope, e.g. {"level":"worker","id":"wrk_..."}
    name        TEXT NOT NULL,                -- kebab-case slug, unique within live scope
    content     TEXT NOT NULL,
    memory_type TEXT NOT NULL,                -- user | feedback | project | reference
    version     INTEGER NOT NULL DEFAULT 1,   -- entity version (Invariant 35)
    created_at  TEXT NOT NULL,                -- RFC3339
    updated_at  TEXT NOT NULL,                -- RFC3339
    deleted_at  TEXT,                          -- soft delete: set, never removed (Invariant 42)
    provenance  TEXT NOT NULL                 -- JSON MemoryProvenance ({"kind":...})
);
CREATE INDEX IF NOT EXISTS idx_amux_memories_name ON _amux_memories(name);
-- Uniqueness within scope: equality on the JSON text is sound because serde
-- emits exactly one spelling per Scope value. Partial on live rows so a
-- soft-deleted entry frees its name while the historical row stays put.
CREATE UNIQUE INDEX IF NOT EXISTS idx_amux_memories_scope_name
    ON _amux_memories(scope, name) WHERE deleted_at IS NULL;

-- Immutable context snapshots (Invariant 27): one row per work assignment,
-- recording exactly what the worker received. `assignment_key` is the
-- planner's idempotency key ("<task>:<worker>:<attempt>"); INSERT OR IGNORE
-- against its UNIQUE constraint is what makes recording idempotent — a
-- re-planned assignment re-records nothing.
CREATE TABLE IF NOT EXISTS _amux_context_snapshots (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    assignment_key TEXT NOT NULL UNIQUE,
    task_id        TEXT NOT NULL,             -- tsk_<ULID>
    worker_id      TEXT NOT NULL,             -- wrk_<ULID>
    content_hash   TEXT NOT NULL,             -- sha256 hex over canonical sorted fragments
    fragments      TEXT NOT NULL,             -- JSON array of ContextFragment
    at             TEXT NOT NULL              -- RFC3339
);
CREATE INDEX IF NOT EXISTS idx_amux_context_snapshots_task
    ON _amux_context_snapshots(task_id);
CREATE INDEX IF NOT EXISTS idx_amux_context_snapshots_worker
    ON _amux_context_snapshots(worker_id);
