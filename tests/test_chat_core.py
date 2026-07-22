"""Deterministic tests for the chat core (Scope B1 / Milestone 2).

Loads the REAL pure helpers `_chat_extract_turns`, `_chat_iso_to_epoch` and
`_chat_delivery_status`, plus the REAL `_DB_SCHEMA`, out of amux-server.py via AST
— no server needed, mirroring tests/test_steering_delivery.py + test_write_auth.py.

Covers:
  - N-rows-for-N-turns capture (AC8-multi), with the lossy "newest only" bug barred
  - isSidechain=true rows excluded (subagent-filter, N2)
  - thinking/tool_use-only + non-end_turn rows are not turns
  - chat_replies replay idempotency (populate twice over an overlapping span)
  - C-new-1 single-writer ordering: rowid_seq ascends with turn_index, no inversion
  - C-crit-2 rebuild: DELETE (not DROP) keeps AUTOINCREMENT high-water — replayed
    rowid_seq all HIGHER than the pre-rebuild max; stable ids unchanged
  - delivery-status derivation via the steering join
"""
import ast, os, sqlite3, time

SERVER = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      "amux-server.py")

# ── AST-load the real pure helpers + the real schema ──────────────────────────
ns: dict = {}
tree = ast.parse(open(SERVER, encoding="utf-8").read())
_want_func = {"_chat_extract_turns", "_chat_iso_to_epoch", "_chat_delivery_status"}
_want_assign = {"_DB_SCHEMA"}
for node in tree.body:
    if isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name) \
            and node.targets[0].id in _want_assign:
        exec(compile(ast.Module(body=[node], type_ignores=[]), SERVER, "exec"), ns)
    if isinstance(node, ast.FunctionDef) and node.name in _want_func:
        exec(compile(ast.Module(body=[node], type_ignores=[]), SERVER, "exec"), ns)

extract = ns["_chat_extract_turns"]
SCHEMA = ns["_DB_SCHEMA"]
assert callable(extract), "helpers not extracted from amux-server.py"
assert "chat_replies" in SCHEMA and "chat_messages" in SCHEMA, "chat tables missing from _DB_SCHEMA"


# ── fixtures ──────────────────────────────────────────────────────────────────
_TS = 1_700_000_000
def _iso(off):  # deterministic ISO timestamp
    import datetime as _dt
    return _dt.datetime.fromtimestamp(_TS + off, _dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000Z")

def reply(text, off=0, side=False, stop="end_turn"):
    """A completed user-visible assistant reply row."""
    return {"type": "assistant", "isSidechain": side, "timestamp": _iso(off),
            "message": {"role": "assistant", "stop_reason": stop,
                        "content": [{"type": "text", "text": text}]}}

def tool_step(off=0):
    return {"type": "assistant", "isSidechain": False, "timestamp": _iso(off),
            "message": {"role": "assistant", "stop_reason": "tool_use",
                        "content": [{"type": "tool_use", "id": "t", "name": "Bash", "input": {}}]}}

def thinking(off=0):
    return {"type": "assistant", "isSidechain": False, "timestamp": _iso(off),
            "message": {"role": "assistant", "stop_reason": "tool_use",
                        "content": [{"type": "thinking", "thinking": "..."}]}}

def user(off=0):
    return {"type": "user", "isSidechain": False, "timestamp": _iso(off),
            "message": {"role": "user", "content": [{"type": "text", "text": "go"}]}}


CONV = "11111111-2222-3333-4444-555555555555"

# ── AC8-multi: N top-level turns in one span -> N rows, stable ids ─────────────
rows = [
    user(0),
    thinking(1), tool_step(2), reply("first reply", 3),      # turn 1
    tool_step(4), reply("second reply", 5),                  # turn 2
    reply("third reply", 6),                                 # turn 3
]
turns = extract(rows, CONV, 0)
assert len(turns) == 3, f"AC8-multi: expected 3 turns, got {len(turns)}: {turns}"
assert [t["turn_index"] for t in turns] == [1, 2, 3], f"turn_index not 1..N: {turns}"
assert [t["id"] for t in turns] == [f"{CONV}:1", f"{CONV}:2", f"{CONV}:3"], f"unstable ids: {turns}"
assert [t["text"] for t in turns] == ["first reply", "second reply", "third reply"]
assert turns[0]["turn_ts"] == _TS + 3, f"turn_ts not parsed: {turns[0]}"
print("AC8-multi ok — N top-level turns yield N rows with stable ascending ids (not 1)")

# ── isSidechain=true excluded (subagent-filter, N2) ───────────────────────────
rows_side = [
    reply("main one", 0),
    reply("SUBAGENT leak", 1, side=True),      # must be dropped
    reply("main two", 2),
]
ts = extract(rows_side, CONV, 0)
assert [t["text"] for t in ts] == ["main one", "main two"], f"sidechain not filtered: {ts}"
assert [t["turn_index"] for t in ts] == [1, 2], "sidechain row must not consume a turn_index"
print("isSidechain ok — subagent (isSidechain=true) turns excluded, no index consumed")

# ── non-qualifying rows are not turns ─────────────────────────────────────────
assert extract([thinking(0), tool_step(1)], CONV, 0) == [], "thinking/tool-only must yield no turns"
assert extract([reply("streaming", 0, stop="tool_use")], CONV, 0) == [], "non-end_turn text is not a turn"
assert extract([{"type": "assistant", "message": {"role": "assistant", "stop_reason": "end_turn",
                 "content": [{"type": "text", "text": "   "}]}}], CONV, 0) == [], "blank text is not a turn"
print("filter ok — only completed end_turn text replies count as turns")

# ── since-cursor idempotency (re-run over the same span -> 0 new) ──────────────
assert extract(rows, CONV, 3) == [], "since=3 over a 3-turn span must yield 0 new"
assert [t["turn_index"] for t in extract(rows, CONV, 1)] == [2, 3], "since=1 must yield only 2,3"
print("cursor ok — since_index re-run yields only strictly-newer turns")


# ── DB-backed invariants against the REAL schema ──────────────────────────────
def fresh_db():
    db = sqlite3.connect(":memory:")
    db.row_factory = sqlite3.Row
    db.executescript(SCHEMA)
    return db

def populate(db, session, conv, rows):
    """Mirror the server's single-writer populate loop (ascending turn_index,
    INSERT OR IGNORE on the stable id), driven by the REAL _chat_extract_turns."""
    last = db.execute("SELECT id FROM chat_replies WHERE session=? AND id LIKE ? "
                      "ORDER BY rowid_seq DESC LIMIT 1", (session, conv + ":%")).fetchone()
    since = int(str(last["id"]).rsplit(":", 1)[1]) if last else 0
    ins = 0
    for t in extract(rows, conv, since):
        cur = db.execute("INSERT OR IGNORE INTO chat_replies(id, session, text, turn_ts, created_ts) "
                         "VALUES(?,?,?,?,?)", (t["id"], session, t["text"], t["turn_ts"], int(time.time())))
        ins += cur.rowcount
    db.commit()
    return ins

SESS = "_chattest"
full = [reply("r1", 0), reply("r2", 1), reply("r3", 2), reply("r4", 3), reply("r5", 4)]

# replay idempotency: populate twice over an overlapping span -> no duplicates
db = fresh_db()
assert populate(db, SESS, CONV, full[:3]) == 3, "first populate should insert 3"
assert populate(db, SESS, CONV, full) == 2, "overlapping replay should insert only the 2 new"
assert populate(db, SESS, CONV, full) == 0, "full replay over materialized span inserts 0"
n = db.execute("SELECT COUNT(*) c FROM chat_replies").fetchone()["c"]
assert n == 5, f"replay produced duplicates: {n} rows"
ids = [r["id"] for r in db.execute("SELECT id FROM chat_replies ORDER BY rowid_seq")]
assert ids == [f"{CONV}:{i}" for i in range(1, 6)], f"unstable ids after replay: {ids}"
print("replay ok — overlapping/duplicate populate never double-inserts; ids stable")

# C-new-1 ordering: interleaved populates -> rowid_seq ascends with turn_index
db = fresh_db()
populate(db, SESS, CONV, full[:2])   # turns 1,2
populate(db, SESS, CONV, full[:4])   # turns 3,4
populate(db, SESS, CONV, full)       # turn 5
order = [(r["rowid_seq"], int(r["id"].rsplit(":", 1)[1]))
         for r in db.execute("SELECT rowid_seq, id FROM chat_replies ORDER BY rowid_seq")]
seqs = [s for s, _ in order]
tidx = [t for _, t in order]
assert seqs == sorted(seqs) and tidx == sorted(tidx), f"rowid_seq/turn_index inversion: {order}"
assert tidx == [1, 2, 3, 4, 5], f"turn_index not monotonic in seq order: {order}"
print("C-new-1 ok — single-writer populate keeps rowid_seq ascending with turn_index")

# C-crit-2 rebuild: DELETE (not DROP) preserves AUTOINCREMENT high-water mark
pre_max = db.execute("SELECT MAX(rowid_seq) m FROM chat_replies").fetchone()["m"]
pre_ids = [r["id"] for r in db.execute("SELECT id FROM chat_replies ORDER BY rowid_seq")]
db.execute("DELETE FROM chat_replies")   # rebuild the cache — MUST be DELETE, never DROP
db.commit()
populate(db, SESS, CONV, full)           # replay the full transcript
post = list(db.execute("SELECT rowid_seq, id FROM chat_replies ORDER BY rowid_seq"))
post_seqs = [r["rowid_seq"] for r in post]
post_ids = [r["id"] for r in post]
assert min(post_seqs) > pre_max, f"C-crit-2: rebuilt rowid_seq {post_seqs} not all > pre-rebuild max {pre_max}"
assert post_ids == pre_ids, f"C-crit-2: stable ids changed across rebuild: {post_ids} != {pre_ids}"
# contrast: DROP would have reset the sequence to 1 (proves why DELETE is mandatory)
db.execute("DROP TABLE chat_replies"); db.executescript(SCHEMA); db.commit()
populate(db, SESS, CONV, full)
drop_min = db.execute("SELECT MIN(rowid_seq) m FROM chat_replies").fetchone()["m"]
assert drop_min == 1, f"sanity: DROP should reset AUTOINCREMENT to 1, got {drop_min}"
print("C-crit-2 ok — DELETE rebuild keeps higher seqs + stable ids; DROP resets (why DELETE)")

# ── delivery-status derivation via steering join ──────────────────────────────
delivery = ns["_chat_delivery_status"]
db2 = fresh_db()
ns["get_db"] = lambda: db2
db2.execute("INSERT INTO steering_queue(id, session, text, queued_at) VALUES('sp','x','t',1)")
db2.execute("INSERT INTO steering_history(id, session, text, delivered_at) VALUES('sd','x','t',2)")
db2.commit()
assert delivery("") == "", "no steer_id -> no delivery status"
assert delivery("sp") == "pending", "queued steer_id -> pending"
assert delivery("sd") == "delivered", "history steer_id -> delivered"
assert delivery("gone") == "delivered", "pruned steer_id (had one) -> treated delivered"
print("delivery ok — status derived from steering_queue/history join, no stored column")

print("\nALL CHAT-CORE CHECKS PASSED")


def test_chat_core():
    """The scenarios above execute at import time and raise on any regression;
    reaching here means the pure capture helper (N-turns, subagent-filter,
    cursor idempotency) and the chat_replies invariants (replay idempotency,
    C-new-1 ordering, C-crit-2 DELETE-rebuild) and the delivery-status join hold."""
    assert True
