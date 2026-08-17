-- 0001_baseline.sql — EXACT mirror of the live amux.db schema as of 2026-08-09.
--
-- Source: sqlite_master of ~/.amux/amux.db (read-only .backup snapshot), i.e. the
-- Python server's _DB_SCHEMA (amux-server.py:9809) PLUS its runtime ALTER TABLE
-- column migrations, captured in their live post-migration shape (appended columns
-- appear at the end of their CREATE TABLE, exactly as SQLite recorded them).
--
-- DO NOT "improve" anything here — Phase 11 requires the Rust server to open the
-- SAME DB file the Python server uses, in both directions. Schema improvements go
-- in 0002_rust_additions.sql. Every statement is idempotent (IF NOT EXISTS) so
-- applying this to the live, already-populated DB is a no-op.
--
-- Not included (SQLite-internal, auto-managed): sqlite_sequence, sqlite_stat1,
-- sqlite_stat4. Users may also own ad-hoc `wb_*` workbench tables — tolerate them.
--
-- Runtime pragmas the Python server sets (connection-level, not schema, listed for
-- parity): journal_mode=WAL.

CREATE TABLE IF NOT EXISTS cal_events (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    start       TEXT NOT NULL,          -- ISO 8601; date-only => all-day
    end         TEXT,                   -- ISO 8601; optional
    all_day     INTEGER NOT NULL DEFAULT 0,
    location    TEXT,
    description TEXT,
    rrule       TEXT,                   -- optional RFC 5545 RRULE (without the "RRULE:" prefix)
    created     INTEGER NOT NULL,
    updated     INTEGER NOT NULL,
    deleted     INTEGER
);
CREATE TABLE IF NOT EXISTS cmd_history (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    text     TEXT NOT NULL,
    type     TEXT NOT NULL DEFAULT 'direct',
    session  TEXT NOT NULL DEFAULT '',
    ts       INTEGER NOT NULL
, origin TEXT NOT NULL DEFAULT '', card_id TEXT);
CREATE TABLE IF NOT EXISTS crm_contacts (
    id       TEXT PRIMARY KEY,
    name     TEXT NOT NULL,
    company  TEXT NOT NULL DEFAULT '',
    role     TEXT NOT NULL DEFAULT '',
    email    TEXT NOT NULL DEFAULT '',
    linkedin TEXT NOT NULL DEFAULT '',
    twitter  TEXT NOT NULL DEFAULT '',
    phone    TEXT NOT NULL DEFAULT '',
    notes    TEXT NOT NULL DEFAULT '',
    created  INTEGER NOT NULL,
    updated  INTEGER NOT NULL,
    deleted  INTEGER
);
CREATE TABLE IF NOT EXISTS crm_interactions (
    id             TEXT PRIMARY KEY,
    contact_id     TEXT NOT NULL,
    date           TEXT NOT NULL,
    type           TEXT NOT NULL DEFAULT 'other',
    notes          TEXT NOT NULL DEFAULT '',
    follow_up_date TEXT,
    follow_up_note TEXT NOT NULL DEFAULT '',
    created        INTEGER NOT NULL,
    updated        INTEGER NOT NULL,
    FOREIGN KEY (contact_id) REFERENCES crm_contacts(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS crm_tags (
    contact_id TEXT NOT NULL,
    tag        TEXT NOT NULL,
    PRIMARY KEY (contact_id, tag),
    FOREIGN KEY (contact_id) REFERENCES crm_contacts(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS dictation_dict (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    word     TEXT NOT NULL,
    correct  TEXT NOT NULL DEFAULT '',
    created  INTEGER NOT NULL,
    UNIQUE(word, correct)
);
CREATE TABLE IF NOT EXISTS dictation_history (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    session   TEXT NOT NULL DEFAULT '',
    ts        INTEGER NOT NULL,
    text      TEXT NOT NULL,
    raw_text  TEXT NOT NULL DEFAULT '',
    prev_text TEXT NOT NULL DEFAULT '',   -- pre-AI-edit copy, for "Undo AI edit"
    ai_edited INTEGER NOT NULL DEFAULT 0,
    words     INTEGER NOT NULL DEFAULT 0,
    dur_ms    INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS email_accounts (
    id           TEXT PRIMARY KEY,
    email        TEXT NOT NULL UNIQUE,
    access_token TEXT,
    refresh_token TEXT,
    token_expiry INTEGER,
    calendar_id  TEXT NOT NULL DEFAULT 'primary',
    last_synced  INTEGER,
    created      INTEGER NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS email_events (
    id               TEXT PRIMARY KEY,
    account_id       TEXT NOT NULL,
    gmail_message_id TEXT NOT NULL,
    gmail_thread_id  TEXT,
    email_subject    TEXT,
    email_from       TEXT,
    email_date       TEXT,
    event_title      TEXT,
    event_start      TEXT,
    event_end        TEXT,
    event_location   TEXT,
    event_description TEXT,
    calendar_event_id TEXT,
    status           TEXT NOT NULL DEFAULT 'pending',
    raw_extract      TEXT,
    created          INTEGER NOT NULL,
    UNIQUE(account_id, gmail_message_id)
);
CREATE TABLE IF NOT EXISTS graph_edges (
    id          TEXT PRIMARY KEY,
    graph_id    TEXT NOT NULL DEFAULT 'default',
    source      TEXT NOT NULL,
    target      TEXT NOT NULL,
    label       TEXT NOT NULL DEFAULT '',
    created     INTEGER NOT NULL,
    FOREIGN KEY (source) REFERENCES graph_nodes(id) ON DELETE CASCADE,
    FOREIGN KEY (target) REFERENCES graph_nodes(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS graph_nodes (
    id          TEXT PRIMARY KEY,
    graph_id    TEXT NOT NULL DEFAULT 'default',
    label       TEXT NOT NULL,
    body        TEXT NOT NULL DEFAULT '',
    color       TEXT NOT NULL DEFAULT '#ffffff',
    folder      TEXT NOT NULL DEFAULT '',
    x           REAL,
    y           REAL,
    pinned      INTEGER NOT NULL DEFAULT 0,
    created     INTEGER NOT NULL,
    updated     INTEGER NOT NULL
, source_path TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS group_config (
    name       TEXT PRIMARY KEY,
    department TEXT NOT NULL DEFAULT '',
    goal       TEXT NOT NULL DEFAULT '',
    kpis       TEXT NOT NULL DEFAULT '[]',
    human_cost INTEGER NOT NULL DEFAULT 0,
    updated    INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS interaction_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    actor       TEXT NOT NULL DEFAULT '',
    target      TEXT NOT NULL DEFAULT '',
    action      TEXT NOT NULL DEFAULT '',
    url         TEXT NOT NULL DEFAULT '',
    detail      TEXT NOT NULL DEFAULT '',
    before      TEXT NOT NULL DEFAULT '',
    result      TEXT NOT NULL DEFAULT '',
    ok          INTEGER NOT NULL DEFAULT 1,
    ms          INTEGER NOT NULL DEFAULT 0,
    seq         INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS issue_counters (
    prefix      TEXT PRIMARY KEY,
    next_n      INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS issue_files (
    issue_id    TEXT NOT NULL,
    path        TEXT NOT NULL,
    added_by    TEXT,
    added_at    INTEGER,
    note        TEXT,
    PRIMARY KEY (issue_id, path),
    FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS issue_tags (
    issue_id    TEXT NOT NULL,
    tag         TEXT NOT NULL, added_at INTEGER,
    PRIMARY KEY (issue_id, tag),
    FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS issues (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    desc        TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'todo',
    session     TEXT,
    creator     TEXT NOT NULL DEFAULT '',
    due         TEXT,
    created     INTEGER NOT NULL,
    updated     INTEGER NOT NULL,
    deleted     INTEGER
, owner_type TEXT NOT NULL DEFAULT 'human', due_time TEXT, pinned INTEGER NOT NULL DEFAULT 0, gcal_event_id TEXT, pos REAL NOT NULL DEFAULT 0, notified INTEGER NOT NULL DEFAULT 0, gate TEXT, shepherd TEXT, type TEXT NOT NULL DEFAULT 'code', archived INTEGER NOT NULL DEFAULT 0, depends_on TEXT, reviewer TEXT, log TEXT, rev INTEGER NOT NULL DEFAULT 0, source_ref TEXT, last_verified_at INTEGER);
CREATE TABLE IF NOT EXISTS journal_entries (
    id          TEXT PRIMARY KEY,
    text        TEXT NOT NULL DEFAULT '',
    date        TEXT NOT NULL,
    created     INTEGER NOT NULL,
    updated     INTEGER NOT NULL,
    lat         REAL,
    lng         REAL,
    place_name  TEXT NOT NULL DEFAULT '',
    starred     INTEGER NOT NULL DEFAULT 0,
    tags        TEXT NOT NULL DEFAULT '',
    prompt1     TEXT NOT NULL DEFAULT '',
    prompt2     TEXT NOT NULL DEFAULT '',
    prompt3     TEXT NOT NULL DEFAULT '',
    deleted     INTEGER
);
CREATE TABLE IF NOT EXISTS journal_media (
    id          TEXT PRIMARY KEY,
    entry_id    TEXT NOT NULL,
    filename    TEXT NOT NULL,
    mime        TEXT NOT NULL DEFAULT 'image/jpeg',
    position    INTEGER NOT NULL DEFAULT 0,
    created     INTEGER NOT NULL,
    FOREIGN KEY (entry_id) REFERENCES journal_entries(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS layout_presets (
    name       TEXT PRIMARY KEY,
    hidden     TEXT NOT NULL DEFAULT '[]',
    tab_order  TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ledger_cursor (
    conversation TEXT PRIMARY KEY,
    offset       INTEGER NOT NULL DEFAULT 0,       -- bytes of the JSONL already ledgered
    mtime        INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS logs (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    ts       INTEGER NOT NULL,
    category TEXT NOT NULL DEFAULT 'system',
    action   TEXT NOT NULL,
    session  TEXT,
    actor    TEXT,
    detail   TEXT,
    level    TEXT NOT NULL DEFAULT 'info'
);
CREATE TABLE IF NOT EXISTS org (
    id         TEXT PRIMARY KEY DEFAULT 'default',
    name       TEXT NOT NULL DEFAULT 'My Workspace',
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS org_invites (
    token      TEXT PRIMARY KEY,
    email      TEXT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER,
    used_by    TEXT
);
CREATE TABLE IF NOT EXISTS org_members (
    id         TEXT PRIMARY KEY,
    email      TEXT NOT NULL UNIQUE,
    name       TEXT,
    role       TEXT NOT NULL DEFAULT 'member',
    joined_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS owner_alerts (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    ts       INTEGER NOT NULL,
    origin   TEXT NOT NULL DEFAULT '',   -- server-verified X-Amux-Session (authoritative)
    claimed  TEXT NOT NULL DEFAULT '',   -- self-reported body 'session' (mismatch = provenance red flag)
    message  TEXT NOT NULL,
    reason   TEXT NOT NULL DEFAULT '',
    channels TEXT NOT NULL DEFAULT '',   -- JSON of the delivery result (push/sms)
    deduped  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS prefs (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS proxies (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    port         INTEGER NOT NULL,
    scheme       TEXT NOT NULL DEFAULT 'http',
    created_at   INTEGER NOT NULL,
    last_started INTEGER
);
CREATE TABLE IF NOT EXISTS push_subscriptions (
    endpoint TEXT PRIMARY KEY,
    p256dh   TEXT NOT NULL,
    auth     TEXT NOT NULL,
    ua       TEXT,
    created  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS reports (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    type         TEXT NOT NULL DEFAULT 'infra-spend',
    config       TEXT NOT NULL DEFAULT '{}',
    position     INTEGER NOT NULL DEFAULT 0,
    created      INTEGER NOT NULL,
    last_refresh INTEGER,
    cached_data  TEXT
);
CREATE TABLE IF NOT EXISTS saved_messages (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    label    TEXT NOT NULL DEFAULT '',
    text     TEXT NOT NULL,
    created  INTEGER NOT NULL DEFAULT 0,
    pos      REAL NOT NULL DEFAULT 0
, session TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS schedule_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    schedule_id TEXT NOT NULL,
    ts          INTEGER NOT NULL,
    field       TEXT NOT NULL,
    old_value   TEXT,
    new_value   TEXT,
    source      TEXT,
    by_who      TEXT
);
CREATE TABLE IF NOT EXISTS schedule_runs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    schedule_id TEXT NOT NULL,
    ran_at      INTEGER NOT NULL,
    status      TEXT NOT NULL DEFAULT 'ok',
    note        TEXT
, source TEXT NOT NULL DEFAULT 'cron');
CREATE TABLE IF NOT EXISTS schedules (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    session     TEXT NOT NULL,
    command     TEXT NOT NULL,
    sched_type  TEXT NOT NULL DEFAULT 'once',
    recurrence  TEXT,
    run_at      TEXT,
    next_run    TEXT,
    last_run    TEXT,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created     INTEGER NOT NULL,
    updated     INTEGER NOT NULL,
    deleted     INTEGER
, run_count INTEGER NOT NULL DEFAULT 0, schedule_expr TEXT, watch INTEGER NOT NULL DEFAULT 0, watch_timeout INTEGER NOT NULL DEFAULT 120, done_pattern TEXT, done_action TEXT NOT NULL DEFAULT 'disable', kind TEXT NOT NULL DEFAULT 'tmux', trigger_on TEXT, trigger_cooldown INTEGER NOT NULL DEFAULT 120, trigger_sessions TEXT, gcal_event_id TEXT, exit_actions TEXT);
CREATE TABLE IF NOT EXISTS send_dedup (
    session TEXT NOT NULL,
    msg_id  TEXT NOT NULL,
    ts      INTEGER NOT NULL,
    PRIMARY KEY (session, msg_id)
);
CREATE TABLE IF NOT EXISTS session_events (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    ts      REAL NOT NULL,
    session TEXT NOT NULL DEFAULT '',
    type    TEXT NOT NULL,
    data    TEXT,
    idem    TEXT,
    source  TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS session_gates (
    session TEXT NOT NULL,
    status  TEXT NOT NULL,
    gate    TEXT,
    PRIMARY KEY (session, status)
);
CREATE TABLE IF NOT EXISTS share_tokens (
    token      TEXT PRIMARY KEY,
    session    TEXT NOT NULL,
    perms      TEXT NOT NULL DEFAULT 'output',
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    label      TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS skills (
    name       TEXT PRIMARY KEY,
    content    TEXT NOT NULL DEFAULT '',
    updated    INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS status_scope (
    status      TEXT NOT NULL,
    scope_type  TEXT NOT NULL,
    scope_value TEXT NOT NULL,
    added_at    INTEGER NOT NULL DEFAULT 0,
    added_by    TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (status, scope_type, scope_value)
);
CREATE TABLE IF NOT EXISTS statuses (
    id          TEXT PRIMARY KEY,
    label       TEXT NOT NULL,
    position    INTEGER NOT NULL DEFAULT 0,
    is_builtin  INTEGER NOT NULL DEFAULT 0
, gate TEXT, mode TEXT NOT NULL DEFAULT 'implicit');
CREATE TABLE IF NOT EXISTS steering_history (
    id           TEXT PRIMARY KEY,
    session      TEXT NOT NULL,
    text         TEXT NOT NULL,
    queued_at    REAL,
    delivered_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS steering_queue (
    id          TEXT PRIMARY KEY,
    session     TEXT NOT NULL,
    text        TEXT NOT NULL,
    queued_at   REAL NOT NULL
, guard TEXT);
CREATE TABLE IF NOT EXISTS task_windows (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    task          TEXT NOT NULL,
    title         TEXT NOT NULL DEFAULT '',
    session       TEXT NOT NULL DEFAULT '',
    entered_doing INTEGER NOT NULL,
    left_doing    INTEGER                          -- NULL = still open (currently doing)
);
CREATE TABLE IF NOT EXISTS tasks (
    id          TEXT PRIMARY KEY,
    session     TEXT NOT NULL,
    text        TEXT NOT NULL,
    done        INTEGER NOT NULL DEFAULT 0,
    pos         INTEGER NOT NULL DEFAULT 0,
    created     INTEGER NOT NULL,
    updated     INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS token_ledger (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           INTEGER NOT NULL,               -- unix seconds of the turn
    session      TEXT NOT NULL DEFAULT '',        -- owning amux session (customTitle)
    conversation TEXT NOT NULL,                   -- JSONL stem (Claude session uuid)
    model        TEXT NOT NULL DEFAULT '',
    input        INTEGER NOT NULL DEFAULT 0,       -- fresh input tokens
    cache_read   INTEGER NOT NULL DEFAULT 0,
    cache_write  INTEGER NOT NULL DEFAULT 0,       -- cache_creation
    output       INTEGER NOT NULL DEFAULT 0,
    cost_usd     REAL NOT NULL DEFAULT 0,
    task         TEXT NOT NULL DEFAULT ''          -- attributed board task id ('' = ambient)
);
CREATE TABLE IF NOT EXISTS waitlist (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    email    TEXT NOT NULL UNIQUE,
    note     TEXT,
    ts       INTEGER NOT NULL
);

-- ── Indexes ──
CREATE INDEX IF NOT EXISTS idx_cal_events_start ON cal_events(start) WHERE deleted IS NULL;
CREATE INDEX IF NOT EXISTS idx_cmd_history_ts ON cmd_history(ts DESC);
CREATE INDEX IF NOT EXISTS idx_crm_contacts_upd ON crm_contacts(updated) WHERE deleted IS NULL;
CREATE INDEX IF NOT EXISTS idx_crm_ix_contact   ON crm_interactions(contact_id, date DESC);
CREATE INDEX IF NOT EXISTS idx_crm_ix_followup  ON crm_interactions(follow_up_date) WHERE follow_up_date IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_dictation_ts ON dictation_history(ts DESC);
CREATE INDEX IF NOT EXISTS idx_email_events_account ON email_events(account_id);
CREATE INDEX IF NOT EXISTS idx_email_events_status  ON email_events(status);
CREATE INDEX IF NOT EXISTS idx_graph_edges_graph ON graph_edges(graph_id);
CREATE INDEX IF NOT EXISTS idx_graph_nodes_graph ON graph_nodes(graph_id);
CREATE INDEX IF NOT EXISTS idx_ilog_kind   ON interaction_log(kind, ts DESC);
CREATE INDEX IF NOT EXISTS idx_ilog_target ON interaction_log(target, ts DESC);
CREATE INDEX IF NOT EXISTS idx_ilog_ts     ON interaction_log(ts DESC);
CREATE INDEX IF NOT EXISTS idx_issue_tags_tag ON issue_tags(tag);
CREATE INDEX IF NOT EXISTS idx_issues_due     ON issues(due) WHERE due IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_issues_session ON issues(session);
CREATE INDEX IF NOT EXISTS idx_issues_status  ON issues(status);
CREATE INDEX IF NOT EXISTS idx_issues_updated ON issues(updated);
CREATE INDEX IF NOT EXISTS idx_journal_date ON journal_entries(date DESC) WHERE deleted IS NULL;
CREATE INDEX IF NOT EXISTS idx_journal_media_entry ON journal_media(entry_id);
CREATE INDEX IF NOT EXISTS idx_ledger_session ON token_ledger(session, ts DESC);
CREATE INDEX IF NOT EXISTS idx_ledger_task ON token_ledger(task, ts DESC);
CREATE INDEX IF NOT EXISTS idx_ledger_ts ON token_ledger(ts DESC);
CREATE INDEX IF NOT EXISTS idx_logs_category ON logs(category);
CREATE INDEX IF NOT EXISTS idx_logs_session  ON logs(session) WHERE session IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_logs_ts       ON logs(ts);
CREATE INDEX IF NOT EXISTS idx_owner_alerts_ts ON owner_alerts(ts DESC);
CREATE INDEX IF NOT EXISTS idx_sched_audit_sched ON schedule_audit(schedule_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_sched_audit_ts    ON schedule_audit(ts DESC);
CREATE INDEX IF NOT EXISTS idx_sched_runs_ran   ON schedule_runs(ran_at DESC);
CREATE INDEX IF NOT EXISTS idx_sched_runs_sched ON schedule_runs(schedule_id, ran_at DESC);
CREATE INDEX IF NOT EXISTS idx_schedules_next ON schedules(next_run) WHERE deleted IS NULL AND enabled=1;
CREATE INDEX IF NOT EXISTS idx_sev_session ON session_events(session, id);
CREATE INDEX IF NOT EXISTS idx_sev_ts      ON session_events(ts);
CREATE INDEX IF NOT EXISTS idx_sev_type    ON session_events(type, id);
CREATE INDEX IF NOT EXISTS idx_steering_hist_session ON steering_history(session, delivered_at DESC);
CREATE INDEX IF NOT EXISTS idx_steering_session ON steering_queue(session);
CREATE INDEX IF NOT EXISTS idx_task_windows_session ON task_windows(session, entered_doing);
CREATE INDEX IF NOT EXISTS idx_tasks_session ON tasks(session);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sev_idem ON session_events(idem) WHERE idem IS NOT NULL;

-- ── Builtin status seeds ──
-- Mirrors the bootstrap INSERTs inside the Python _DB_SCHEMA (amux-server.py:9829).
-- Not an improvement: a fresh DB without these rows has no board lanes. Idempotent;
-- a no-op against the live DB (rows exist). The Python data-fixup UPDATEs for
-- 'done'/'discarded' positions are intentionally NOT replicated (data migration,
-- already applied to the live DB; the values below are the corrected ones).
INSERT OR IGNORE INTO statuses (id, label, position, is_builtin) VALUES
    ('backlog',   'Backlog',      0, 1),
    ('todo',      'To Do',        1, 1),
    ('doing',     'In Progress',  2, 1),
    ('review',    'In Review',    3, 1),
    ('done',      'Done',         4, 1),
    ('verified',  'Verified',     5, 1),
    ('discarded', 'Discarded',    6, 1);
