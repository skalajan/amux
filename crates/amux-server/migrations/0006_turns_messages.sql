-- 0006_turns_messages.sql — Turn ledger + durable messages (Phase 4:
-- RR-0065 turn tracking, RR-0066 message delivery, Invariants 6/11/29).
-- New `_amux_` tables, invisible to the Python server — additive only
-- (Phase 11 rollback holds). Planned as 0005; a concurrently-landed
-- memories migration took that slot, so this is 0006 — numbers are
-- append-only, never reshuffled.

-- One row per worker turn (Invariant 6: the turn is a first-class entity,
-- not a heuristic). Written by the event processor (orchestrator/events.rs)
-- from TurnStarted/TurnCompleted WorkerEvents.
--
-- session_id is NULLABLE on purpose: a TurnStarted observed while no live
-- `_amux_sessions` row exists is still a FACT and gets recorded — dropping
-- it would make the session-bookkeeping gap undiagnosable from the data we
-- keep (ethos rule 4). task_id is NULL for taskless turns (steering
-- chatter, compaction passes).
--
-- `outcome` is JSON `{"outcome": "<raw outcome line>"}` — the raw string
-- from TurnResult, never re-normalized into an enum the event did not
-- state. `tokens` is JSON `{"reported_total": N}` accumulated from
-- Progress events' tokens_used ("tokens so far this turn"); `{}` = the
-- provider never reported usage, which is distinct from reporting 0.
CREATE TABLE IF NOT EXISTS _amux_turns (
    id          TEXT PRIMARY KEY,             -- trn_<ULID>
    session_id  TEXT,                          -- ses_<ULID>, NULL if none live
    worker_id   TEXT NOT NULL,
    task_id     TEXT,                          -- NULL for taskless turns
    started_at  TEXT NOT NULL,                 -- RFC3339
    ended_at    TEXT,                          -- NULL while running
    outcome     TEXT,                          -- JSON, NULL while running
    tokens      TEXT NOT NULL DEFAULT '{}'     -- JSON {"reported_total": N}
);
CREATE INDEX IF NOT EXISTS idx_amux_turns_worker ON _amux_turns(worker_id, started_at);
CREATE INDEX IF NOT EXISTS idx_amux_turns_session ON _amux_turns(session_id);

-- Durable messages (Invariant 29): steering text lives HERE, never in the
-- command — `WorkerCommand::DeliverMessage(MessageId)` carries only the
-- reference, so a restart cannot lose pending steering text (the Python
-- failure mode this schema exists to kill). Columns mirror
-- amux_core::message::Message; `thread` links fan-out children and replies
-- to their parent.
CREATE TABLE IF NOT EXISTS _amux_messages (
    id          TEXT PRIMARY KEY,             -- msg_<ULID>
    from_actor  TEXT NOT NULL,                 -- JSON Actor
    target      TEXT NOT NULL,                 -- JSON MessageTarget
    body        TEXT NOT NULL,
    thread      TEXT,                          -- parent msg_<ULID>, NULL = root
    created_at  TEXT NOT NULL,                 -- RFC3339
    delivery    TEXT NOT NULL                  -- JSON DeliveryState
);
CREATE INDEX IF NOT EXISTS idx_amux_messages_thread ON _amux_messages(thread);
CREATE INDEX IF NOT EXISTS idx_amux_messages_created ON _amux_messages(created_at);
