#!/usr/bin/env python3
"""amux-chat — session chat as a standalone sidecar.

WHY THIS EXISTS
Chat used to live inside `amux-server.py`. Upstream deleted its own Python
server at 792ce1f (2026-08-09) and this fork is migrating onto upstream's Rust
server, which has no `/api/chat` (measured: 404). `amux-telegram.py` — the only
front-end actually in use — calls exactly three surfaces: `/api/sessions`,
`/api/sessions/<n>/...`, and `/api/chat`. The first two are native on Rust; the
third is this fork's own delta. Extracting it here is what lets the Python
server be retired without taking Telegram down with it.

SERVER-AGNOSTIC BY DESIGN
This process talks to whichever amux server is running (Python on :8822 today,
Rust on :8824 after cutover) over plain HTTP via $AMUX_URL. It owns no session
state of its own; it reads the Claude JSONL transcripts directly and keeps its
materialised index in the same `~/.amux/amux.db` the server uses.

THE CUT
A transitive extraction of the chat core from `amux-server.py` pulls 148
functions / 5,723 lines — `_chat_notify` reaches web-push, `_chat_insert_owner`
reaches the session/board machinery. That entanglement is why chat was in-file
in the first place. So the cut below is DELIBERATE: the read path (resolve a
transcript -> extract turns -> materialise `chat_replies` -> build the thread)
comes across verbatim, and exactly four names are reimplemented here at the
boundary:

  get_db        -> this process opens ~/.amux/amux.db itself
  is_running    -> plain tmux check (the server's version reaches iTerm2 support
                   this sidecar has no need for)
  _chat_notify  -> no-op; the sidecar has no SSE bus of its own
  Path          -> stdlib import

Owner input (`POST /api/chat`) is NOT lifted: in-process `_steer_enqueue` has no
meaning here, so it becomes an HTTP POST to the running server. See
`_chat_insert_owner` at the bottom of this file.

INVARIANTS PRESERVED (see MODIFICATIONS.md, session-chat row)
  * `chat_messages` stays OWNER/SYSTEM-input-only; no `role='session'` rows.
  * `chat_replies` is a rebuildable materialised index over the transcript —
    rebuild with DELETE FROM, never DROP TABLE (the AUTOINCREMENT high-water
    mark must survive so persisted consumer cursors never go stale).
  * `_chat_populate_replies` stays single-writer under a lock, inserting in
    ascending turn_index.
  * `_chat_parse_summary_marker` stays pure so a rebuild re-derives `summary`
    identically from the transcript.
  * `_chat_owned_conv` ranks by ROW COUNT, not recency — a stolen turn is one
    row while the real conversation has hundreds, so "most recent" would hand a
    hijacked slot straight back to the thief.

Run:  AMUX_CHAT_PORT=8825 AMUX_URL=https://localhost:8822 python3 amux-chat.py
"""
import glob
import json
import os
import re
import shlex
import sqlite3
import ssl
import subprocess
import sys
import threading
import time
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

AMUX_DIR = os.path.expanduser("~/.amux")
DB_PATH = os.path.join(AMUX_DIR, "amux.db")
PORT = int(os.environ.get("AMUX_CHAT_PORT", "8825") or 8825)

# The server this sidecar delegates owner input to. Default is the Python
# server's address pre-cutover; flip to :8824 when Rust takes over. Kept as one
# env var precisely so the cutover is a config change, not an edit.
AMUX_URL = os.environ.get("AMUX_URL", "https://localhost:8822").rstrip("/")

_SSL_CTX = ssl._create_unverified_context()  # self-signed, localhost only


def _log(msg):
    sys.stderr.write("[amux-chat] %s\n" % msg)
    sys.stderr.flush()


def _auth_headers():
    """Rust's loopback auth bypass is a config flag this fork turns OFF
    (AMUX_RS_NO_LOOPBACK_BYPASS=1), so send the shared token on every request
    rather than relying on locality. Harmless against the Python server, which
    ignores it for localhost."""
    h = {"Content-Type": "application/json"}
    for fn in ("auth_token", "write_token"):
        p = os.path.join(AMUX_DIR, fn)
        try:
            tok = open(p).read().strip()
        except Exception:
            continue
        if tok:
            h["Authorization"] = "Bearer %s" % tok
            h["X-Amux-Write-Token"] = tok
            break
    return h


def _server(method, path, payload=None, timeout=20):
    """One call into the running amux server. Returns (status, parsed-or-raw)."""
    body = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(AMUX_URL + path, data=body, method=method)
    for k, v in _auth_headers().items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, context=_SSL_CTX, timeout=timeout) as r:
            raw = r.read()
            try:
                return r.status, json.loads(raw)
            except Exception:
                return r.status, raw
    except Exception as e:
        return 0, {"error": str(e)}


# ---- boundary shims (the four names the extracted core still needs) --------

_db_local = threading.local()


def get_db():
    """Thread-local connection to the SAME db the server uses. WAL means a
    second reader/writer coexists safely; this sidecar only ever touches
    chat_messages / chat_replies."""
    conn = getattr(_db_local, "conn", None)
    if conn is None:
        conn = sqlite3.connect(DB_PATH, timeout=30)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA busy_timeout=30000")
        _db_local.conn = conn
    return conn


def is_running(session: str) -> bool:
    """Plain tmux liveness. The server's version also handles iTerm2 backends;
    this sidecar deliberately does not, so the iTerm2 helper closure stays out
    of the cut."""
    try:
        r = subprocess.run(["tmux", "list-sessions", "-F", "#{session_name}"],
                           capture_output=True, text=True, timeout=5)
        return tmux_name(session) in r.stdout.split()
    except Exception:
        return False


def _chat_notify(session: str, kind: str) -> None:
    """No-op. In the server this pushed onto the SSE bus; the sidecar has no bus
    of its own and its consumers (Telegram, and the dashboard via the server)
    poll. Kept as a named seam so an SSE relay can land here later without
    touching the extracted core."""
    return None


def _init_db():
    """Schema is IF NOT EXISTS and matches amux-server.py's `_DB_SCHEMA` block
    verbatim, so running against an existing db is a no-op and running against a
    fresh one produces the same shape."""
    db = get_db()
    db.executescript("""
CREATE TABLE IF NOT EXISTS chat_messages (
    id         TEXT PRIMARY KEY,
    session    TEXT NOT NULL,
    role       TEXT NOT NULL,
    origin     TEXT NOT NULL DEFAULT '',
    text       TEXT NOT NULL,
    steer_id   TEXT DEFAULT '',
    created_ts INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chat_session_ts ON chat_messages(session, created_ts);
CREATE TABLE IF NOT EXISTS chat_replies (
    rowid_seq  INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT UNIQUE NOT NULL,
    session    TEXT NOT NULL,
    text       TEXT NOT NULL,
    summary    TEXT,
    turn_ts    INTEGER NOT NULL,
    created_ts INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chat_replies_session_seq ON chat_replies(session, rowid_seq);
""")
    db.commit()


# ---- extracted core (verbatim from amux-server.py, see THE CUT above) ------
CLAUDE_HOME = Path.home() / ".claude"


def _claude_config_homes() -> list:
    """All Claude config homes amux sessions may run under (existing dirs only,
    default ~/.claude always first)."""
    homes = [CLAUDE_HOME]
    for env_var, default in (("AMUX_WORK_CONFIG_DIR", ".claude-work"),
                             ("AMUX_PERSONAL_CONFIG_DIR", ".claude-personal")):
        p = Path(os.environ.get(env_var, str(Path.home() / default))).expanduser()
        if p.is_dir() and p not in homes:
            homes.append(p)
    return homes


def _claude_project_dir(work_dir: str) -> Path:
    """Transcript project dir for work_dir, searched across all config homes.

    A session's transcripts live under whichever config home it was launched
    with, so readers must look beyond ~/.claude. Prefers the most recently
    modified match; falls back to the default-home path."""
    pname = _project_name(work_dir)
    best, best_mtime = None, -1.0
    for home in _claude_config_homes():
        d = home / "projects" / pname
        if d.is_dir():
            try:
                mt = d.stat().st_mtime
            except OSError:
                continue
            if mt > best_mtime:
                best, best_mtime = d, mt
    return best or (CLAUDE_HOME / "projects" / pname)


def parse_env_file(path: Path) -> dict:
    """Parse a amux session .env file into a dict."""
    data = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        m = re.match(r'^(\w+)="(.*)"$', line)
        if m:
            data[m.group(1)] = m.group(2)
            continue
        m = re.match(r"^(\w+)='(.*)'$", line)
        if m:
            data[m.group(1)] = m.group(2)
            continue
        m = re.match(r"^(\w+)=(.*)$", line)
        if m:
            data[m.group(1)] = m.group(2)
    return data


def tmux_name(session: str) -> str:
    new = f"amux-{session}"
    # Only check for legacy cmux-*/cc-* names once per session per process lifetime
    if session not in _tmux_name_migrated:
        _tmux_name_migrated.add(session)
        for old in [f"cmux-{session}", f"cc-{session}"]:
            try:
                r = subprocess.run(["tmux", "has-session", "-t", old], capture_output=True, timeout=3)
                if r.returncode == 0:
                    subprocess.run(["tmux", "rename-session", "-t", old, new], capture_output=True, timeout=5)
                    break
            except Exception:
                pass
    return new


def tmux_target(session: str) -> str:
    """Return the tmux target for -t flags."""
    return tmux_name(session)


_jsonl_path_cache: dict[str, tuple[float, "Path | None"]] = {}  # session -> (monotonic, path)


def _session_jsonl_path(name: str):
    """Newest Claude Code JSONL conversation file for a session's working dir,
    or None. This is the authoritative, complete transcript (never torn like the
    alt-screen snapshot log)."""
    now = time.monotonic()
    cached = _jsonl_path_cache.get(name)
    if cached and (now - cached[0]) < 10:
        return cached[1]
    result = _session_jsonl_path_uncached(name)
    _jsonl_path_cache[name] = (now, result)
    return result


def _session_jsonl_path_uncached(name: str):
    env_file = CC_SESSIONS / f"{name}.env"
    if not env_file.exists():
        return None
    try:
        cfg = parse_env_file(env_file)
    except Exception:
        return None
    wd = (cfg.get("CC_DIR") or "").strip()
    if not wd:
        return None
    try:
        # Deterministic path first: the PostToolUse hook reports Claude's live
        # session_id (+ real cwd) on every tool call — <id>.jsonl IS this
        # session's conversation. Title matching below is only a fallback: in
        # shared workdirs conversations get resumed across sessions and titles
        # go stale, which made peeks render another session's (or an old)
        # transcript — whole turns missing vs tmux.
        meta = _load_meta(name)
        conv_id = (meta.get("cc_conversation_id") or "").strip()
        for base in ((meta.get("cc_cwd") or "").strip(), wd):
            if not (conv_id and base):
                continue
            cand = CLAUDE_HOME / "projects" / _project_name(base) / f"{conv_id}.jsonl"
            if cand.is_file():
                return cand
        project_dir = CLAUDE_HOME / "projects" / _project_name(wd)
        if not project_dir.is_dir():
            return None
        files = sorted(project_dir.glob("*.jsonl"),
                       key=lambda f: f.stat().st_mtime, reverse=True)
        if not files:
            return None
        if len(files) == 1:
            return files[0]
        # Multiple conversations live in this project dir — several amux sessions
        # share one CC_DIR (e.g. studio-plg + mixpeek-studio). Returning the
        # newest bleeds ANOTHER session's transcript into this session's peek, so
        # resolve to THIS session's own conversation by matching Claude Code's
        # per-conversation title (set from the launch `--name`) to the amux
        # session name. Files are mtime-desc, so the first match is this
        # session's newest conversation.
        for jf in files:
            try:
                with jf.open() as fh:
                    rec = json.loads(fh.readline() or "{}")
            except Exception:
                continue
            if rec.get("customTitle") == name or rec.get("sessionName") == name:
                return jf
        # No titled match. Do NOT fall back to the newest file — in a shared
        # workdir that's a SIBLING session's transcript bleeding into this one
        # (e.g. a freshly-created session that has no conversation of its own
        # yet). Exclude conversations already claimed by other amux sessions
        # (their meta records cc_conversation_id); only return a file if exactly
        # one plausibly-ours candidate remains, else show live-only (None).
        owned = set()
        try:
            for oenv in CC_SESSIONS.glob("*.env"):
                if oenv.stem == name:
                    continue
                ocid = (_load_meta(oenv.stem).get("cc_conversation_id") or "").strip()
                if ocid:
                    owned.add(ocid)
        except Exception:
            pass
        unclaimed = [jf for jf in files if jf.stem not in owned]
        return unclaimed[0] if len(unclaimed) == 1 else None
    except Exception:
        return None


def _iter_jsonl_tail(filepath: Path, max_bytes: int = 5_000_000):
    """Iterate over parsed entries from the tail of a JSONL file.

    Unlike _read_jsonl_tail, this yields entries one at a time instead of
    accumulating them all in a list — much less memory for large files.
    """
    try:
        size = filepath.stat().st_size
    except OSError:
        return
    try:
        with filepath.open("rb") as fh:
            if size > max_bytes:
                fh.seek(size - max_bytes)
                fh.readline()  # discard partial first line
            for raw in fh:
                try:
                    yield json.loads(raw)
                except (json.JSONDecodeError, ValueError):
                    continue
    except Exception:
        pass


def _chat_iso_to_epoch(ts) -> int:
    """Best-effort ISO-8601 (Claude JSONL 'timestamp') -> unix seconds; 0 on failure."""
    if not ts:
        return 0
    if isinstance(ts, (int, float)):
        return int(ts)
    try:
        import datetime as _dt
        s = str(ts).strip().replace("Z", "+00:00")
        return int(_dt.datetime.fromisoformat(s).timestamp())
    except Exception:
        return 0


# AMUX-LOCAL:session-chat — reply-summary marker parser (docs/reply-summary.md).
# The marker convention (shared via ~/.claude/common.md "Reply Summary Marker") asks
# the main model to end a substantive reply with a final standalone line "⌁ <one
# sentence>". This is the ONE parser for that contract — pure + deterministic, so a
# chat_replies rebuild (DELETE + replay) re-derives the same summary from the same
# transcript text every time (no drift).
_CHAT_SUMMARY_MARKER = "⌁"


def _chat_parse_summary_marker(text: str) -> tuple:
    """PURE: split a turn's full reply text into (clean_text, summary). `summary`
    is the marker sentence (capped to _CHAT_SUMMARY_MAX_CHARS) when the LAST
    non-empty line starts with the "⌁" glyph (any amount of whitespace after it —
    "⌁ foo", "⌁foo", "⌁   foo" all match); `clean_text` has that line removed so
    the marker never leaks into the stored/rendered reply body. Returns
    (text, None) unchanged when no marker line is present, or when stripping it
    would leave nothing (never store an empty reply)."""
    lines = text.split("\n")
    last_idx = None
    for i in range(len(lines) - 1, -1, -1):
        if lines[i].strip():
            last_idx = i
            break
    if last_idx is None:
        return text, None
    candidate = lines[last_idx].strip()
    if not candidate.startswith(_CHAT_SUMMARY_MARKER):
        return text, None
    summary = candidate[len(_CHAT_SUMMARY_MARKER):].strip()
    if not summary:
        return text, None
    clean_text = "\n".join(lines[:last_idx] + lines[last_idx + 1:]).rstrip()
    if not clean_text:
        return text, summary[:_CHAT_SUMMARY_MAX_CHARS]
    return clean_text, summary[:_CHAT_SUMMARY_MAX_CHARS]


def _chat_extract_turns(rows, conversation_id: str = "", since_index: int = 0) -> list:
    """PURE projection helper (AST-loadable / harness-driven like _steer_try_deliver —
    no I/O): a conversation's JSONL rows in file order -> the list of top-level
    assistant reply turns to surface, as {id, turn_index, text, summary, turn_ts} dicts.
    `summary` (AMUX-LOCAL:session-chat) is the parsed "⌁" marker sentence, or None
    (see _chat_parse_summary_marker / docs/reply-summary.md).

    A 'turn' is ONE completed, user-visible assistant reply: an assistant message
    with stop_reason == 'end_turn' carrying >=1 non-empty text block. Intermediate
    tool_use / thinking-only steps are not turns. Sub-agent turns are filtered by
    JSONL `isSidechain` (defense-in-depth: in the current Claude Code schema they
    route to sibling <conv>/subagents/*.jsonl files and never enter the main
    transcript, but this guarantees none can ever leak into the top-level thread).

    turn_index is the 1-based ordinal of qualifying turns from the START of the
    conversation (JSONL is append-only => stable). Only turns with ordinal >
    since_index are returned, strictly ascending — so the single-writer populate
    assigns rowid_seq in transcript order (C-new-1) and an INSERT OR IGNORE replay
    over an already-materialized span is a no-op (idempotent on the stable id)."""
    turns = []
    idx = 0
    for e in rows:
        if not isinstance(e, dict):
            continue
        if e.get("isSidechain"):
            continue
        if e.get("type") != "assistant":
            continue
        msg = e.get("message") or {}
        if not isinstance(msg, dict) or msg.get("role") != "assistant":
            continue
        if msg.get("stop_reason") != "end_turn":
            continue
        content = msg.get("content")
        parts = []
        if isinstance(content, list):
            for b in content:
                if isinstance(b, dict) and b.get("type") == "text":
                    t = b.get("text") or ""
                    if t.strip():
                        parts.append(t)
        elif isinstance(content, str):
            if content.strip():
                parts.append(content)
        text = "\n".join(parts).strip()
        if not text:
            continue
        idx += 1
        if idx <= since_index:
            continue
        clean_text, summary = _chat_parse_summary_marker(text)  # AMUX-LOCAL:session-chat
        turns.append({
            "id": (conversation_id + ":" + str(idx)) if conversation_id else str(idx),
            "turn_index": idx,
            "text": clean_text,
            "summary": summary,  # AMUX-LOCAL:session-chat — see docs/reply-summary.md
            "turn_ts": _chat_iso_to_epoch(e.get("timestamp")),
        })
    return turns


# ── chat_replies single-writer population (C-new-1) ──────────────────────────
_chat_replies_lock = threading.Lock()


_CHAT_JSONL_MAX_BYTES = 64_000_000   # full-read cap for stable global turn indexing


# AMUX-LOCAL:session-chat
# AMUX-10: live-conv-id fallback cache. Fresh sessions started WITHOUT
# --session-id/--resume never get the graceful-stop rename that records
# cc_conversation_id in meta, so _session_jsonl_path's meta path can't resolve
# their transcript; the same is true when the transcript lives under a non-default
# config home (mac-server: ~/.claude-personal), which _session_jsonl_path_uncached
# does not scan. We resolve the conv id from the running process argv / newest
# jsonl across ALL config homes via _live_conv_id, but that runs ps/tmux, so cache
# the result in-memory per session (NOT persisted to meta — a wrong guess from a
# shared-dir newest-jsonl must self-correct next poll, never cement into resume).
_chat_conv_fallback_cache: dict = {}   # session -> (conv_id, monotonic_ts)


_CHAT_CONV_FALLBACK_TTL = 30.0         # re-verify via _live_conv_id at most this often


def _chat_conv_jsonl(wd: str, conv_id: str):
    """Path of `conv_id`'s transcript under whichever config home holds it, else
    None. Config homes are scanned because a transcript may live outside CLAUDE_HOME
    (mac-server: ~/.claude-personal)."""
    if not conv_id:
        return None
    try:
        for home in _claude_config_homes():
            cand = home / "projects" / _project_name(wd) / f"{conv_id}.jsonl"
            if cand.is_file():
                return cand
    except Exception:
        pass
    return None


def _chat_owned_conv(name: str) -> str:
    """The conversation this slot has ESTABLISHED chat history for — the conv id
    backing the most chat_replies rows (ties broken by most recent). '' when the
    slot has captured nothing yet. Row count, NOT recency, is the discriminator:
    a stolen turn contributes exactly one row while the slot's real conversation
    accumulates hundreds, so 'most rows' survives a leak that 'most recent' would
    hand right back to the thief."""
    try:
        row = get_db().execute(
            "SELECT substr(id, 1, instr(id, ':') - 1) AS conv, COUNT(*) AS n, "
            "MAX(created_ts) AS t FROM chat_replies WHERE session=? AND instr(id, ':')>1 "
            "GROUP BY conv ORDER BY n DESC, t DESC LIMIT 1", (name,)).fetchone()
    except Exception:
        return ""
    return (row["conv"] or "").strip() if row else ""


def _chat_live_conv_path(name: str):
    """AMUX-10 fallback: resolve a session's transcript path from its LIVE
    conversation id when the meta-based _session_jsonl_path can't. Cached in-memory
    (see _chat_conv_fallback_cache) so ps/tmux runs at most once per
    _CHAT_CONV_FALLBACK_TTL per session. Returns a Path or None; on miss caches
    nothing (retry next poll).

    Resolution order, most authoritative first:
      1. `is_running` gate — a slot with no live Claude has no current transcript,
         so it must never guess at all.
      2. argv (`_live_conv_id(name)`, no work dir ⇒ argv only) — definitive whenever
         amux launched the session itself with --session-id/--resume.
      3. STICKY: the conversation this slot already owns (`_chat_owned_conv`), if its
         transcript still exists.
      4. Only with none of the above: `_live_conv_id(name, wd)`, whose step 2 is
         "newest jsonl in the work dir".

    Steps 1 and 3 exist because step 4 is racy by construction: in a work dir shared
    with any other Claude session it resolves to whichever conversation was written
    last — including a bare `claude` CLI amux does not own. The stolen turn is then
    written to the slot's chat_replies AND pushed to Telegram. Both slots with
    CC_DIR ~/Desktop/Projects/amux were affected on 2026-08-01: `--help` (dead, all
    7 of its rows stolen — fixed by step 1) and `amux-helper` (live, no argv id and
    no meta id because it resumes a conversation born 2026-07-16, 10 rows stolen
    beside its own 92 — fixed by step 3).

    Residual: a brand-new slot with no argv id and no history still relies on step 4,
    and once step 3 has something to hold onto it holds on — a slot that starts a
    genuinely new conversation without an argv id stays pinned to the old one while
    that transcript exists. Pinned-but-stale beats stealing a neighbour's replies,
    and passing --session-id at launch removes the guess entirely."""
    try:
        wd = _session_work_dir(name)
    except Exception:
        wd = ""
    if not wd:
        return None
    now = time.monotonic()
    cached = _chat_conv_fallback_cache.get(name)
    if cached and (now - cached[1]) < _CHAT_CONV_FALLBACK_TTL:
        conv_id = cached[0]
    else:
        # The whole ladder sits INSIDE the cache-miss branch so it stays TTL-bounded
        # like the _live_conv_id call it guards. For a dead slot it is also cheaper
        # than what it replaces: one `tmux list-sessions` instead of list-panes +
        # pgrep + ps-per-pid + a work-dir scan.
        try:
            alive = is_running(name)
        except Exception:
            alive = False
        conv_id = ""
        if alive:
            try:
                conv_id = (_live_conv_id(name) or "").strip()   # argv only (no wd)
            except Exception:
                conv_id = ""
            if not conv_id:
                owned = _chat_owned_conv(name)
                if _chat_conv_jsonl(wd, owned) is not None:
                    conv_id = owned
            if not conv_id:
                try:
                    conv_id = (_live_conv_id(name, wd) or "").strip()
                except Exception:
                    conv_id = ""
        if conv_id:
            _chat_conv_fallback_cache[name] = (conv_id, now)
        else:
            _chat_conv_fallback_cache.pop(name, None)
    return _chat_conv_jsonl(wd, conv_id)


def _chat_resolve_jsonl_path(name: str):
    """Transcript path for chat capture (AMUX-10). Primary: the meta-based
    _session_jsonl_path (cheap + stable). Fallback: _chat_live_conv_path, which
    resolves fresh sessions (no recorded cc_conversation_id) and transcripts under
    non-default config homes that the primary path misses. Returns a Path or None."""
    try:
        path = _session_jsonl_path(name)
    except Exception:
        path = None
    if path and path.exists():
        return path
    return _chat_live_conv_path(name)


def _chat_populate_replies(name: str) -> list:
    """SINGLE-WRITER (C-new-1): materialize new transcript reply turns for a
    session into chat_replies, under _chat_replies_lock, inserting strictly in
    ascending turn_index so rowid_seq tracks transcript order. Idempotent
    (INSERT OR IGNORE on the stable id). BOTH the monitor idle-hook and the SSE
    reconciliation path funnel through here (never insert independently). Returns
    the newly-inserted turn dicts."""
    path = _chat_resolve_jsonl_path(name)   # AMUX-10: meta path, else live-conv-id fallback
    if not path or not path.exists():
        return []
    conv = path.stem
    new = []
    with _chat_replies_lock:
        try:
            db = get_db()
            last = db.execute(
                "SELECT id FROM chat_replies WHERE session=? AND id LIKE ? "
                "ORDER BY rowid_seq DESC LIMIT 1", (name, conv + ":%")
            ).fetchone()
            since = 0
            if last and last["id"]:
                try:
                    since = int(str(last["id"]).rsplit(":", 1)[1])
                except Exception:
                    since = 0
            rows = list(_iter_jsonl_tail(path, max_bytes=_CHAT_JSONL_MAX_BYTES))
            turns = _chat_extract_turns(rows, conv, since)
            now = int(time.time())
            for t in turns:   # already ascending turn_index
                cur = db.execute(
                    "INSERT OR IGNORE INTO chat_replies(id, session, text, summary, turn_ts, created_ts) "
                    "VALUES(?,?,?,?,?,?)",
                    (t["id"], name, t["text"], t.get("summary"), t["turn_ts"], now),
                )
                if cur.rowcount:
                    new.append(t)
            db.commit()
        except Exception:
            return []
    if new:
        _chat_notify(name, "reply")
    return new


# ── GET-triggered freshness hook (Bug-1: reply-capture latency) ──────────────
# _chat_populate_replies only ran from the monitor idle-transition (2s samples
# miss fast turns) and SSE-connect reconcile (dashboard holds SSE ~5min, so no
# reconcile for minutes). A reply could therefore sit uncaptured for minutes.
# These two hooks close that window WITHOUT a new busy loop, both funnelling
# through the SAME single-writer _chat_populate_replies (under _chat_replies_lock
# — single-writer invariant preserved):
#   1. every GET /api/chat?session=X populates X first, per-session throttled;
#   2. every delivered steer schedules a few populate attempts (a delivery
#      guarantees a turn is starting — capture its reply soon after it ends).
_chat_get_populate_last: dict = {}    # session -> monotonic ts of last GET-triggered populate


_chat_get_populate_lock = threading.Lock()


_CHAT_GET_POPULATE_THROTTLE = 4.0     # at most one GET-triggered populate per session per this many secs


def _chat_populate_replies_throttled(name: str) -> None:
    """Per-session throttled populate for the GET /api/chat freshness hook.
    Runs synchronously (so the SAME response reflects any just-captured reply),
    but at most once per session per _CHAT_GET_POPULATE_THROTTLE seconds so the
    dashboard poll + Telegram sidecar outbound loop can't storm the transcript
    read. The write still holds the single _chat_replies_lock."""
    if not name:
        return
    now = time.monotonic()
    with _chat_get_populate_lock:
        last = _chat_get_populate_last.get(name, 0.0)
        if last and (now - last) < _CHAT_GET_POPULATE_THROTTLE:
            return
        _chat_get_populate_last[name] = now
    try:
        _chat_populate_replies(name)
    except Exception:
        pass


def _chat_delivery_status(steer_id: str) -> str:
    """Derive an owner message's delivery status from steering state (NO stored
    column): 'delivered' if in steering_history, 'pending' if still queued, and
    'delivered' when a steer_id exists but the history row was pruned; '' otherwise."""
    if not steer_id:
        return ""
    try:
        db = get_db()
        if db.execute("SELECT 1 FROM steering_history WHERE id=?", (steer_id,)).fetchone():
            return "delivered"
        if db.execute("SELECT 1 FROM steering_queue WHERE id=?", (steer_id,)).fetchone():
            return "pending"
    except Exception:
        return ""
    return "delivered"


def _chat_build_thread(session: str, since_seq: int = 0, limit: int = 500) -> dict:
    """Merged read (§4.3f): owner/system rows from chat_messages + reply turns from
    the chat_replies materialized index, delivery status joined from steering,
    ordered for display. `since_seq` is the chat_replies.rowid_seq cursor — replies
    with rowid_seq > since are returned; when a cursor is given, owner/system rows
    are also incremental (created after the cursor's turn timestamp). `cursor` is
    the session's max rowid_seq (rebuild-safe high-water mark) for the next poll."""
    db = get_db()
    since_seq = int(since_seq or 0)
    since_ts = 0
    if since_seq > 0:
        r = db.execute("SELECT turn_ts FROM chat_replies WHERE session=? AND rowid_seq=?",
                       (session, since_seq)).fetchone()
        if r:
            since_ts = int(r["turn_ts"] or 0)
    reps = db.execute(
        "SELECT rowid_seq, id, text, summary, turn_ts FROM chat_replies WHERE session=? AND rowid_seq>? "
        "ORDER BY rowid_seq ASC LIMIT ?", (session, since_seq, limit)).fetchall()
    if since_seq > 0:
        msgs = db.execute(
            "SELECT id, role, origin, text, steer_id, created_ts FROM chat_messages "
            "WHERE session=? AND created_ts>? ORDER BY created_ts ASC LIMIT ?",
            (session, since_ts, limit)).fetchall()
    else:
        msgs = db.execute(
            "SELECT id, role, origin, text, steer_id, created_ts FROM chat_messages "
            "WHERE session=? ORDER BY created_ts ASC LIMIT ?", (session, limit)).fetchall()
    items = []
    for m in msgs:
        items.append({
            "id": m["id"], "role": m["role"], "origin": m["origin"], "text": m["text"],
            "ts": int(m["created_ts"] or 0), "seq": None, "summary": None,
            "delivery": _chat_delivery_status(m["steer_id"]) if m["role"] == "owner" else "",
        })
    for r in reps:
        items.append({
            "id": r["id"], "role": "session", "origin": "session", "text": r["text"],
            "summary": r["summary"] if r["summary"] else None,  # AMUX-LOCAL:session-chat
            "ts": int(r["turn_ts"] or 0), "seq": r["rowid_seq"], "delivery": "",
        })
    items.sort(key=lambda x: (x["ts"], x["seq"] if x["seq"] is not None else -1))
    cur = db.execute("SELECT MAX(rowid_seq) AS m FROM chat_replies WHERE session=?",
                     (session,)).fetchone()
    cursor = int(cur["m"]) if cur and cur["m"] is not None else since_seq
    return {"session": session, "thread": items, "cursor": cursor}


def _meta_path(name: str) -> Path:
    return CC_SESSIONS / f"{name}.meta.json"


def _load_meta(name: str) -> dict:
    p = _meta_path(name)
    if p.exists():
        try:
            return json.loads(p.read_text())
        except Exception:
            pass
    return {}


def _find_latest_session_id(work_dir: str) -> str:
    """Find the most recent Claude Code conversation session ID for a working directory.
    Skips snapshot-only files that have no user/assistant messages (claude --resume exits on those)."""
    project_dir = _claude_project_dir(work_dir)
    if not project_dir.is_dir():
        return ""
    jsonl_files = sorted(project_dir.glob("*.jsonl"), key=lambda f: f.stat().st_mtime, reverse=True)
    for f in jsonl_files:
        try:
            text = f.read_text(errors="replace")
            for line in text.splitlines():
                entry = json.loads(line)
                if entry.get("type") in ("user", "assistant"):
                    return f.stem
        except Exception:
            continue
    return ""


def _conversation_owned_by_other(conv_id: str, this_session: str) -> str:
    """Return the name of a DIFFERENT session whose meta already claims `conv_id`,
    or '' if none. Two amux sessions must never share one Claude conversation:
    when they do (shared CC_DIR + a borrowed/stale id), the two panes render one
    JSONL and commands to one mirror into the other (mixpeek-general adopted
    mixpeek-frustrations' f035d084, 2026-07-16). Used to refuse adopting a
    neighbor's conversation id so a collision can never be cemented."""
    if not conv_id:
        return ""
    for mf in CC_SESSIONS.glob("*.meta.json"):
        other = mf.name[:-len(".meta.json")]
        if other == this_session:
            continue
        try:
            m = json.loads(mf.read_text(errors="replace"))
        except Exception:
            continue
        if (m.get("cc_conversation_id") or "") == conv_id:
            return other
    return ""


def _project_name(work_dir: str) -> str:
    """Return the Claude project folder name for a given work dir (mirrors
    Claude's own encoding). Claude replaces EVERY non-alphanumeric character
    with '-', not just slashes — a workdir containing a space or dot (e.g.
    '~/Obsidian Vault/Self' -> '-Users-ethan-Obsidian-Vault-Self') otherwise
    resolves to a project dir Claude never writes, silently breaking
    transcripts, token counts, and model/resume detection for that session."""
    resolved = str(Path(work_dir).expanduser().resolve())
    return re.sub(r"[^A-Za-z0-9]", "-", resolved)


def _live_conv_id(name: str, work_dir: str = "") -> str:
    """Return the conversation id of the running claude process for a session.

    Sources, in order of authority:
      1. Argv of the running claude process — definitive when --session-id or
         --resume is set (i.e. when start_session launched it).
      2. Most-recently-modified jsonl in the work_dir's project folder.
         Unreliable when multiple amux sessions share a work_dir, but the only
         option for sessions started via the bash CLI (which doesn't set the
         flag — Claude generates the id internally and doesn't expose it).
    Empty string if nothing can be determined.
    """
    try:
        r = subprocess.run(
            ["tmux", "list-panes", "-t", tmux_target(name), "-F", "#{pane_pid}"],
            capture_output=True, text=True, timeout=5,
        )
        pane_pid = r.stdout.strip().split("\n")[0] if r.returncode == 0 else ""
        if pane_pid:
            r2 = subprocess.run(["pgrep", "-P", pane_pid], capture_output=True, text=True, timeout=5)
            for pid in r2.stdout.strip().split("\n"):
                if not pid:
                    continue
                r3 = subprocess.run(
                    ["ps", "-o", "command=", "-p", pid],
                    capture_output=True, text=True, timeout=5,
                )
                cmd = r3.stdout.strip()
                if "claude" not in cmd:
                    continue
                parts = shlex.split(cmd) if cmd else []
                for flag in ("--resume", "--session-id"):
                    if flag in parts:
                        idx = parts.index(flag)
                        if idx + 1 < len(parts):
                            return parts[idx + 1]
    except Exception:
        pass
    if work_dir:
        try:
            cand = _find_latest_session_id(work_dir)
            # In a SHARED work_dir the "latest jsonl" belongs to whichever session
            # was last active, not necessarily this one. Adopting it cross-links two
            # sessions onto one Claude conversation (the mixpeek-general/frustrations
            # bug, 2026-07-16). Refuse a candidate another session already owns so a
            # separated session can't silently re-grab its neighbor's conversation.
            if cand and _conversation_owned_by_other(cand, name):
                return ""
            return cand
        except Exception:
            return ""
    return ""


def _session_work_dir(name: str) -> str:
    """Return the CC_DIR for a session, or empty string if not configured."""
    env_file = CC_SESSIONS / f"{name}.env"
    if env_file.exists():
        cfg = parse_env_file(env_file)
        wd = cfg.get("CC_DIR", "").strip()
        if wd:
            return str(Path(wd).expanduser().resolve())
    return ""




# ---- owner input: the one path that could NOT be lifted verbatim ----------

def _steer_via_server(session: str, text: str) -> str:
    """Replacement for the server's in-process `_steer_enqueue`.

    MEASURED 2026-08-17, both servers live: `GET /api/sessions/<n>/steer` -> 200
    on :8822 AND :8824; `/send` -> 404 on BOTH. So `steer` is the real path on
    each and the `/send` attempt below is a dead fallback today — kept only
    because it costs one 404 on a path that already failed, and removing it
    would leave nothing if upstream renames the verb. It is NOT load-bearing,
    and this comment exists so nobody later reads it as evidence that `/send`
    works somewhere. (An earlier draft of this docstring asserted "both
    spellings exist on the Python server"; that was never measured and was
    wrong.)

    Returns a steer id, or "" if neither path accepted it. "" is honest: the
    caller records it and delivery status stays underived rather than claiming
    a send that did not happen.
    """
    for path, payload in (
        ("/api/sessions/%s/steer" % session, {"text": text}),
        ("/api/sessions/%s/send" % session, {"text": text}),
    ):
        st, body = _server("POST", path, payload)
        if st == 200:
            if isinstance(body, dict):
                for k in ("steer_id", "id", "queued_id"):
                    if body.get(k):
                        return str(body[k])
            return "sent"
        _log("steer path %s -> %s" % (path, st))
    return ""


def _chat_insert_owner(session: str, text: str, msg_id: str = "",
                       origin: str = "dashboard") -> dict:
    """Owner input — the ONE authoritative path. Insert exactly one owner row
    (idempotent on the stable id), then deliver over HTTP. Only the INSERT
    winner delivers, so a retry or mirror carrying the same id neither
    re-inserts nor re-sends (echo-loop immunity). Never writes cmd_history —
    that stays upstream's audit log."""
    cid = (str(msg_id or "").strip()[:64]) or ("chat-" + uuid.uuid4().hex)
    now = int(time.time())
    db = get_db()
    cur = db.execute(
        "INSERT OR IGNORE INTO chat_messages(id, session, role, origin, text, steer_id, created_ts) "
        "VALUES(?,?,?,?,?,?,?)",
        (cid, session, "owner", origin, text, "", now),
    )
    if not cur.rowcount:
        db.commit()
        row = db.execute("SELECT * FROM chat_messages WHERE id=?", (cid,)).fetchone()
        base = dict(row) if row else {
            "id": cid, "session": session, "role": "owner", "origin": origin,
            "text": text, "steer_id": "", "created_ts": now}
        base["deduped"] = True
        return base
    steer_id = ""
    try:
        steer_id = _steer_via_server(session, text)
    except Exception as e:
        _log("steer failed for %s: %s" % (session, e))
    db.execute("UPDATE chat_messages SET steer_id=? WHERE id=?", (steer_id, cid))
    db.commit()
    _chat_notify(session, "owner")
    return {"id": cid, "session": session, "role": "owner", "origin": origin,
            "text": text, "steer_id": steer_id, "created_ts": now,
            "deduped": False}


# ---- HTTP surface: byte-compatible with the server's /api/chat ------------

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass  # access logging is noise; real events go through _log

    def _reply(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _qs(self):
        from urllib.parse import urlparse, parse_qs
        return parse_qs(urlparse(self.path).query)

    def do_GET(self):
        from urllib.parse import urlparse
        if urlparse(self.path).path != "/api/chat":
            return self._reply(404, {"error": "not found"})
        q = self._qs()
        session = (q.get("session") or [""])[0].strip()
        if not session:
            return self._reply(400, {"error": "session required"})
        since = int((q.get("since") or ["0"])[0] or 0)
        # Freshness hook: same as the server's GET — materialise any new turns
        # before building the thread, throttled so a polling consumer cannot
        # spin the transcript reader.
        try:
            _chat_populate_replies_throttled(session)
        except Exception as e:
            _log("populate failed for %s: %s" % (session, e))
        try:
            return self._reply(200, _chat_build_thread(session, since_seq=since))
        except Exception as e:
            _log("build_thread failed for %s: %s" % (session, e))
            return self._reply(500, {"error": str(e)})

    def do_POST(self):
        from urllib.parse import urlparse
        if urlparse(self.path).path != "/api/chat":
            return self._reply(404, {"error": "not found"})
        try:
            n = int(self.headers.get("Content-Length") or 0)
            body = json.loads(self.rfile.read(n) or b"{}")
        except Exception as e:
            return self._reply(400, {"error": "bad json: %s" % e})
        session = str(body.get("session") or "").strip()
        text = str(body.get("text") or "")
        if not session or not text:
            return self._reply(400, {"error": "session and text required"})
        try:
            row = _chat_insert_owner(session, text,
                                     msg_id=str(body.get("id") or ""),
                                     origin=str(body.get("origin") or "sidecar"))
            return self._reply(200, row)
        except Exception as e:
            _log("insert_owner failed: %s" % e)
            return self._reply(500, {"error": str(e)})


def main():
    _init_db()
    _log("db=%s  server=%s  port=%d" % (DB_PATH, AMUX_URL, PORT))
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    srv.daemon_threads = True
    _log("listening on http://127.0.0.1:%d/api/chat" % PORT)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
