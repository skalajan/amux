-- 0011_conversations.sql — per-worker provider conversation refs
-- (AMUX-2613 gap 2: conversation continuity for headless structured turns).
--
-- One row per worker: the provider-native conversation identifier its LAST
-- structured run reported (claude `session_id`, codex `thread_id`, gemini
-- `session_id`), captured from the stream by the protocol reader and passed
-- back on the next spawn (`claude --resume <id>` / `codex exec resume <id>`)
-- so successive headless turns share memory instead of each being a fresh
-- fork+exec. Kept OUT of `_amux_workers` deliberately: the worker snapshot
-- feeds the replay journal (RR-0111a), and widening it would invalidate
-- every payload already journaled. Additive, `_amux_`-prefixed, invisible
-- to the Python server (Phase 11 rollback holds).
--
-- `provider` is stored alongside the ref because a conversation id is only
-- meaningful to the CLI that minted it: after a worker's provider changes,
-- the stored ref must NOT be replayed into the new provider — readers
-- filter on provider matching the worker row's current value.

CREATE TABLE IF NOT EXISTS _amux_conversations (
    worker_id        TEXT PRIMARY KEY,             -- wrk_<ULID>
    provider         TEXT NOT NULL,                -- worker-row provider spelling at capture time
    conversation_ref TEXT NOT NULL,                -- provider-native session/thread id
    updated_at       TEXT NOT NULL                 -- RFC3339
);
