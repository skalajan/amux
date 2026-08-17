-- 0009: durable media-prepare job state (AMUX-2598 file-viewer cutover).
--
-- Python tracked /api/file/prepare remux jobs in process memory
-- (_MEDIA_PREP_JOBS, amux-server.py:64540) — a server restart orphaned every
-- in-flight job invisibly. The native port keeps job state in the shared DB
-- so a restart is DETECTABLE: a 'running' row whose updated_at heartbeat has
-- gone stale is an orphan, and the poll endpoint restarts it instead of
-- reporting a progress number nobody is advancing (ethos rule 4: the wrong
-- state must be visible from the data kept).
--
-- _amux_ prefix: rust-owned table, invisible to the python server's schema.
-- Additive only (Phase 11 rollback: python must keep opening this DB).
--
-- key         sha1("<path>|<mtime>|<size>")[:24] — the SAME derivation python
--             uses, so prepared copies already in ~/.amux/media-cache keep
--             being found after cutover.
-- status      running | done | error
-- progress    0..100 (caps at 99 until the faststart moov pass finishes)
-- pid         the ffmpeg child, for post-restart forensics
-- updated_at  heartbeat (unix seconds); running + stale heartbeat = orphan
CREATE TABLE IF NOT EXISTS _amux_media_jobs (
    key        TEXT PRIMARY KEY,
    src_path   TEXT NOT NULL,
    out_path   TEXT NOT NULL,
    status     TEXT NOT NULL DEFAULT 'running',
    progress   REAL NOT NULL DEFAULT 0,
    error      TEXT NOT NULL DEFAULT '',
    pid        INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
