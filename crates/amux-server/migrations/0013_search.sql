-- RR-0110 — universal search (FTS5), Invariant 32.
--
-- Net-new capability: neither the Python server nor the SPA ever had an
-- /api/search. This migration creates the index AND the mechanism that keeps
-- it current, in one place, plus the backfill of everything already in the DB.
--
-- WHY TRIGGERS AND NOT THE WRITER PATH
-- -----------------------------------
-- The obvious seam is `PendingEvent` in db/mod.rs: every Rust mutation that
-- journals a StateEvent could index in the same breath. That was rejected for
-- one reason — it only covers writers that REMEMBER to journal. A search index
-- maintained by a code path is exactly as complete as the set of call sites
-- someone thought of, and the failure is silent (a card exists, search says it
-- does not). SQLite triggers are the mechanism no writer can bypass: the Rust
-- writer, a future write site nobody has written yet, a `sqlite3` shell, or a
-- restored backup all go through them. That is ethos rule 1 applied to an
-- index — "who is enrolled by default?" — and the answer here is every write.
--
-- Drift is still detectable rather than assumed away: `GET /api/search/status`
-- compares per-type index counts against the live source-table counts and says
-- so when they disagree (ethos rule 4 — a wrong answer must be visible from
-- the data we keep). `POST /api/search/reindex` rebuilds and returns the
-- before/after counts.
--
-- SHAPE
-- -----
-- `search_docs` is the content table (one row per indexed entity, carrying the
-- provenance the plan asks a SearchHit to have: entity_type, scope, task_id,
-- worker_id, timestamp) and `search_fts` is an external-content FTS5 index over
-- its title/body. External content means the text is stored once, and
-- snippet()/highlight() still work.
--
-- Additive only (Phase 11 rollback rule): new tables and new triggers, no
-- change to any existing table.

CREATE TABLE IF NOT EXISTS search_docs (
    -- INTEGER PRIMARY KEY == rowid, which is what the FTS index keys on.
    rowid_      INTEGER PRIMARY KEY,
    -- '<entity_type>:<entity_id>', the upsert key.
    doc_id      TEXT NOT NULL UNIQUE,
    entity_type TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    title       TEXT NOT NULL DEFAULT '',
    body        TEXT NOT NULL DEFAULT '',
    -- Provenance chips (plan: "SearchHit provenance (entity_type, scope,
    -- task_id, worker_id, timestamp)").
    scope       TEXT,
    task_id     TEXT,
    worker_id   TEXT,
    -- Where the SPA should navigate for this hit.
    link        TEXT NOT NULL DEFAULT '',
    -- Small JSON of render-relevant fields (status, archived, type, …). Kept
    -- so a hit can be rendered and FILTERED by the client without a second
    -- fetch — notably `archived`, which is information the index must carry
    -- rather than silently drop (ethos rule 1's archived-filter trap).
    meta        TEXT NOT NULL DEFAULT '{}',
    updated_at  INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_search_docs_type    ON search_docs(entity_type);
CREATE INDEX IF NOT EXISTS idx_search_docs_updated ON search_docs(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_search_docs_entity  ON search_docs(entity_type, entity_id);

CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(
    title,
    body,
    content='search_docs',
    content_rowid='rowid_',
    tokenize='unicode61 remove_diacritics 2'
);

-- External-content sync triggers (the shape from the SQLite FTS5 docs). The
-- 'delete' command is mandatory for external content: an ordinary DELETE
-- cannot remove index entries because the index does not own the text.
CREATE TRIGGER IF NOT EXISTS search_docs_ai AFTER INSERT ON search_docs BEGIN
    INSERT INTO search_fts(rowid, title, body) VALUES (new.rowid_, new.title, new.body);
END;
CREATE TRIGGER IF NOT EXISTS search_docs_ad AFTER DELETE ON search_docs BEGIN
    INSERT INTO search_fts(search_fts, rowid, title, body)
        VALUES('delete', old.rowid_, old.title, old.body);
END;
CREATE TRIGGER IF NOT EXISTS search_docs_au AFTER UPDATE ON search_docs BEGIN
    INSERT INTO search_fts(search_fts, rowid, title, body)
        VALUES('delete', old.rowid_, old.title, old.body);
    INSERT INTO search_fts(rowid, title, body) VALUES (new.rowid_, new.title, new.body);
END;

-- ---------------------------------------------------------------------------
-- Source-table triggers. Each family gets insert / update / update-that-deletes
-- / delete. The upsert is `ON CONFLICT(doc_id) DO UPDATE` rather than
-- `INSERT OR REPLACE` deliberately: REPLACE does not fire the search_docs
-- DELETE trigger unless PRAGMA recursive_triggers is on, so a REPLACE-based
-- upsert would leave stale rows in the FTS index forever.
-- ---------------------------------------------------------------------------

-- ---- board cards (`issues`) — title, desc AND the card LOG -----------------
-- The log is a plain TEXT column on `issues`; a term that appears only in a
-- card's history has to be findable, which is why body concatenates it.
CREATE TRIGGER IF NOT EXISTS search_issues_ai AFTER INSERT ON issues
WHEN new.deleted IS NULL BEGIN
    INSERT INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
    VALUES ('task:'||new.id, 'task', new.id, new.title,
            new.desc || char(10) || COALESCE(new.log,''),
            new.session, new.id, NULL, '#board/'||new.id,
            json_object('status', new.status, 'archived', new.archived, 'type', new.type, 'session', new.session),
            new.updated)
    ON CONFLICT(doc_id) DO UPDATE SET
        title=excluded.title, body=excluded.body, scope=excluded.scope,
        task_id=excluded.task_id, link=excluded.link, meta=excluded.meta,
        updated_at=excluded.updated_at;
END;

CREATE TRIGGER IF NOT EXISTS search_issues_au AFTER UPDATE ON issues
WHEN new.deleted IS NULL BEGIN
    INSERT INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
    VALUES ('task:'||new.id, 'task', new.id, new.title,
            new.desc || char(10) || COALESCE(new.log,''),
            new.session, new.id, NULL, '#board/'||new.id,
            json_object('status', new.status, 'archived', new.archived, 'type', new.type, 'session', new.session),
            new.updated)
    ON CONFLICT(doc_id) DO UPDATE SET
        title=excluded.title, body=excluded.body, scope=excluded.scope,
        task_id=excluded.task_id, link=excluded.link, meta=excluded.meta,
        updated_at=excluded.updated_at;
END;

-- Soft delete is the real delete on this table; the row survives, the doc must not.
CREATE TRIGGER IF NOT EXISTS search_issues_au_del AFTER UPDATE ON issues
WHEN new.deleted IS NOT NULL BEGIN
    DELETE FROM search_docs WHERE doc_id = 'task:'||new.id;
END;

CREATE TRIGGER IF NOT EXISTS search_issues_ad AFTER DELETE ON issues BEGIN
    DELETE FROM search_docs WHERE doc_id = 'task:'||old.id;
END;

-- ---- schedules ------------------------------------------------------------
CREATE TRIGGER IF NOT EXISTS search_schedules_ai AFTER INSERT ON schedules
WHEN new.deleted IS NULL BEGIN
    INSERT INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
    VALUES ('schedule:'||new.id, 'schedule', new.id, new.title,
            new.command || char(10) || COALESCE(new.schedule_expr,'') || ' ' || COALESCE(new.recurrence,''),
            new.session, NULL, NULL, '#schedules',
            json_object('enabled', new.enabled, 'kind', new.kind, 'session', new.session),
            new.updated)
    ON CONFLICT(doc_id) DO UPDATE SET
        title=excluded.title, body=excluded.body, scope=excluded.scope,
        link=excluded.link, meta=excluded.meta, updated_at=excluded.updated_at;
END;

CREATE TRIGGER IF NOT EXISTS search_schedules_au AFTER UPDATE ON schedules
WHEN new.deleted IS NULL BEGIN
    INSERT INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
    VALUES ('schedule:'||new.id, 'schedule', new.id, new.title,
            new.command || char(10) || COALESCE(new.schedule_expr,'') || ' ' || COALESCE(new.recurrence,''),
            new.session, NULL, NULL, '#schedules',
            json_object('enabled', new.enabled, 'kind', new.kind, 'session', new.session),
            new.updated)
    ON CONFLICT(doc_id) DO UPDATE SET
        title=excluded.title, body=excluded.body, scope=excluded.scope,
        link=excluded.link, meta=excluded.meta, updated_at=excluded.updated_at;
END;

CREATE TRIGGER IF NOT EXISTS search_schedules_au_del AFTER UPDATE ON schedules
WHEN new.deleted IS NOT NULL BEGIN
    DELETE FROM search_docs WHERE doc_id = 'schedule:'||new.id;
END;

CREATE TRIGGER IF NOT EXISTS search_schedules_ad AFTER DELETE ON schedules BEGIN
    DELETE FROM search_docs WHERE doc_id = 'schedule:'||old.id;
END;

-- ---- journal entries ------------------------------------------------------
CREATE TRIGGER IF NOT EXISTS search_journal_ai AFTER INSERT ON journal_entries
WHEN new.deleted IS NULL BEGIN
    INSERT INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
    VALUES ('journal:'||new.id, 'journal', new.id,
            new.date || CASE WHEN new.place_name <> '' THEN ' · '||new.place_name ELSE '' END,
            new.text || char(10) || new.tags || ' ' || new.place_name,
            NULL, NULL, NULL, '#journal',
            json_object('date', new.date, 'starred', new.starred, 'tags', new.tags),
            new.updated)
    ON CONFLICT(doc_id) DO UPDATE SET
        title=excluded.title, body=excluded.body, link=excluded.link,
        meta=excluded.meta, updated_at=excluded.updated_at;
END;

CREATE TRIGGER IF NOT EXISTS search_journal_au AFTER UPDATE ON journal_entries
WHEN new.deleted IS NULL BEGIN
    INSERT INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
    VALUES ('journal:'||new.id, 'journal', new.id,
            new.date || CASE WHEN new.place_name <> '' THEN ' · '||new.place_name ELSE '' END,
            new.text || char(10) || new.tags || ' ' || new.place_name,
            NULL, NULL, NULL, '#journal',
            json_object('date', new.date, 'starred', new.starred, 'tags', new.tags),
            new.updated)
    ON CONFLICT(doc_id) DO UPDATE SET
        title=excluded.title, body=excluded.body, link=excluded.link,
        meta=excluded.meta, updated_at=excluded.updated_at;
END;

CREATE TRIGGER IF NOT EXISTS search_journal_au_del AFTER UPDATE ON journal_entries
WHEN new.deleted IS NOT NULL BEGIN
    DELETE FROM search_docs WHERE doc_id = 'journal:'||new.id;
END;

CREATE TRIGGER IF NOT EXISTS search_journal_ad AFTER DELETE ON journal_entries BEGIN
    DELETE FROM search_docs WHERE doc_id = 'journal:'||old.id;
END;

-- ---- memories (`_amux_memories`) ------------------------------------------
-- updated_at is RFC3339 text here; strftime('%s', …) parses it. A value it
-- cannot parse yields NULL, and COALESCE lands it at 0 (sorted last) rather
-- than failing the write — a memory that sorts badly still has to be findable.
CREATE TRIGGER IF NOT EXISTS search_memories_ai AFTER INSERT ON _amux_memories
WHEN new.deleted_at IS NULL BEGIN
    INSERT INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
    VALUES ('memory:'||new.id, 'memory', new.id, new.name, new.content,
            new.scope, NULL, json_extract(new.scope, '$.id'), '#memories',
            json_object('memory_type', new.memory_type, 'version', new.version),
            COALESCE(CAST(strftime('%s', new.updated_at) AS INTEGER), 0))
    ON CONFLICT(doc_id) DO UPDATE SET
        title=excluded.title, body=excluded.body, scope=excluded.scope,
        worker_id=excluded.worker_id, link=excluded.link, meta=excluded.meta,
        updated_at=excluded.updated_at;
END;

CREATE TRIGGER IF NOT EXISTS search_memories_au AFTER UPDATE ON _amux_memories
WHEN new.deleted_at IS NULL BEGIN
    INSERT INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
    VALUES ('memory:'||new.id, 'memory', new.id, new.name, new.content,
            new.scope, NULL, json_extract(new.scope, '$.id'), '#memories',
            json_object('memory_type', new.memory_type, 'version', new.version),
            COALESCE(CAST(strftime('%s', new.updated_at) AS INTEGER), 0))
    ON CONFLICT(doc_id) DO UPDATE SET
        title=excluded.title, body=excluded.body, scope=excluded.scope,
        worker_id=excluded.worker_id, link=excluded.link, meta=excluded.meta,
        updated_at=excluded.updated_at;
END;

CREATE TRIGGER IF NOT EXISTS search_memories_au_del AFTER UPDATE ON _amux_memories
WHEN new.deleted_at IS NOT NULL BEGIN
    DELETE FROM search_docs WHERE doc_id = 'memory:'||new.id;
END;

CREATE TRIGGER IF NOT EXISTS search_memories_ad AFTER DELETE ON _amux_memories BEGIN
    DELETE FROM search_docs WHERE doc_id = 'memory:'||old.id;
END;

-- ---- messages (`_amux_messages`) ------------------------------------------
-- Messages are append-only (delivery state changes, the text never does), so
-- the update trigger only needs to keep the body in step for safety.
CREATE TRIGGER IF NOT EXISTS search_messages_ai AFTER INSERT ON _amux_messages BEGIN
    INSERT INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
    VALUES ('message:'||new.id, 'message', new.id, substr(new.body, 1, 80), new.body,
            json_extract(new.from_actor, '$.id'), NULL, json_extract(new.target, '$.id'), '#messages',
            json_object('thread', new.thread, 'from', new.from_actor, 'target', new.target),
            COALESCE(CAST(strftime('%s', new.created_at) AS INTEGER), 0))
    ON CONFLICT(doc_id) DO UPDATE SET
        title=excluded.title, body=excluded.body, meta=excluded.meta;
END;

CREATE TRIGGER IF NOT EXISTS search_messages_ad AFTER DELETE ON _amux_messages BEGIN
    DELETE FROM search_docs WHERE doc_id = 'message:'||old.id;
END;

-- ---- workers (`_amux_workers`) — session/worker metadata ------------------
CREATE TRIGGER IF NOT EXISTS search_workers_ai AFTER INSERT ON _amux_workers BEGIN
    INSERT INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
    VALUES ('worker:'||new.id, 'worker', new.id, new.display_name,
            new.display_name || char(10) || new.cwd || ' ' || new.provider || ' '
                || COALESCE(new.model,'') || ' ' || new.backend || ' ' || new.name_aliases,
            new.group_id, NULL, new.id, '#workers/'||new.id,
            json_object('provider', new.provider, 'backend', new.backend, 'model', new.model, 'group_id', new.group_id),
            COALESCE(CAST(strftime('%s', new.updated_at) AS INTEGER), 0))
    ON CONFLICT(doc_id) DO UPDATE SET
        title=excluded.title, body=excluded.body, scope=excluded.scope,
        worker_id=excluded.worker_id, link=excluded.link, meta=excluded.meta,
        updated_at=excluded.updated_at;
END;

CREATE TRIGGER IF NOT EXISTS search_workers_au AFTER UPDATE ON _amux_workers BEGIN
    INSERT INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
    VALUES ('worker:'||new.id, 'worker', new.id, new.display_name,
            new.display_name || char(10) || new.cwd || ' ' || new.provider || ' '
                || COALESCE(new.model,'') || ' ' || new.backend || ' ' || new.name_aliases,
            new.group_id, NULL, new.id, '#workers/'||new.id,
            json_object('provider', new.provider, 'backend', new.backend, 'model', new.model, 'group_id', new.group_id),
            COALESCE(CAST(strftime('%s', new.updated_at) AS INTEGER), 0))
    ON CONFLICT(doc_id) DO UPDATE SET
        title=excluded.title, body=excluded.body, scope=excluded.scope,
        worker_id=excluded.worker_id, link=excluded.link, meta=excluded.meta,
        updated_at=excluded.updated_at;
END;

CREATE TRIGGER IF NOT EXISTS search_workers_ad AFTER DELETE ON _amux_workers BEGIN
    DELETE FROM search_docs WHERE doc_id = 'worker:'||old.id;
END;

-- ---------------------------------------------------------------------------
-- Backfill. `INSERT OR IGNORE` so re-running is safe; the search_docs AFTER
-- INSERT trigger carries each row into the FTS index. Counts are NOT printed
-- from SQL (a migration cannot report) — `GET /api/search/status` is the
-- report, and it compares these numbers against the live tables so a partial
-- backfill announces itself instead of looking like an empty corpus.
-- ---------------------------------------------------------------------------

INSERT OR IGNORE INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
SELECT 'task:'||id, 'task', id, title, desc || char(10) || COALESCE(log,''),
       session, id, NULL, '#board/'||id,
       json_object('status', status, 'archived', archived, 'type', type, 'session', session),
       updated
FROM issues WHERE deleted IS NULL;

INSERT OR IGNORE INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
SELECT 'schedule:'||id, 'schedule', id, title,
       command || char(10) || COALESCE(schedule_expr,'') || ' ' || COALESCE(recurrence,''),
       session, NULL, NULL, '#schedules',
       json_object('enabled', enabled, 'kind', kind, 'session', session),
       updated
FROM schedules WHERE deleted IS NULL;

INSERT OR IGNORE INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
SELECT 'journal:'||id, 'journal', id,
       date || CASE WHEN place_name <> '' THEN ' · '||place_name ELSE '' END,
       text || char(10) || tags || ' ' || place_name,
       NULL, NULL, NULL, '#journal',
       json_object('date', date, 'starred', starred, 'tags', tags),
       updated
FROM journal_entries WHERE deleted IS NULL;

INSERT OR IGNORE INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
SELECT 'memory:'||id, 'memory', id, name, content,
       scope, NULL, json_extract(scope, '$.id'), '#memories',
       json_object('memory_type', memory_type, 'version', version),
       COALESCE(CAST(strftime('%s', updated_at) AS INTEGER), 0)
FROM _amux_memories WHERE deleted_at IS NULL;

INSERT OR IGNORE INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
SELECT 'message:'||id, 'message', id, substr(body, 1, 80), body,
       json_extract(from_actor, '$.id'), NULL, json_extract(target, '$.id'), '#messages',
       json_object('thread', thread, 'from', from_actor, 'target', target),
       COALESCE(CAST(strftime('%s', created_at) AS INTEGER), 0)
FROM _amux_messages;

INSERT OR IGNORE INTO search_docs (doc_id, entity_type, entity_id, title, body, scope, task_id, worker_id, link, meta, updated_at)
SELECT 'worker:'||id, 'worker', id, display_name,
       display_name || char(10) || cwd || ' ' || provider || ' '
           || COALESCE(model,'') || ' ' || backend || ' ' || name_aliases,
       group_id, NULL, id, '#workers/'||id,
       json_object('provider', provider, 'backend', backend, 'model', model, 'group_id', group_id),
       COALESCE(CAST(strftime('%s', updated_at) AS INTEGER), 0)
FROM _amux_workers;
