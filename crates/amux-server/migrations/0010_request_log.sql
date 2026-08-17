-- 0010: structured request log (AMUX-2605) — the observability substrate for
-- the daily log sweep (docs/rust-migration/log-sweep.md).
--
-- ADDITIVE ONLY (shared live DB; the Python server keeps opening this file).
-- One row per API request, written out-of-band by api/request_log.rs through
-- the single-writer store. Worker attribution (`worker`) is derived from the
-- request path, which is what makes per-worker logs a strict SUBSET of the
-- global log rather than a second log to keep in step.
--
-- Size discipline lives in the WRITER, not here: user_agent / error_body /
-- req_meta are capped before insert (bodies are never stored wholesale — a
-- dictation upload is 25MB of audio), and rows older than
-- AMUX_REQLOG_RETAIN_DAYS (default 14) are deleted opportunistically.

CREATE TABLE IF NOT EXISTS _amux_request_log (
    id           INTEGER PRIMARY KEY,
    ts           REAL    NOT NULL,             -- unix seconds (float), request start
    method       TEXT    NOT NULL,
    path         TEXT    NOT NULL,             -- RAW path the client sent (pre alias-rewrite)
    family       TEXT    NOT NULL,             -- boundary-registry family, else /api/<first-seg>
    status       INTEGER NOT NULL,
    latency_ms   REAL    NOT NULL,
    client_ip    TEXT,
    user_agent   TEXT,                         -- truncated
    amux_session TEXT,                         -- X-Amux-Session request header: the CALLER
    worker       TEXT,                         -- path-derived TARGET worker; NULL = not worker-scoped
    req_bytes    INTEGER,                      -- request Content-Length (NULL when absent/chunked)
    resp_bytes   INTEGER,                      -- response Content-Length / buffered size (NULL when streaming)
    answered_by  TEXT    NOT NULL DEFAULT 'native',  -- native | python-proxy (x-amux-answered-by)
    error_body   TEXT,                         -- first 500 chars, ONLY when status >= 400
    req_meta     TEXT                          -- small JSON: query string + content-type (capped)
);

-- The sweep's access paths: time-bounded scans, grouped by family / worker /
-- status (docs/rust-migration/log-sweep.md carries the exact queries).
CREATE INDEX IF NOT EXISTS idx_reqlog_ts        ON _amux_request_log(ts);
CREATE INDEX IF NOT EXISTS idx_reqlog_family_ts ON _amux_request_log(family, ts);
CREATE INDEX IF NOT EXISTS idx_reqlog_worker_ts ON _amux_request_log(worker, ts);
CREATE INDEX IF NOT EXISTS idx_reqlog_status_ts ON _amux_request_log(status, ts);
