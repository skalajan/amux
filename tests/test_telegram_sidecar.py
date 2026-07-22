"""Deterministic tests for the Telegram sidecar (Scope B3 / Milestone 4).

Imports the sidecar's PURE logic from amux-telegram.py (hyphenated -> loaded via
importlib) and drives the Bot orchestration with mocked network clients. No real
Telegram token, no server. Mirrors the AST-load style of the other test files but
uses importlib since the module is a real standalone file.

Covers (plan .omc/plans/chat-layer-auth.md sec 8, and the B3 task tests):
  * inbound idempotent-id derivation (same update_id -> same id)
  * offset-advance-only-after-ack (simulated crash before ack -> redelivery ->
    server dedups by id, offset advances on the retry's 200)
  * outbound exactly-once + transcript order over a mocked /api/chat feed
  * cache-rebuild simulation (rowid_seq renumbered, stable ids same -> no re-post,
    no stall — the C-crit-2 case)
  * non-owner message ignored (no amux write)
  * mute suppresses forwarding
"""
import importlib.util
import os
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SIDECAR = os.path.join(os.path.dirname(HERE), "amux-telegram.py")

_spec = importlib.util.spec_from_file_location("amux_telegram", SIDECAR)
tg = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tg)


# ── mock network clients ───────────────────────────────────────────────────────
class MockTelegram:
    def __init__(self):
        self.sent = []            # (chat_id, text, topic_id)
        self.created = []         # names
        self._next_topic = 1000

    def get_me(self):
        return {"id": 1, "username": "amux_test_bot"}

    def send_message(self, chat_id, text, topic_id=None):
        self.sent.append((chat_id, text, topic_id))
        return {"message_id": len(self.sent)}

    def create_forum_topic(self, chat_id, name):
        self.created.append(name)
        self._next_topic += 1
        return self._next_topic


class MockAmux:
    """Mock amux HTTP surface. `threads[session]` is the merged-thread the next
    get_chat returns; `posted` records POST /api/chat; `fail_posts` forces AmuxError
    to simulate amux being down (durable-ack failure)."""
    def __init__(self):
        self.threads = {}         # session -> {"thread": [...], "cursor": int}
        self.posted = []          # (session, text, origin, msg_id)
        self._seen_ids = set()    # server-side idempotency
        self.fail_posts = 0
        self.sessions = []

    def health(self):
        return {"status": "ok"}

    def list_sessions(self):
        return self.sessions

    def get_chat(self, session, since=0):
        data = self.threads.get(session, {"thread": [], "cursor": 0})
        thread = data["thread"]
        # emulate the server's since filter on rowid_seq for session replies
        if since:
            thread = [it for it in thread
                      if it.get("seq") is None or (it.get("seq") or 0) > since]
        return {"session": session, "thread": thread, "cursor": data.get("cursor", 0)}

    def post_chat(self, session, text, origin="telegram", msg_id=""):
        if self.fail_posts > 0:
            self.fail_posts -= 1
            raise tg.AmuxError("simulated amux down")
        deduped = msg_id in self._seen_ids
        if not deduped:
            self._seen_ids.add(msg_id)
            self.posted.append((session, text, origin, msg_id))
        return {"ok": True, "id": msg_id, "session": session, "deduped": deduped}

    def peek(self, session, lines=40):
        return "peek-output"

    def wake(self, session):
        return {"ok": True}

    def create_session(self, name, directory=""):
        return {"ok": True}


def make_bot(topics_state=None, outbound_state=None, offset=0, chat_id="-100999"):
    td = tempfile.mkdtemp()
    topics = tg.TopicStore(os.path.join(td, "topics.json"), topics_state or {})
    outbound = tg.OutboundTracker(os.path.join(td, "out.json"), outbound_state or {})
    off = tg.OffsetStore(os.path.join(td, "offset"), offset)
    cfg = {"owner_id": 42, "chat_id": chat_id, "amux_base": "x", "tg_api_base": "y",
           "write_token": "wt", "poll_secs": 0.01, "long_poll_secs": 1}
    mt, ma = MockTelegram(), MockAmux()
    bot = tg.Bot(cfg, mt, ma, topics, off, outbound)
    return bot, mt, ma, off


def owner_msg(update_id, text, topic_id=None, from_id=42):
    m = {"from": {"id": from_id, "username": "owner"}, "text": text}
    if topic_id is not None:
        m["message_thread_id"] = topic_id
    return {"update_id": update_id, "message": m}


# ── 1. idempotent inbound id ────────────────────────────────────────────────────
assert tg.derive_inbound_id(7) == tg.derive_inbound_id(7) == "tg-7"
assert tg.derive_inbound_id(7) != tg.derive_inbound_id(8)
print("id ok — same update_id -> same chat id, distinct ids distinct")


# ── 2. offset advance only after durable ack (crash before ack -> redelivery) ───
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessA": 100}})
ma.fail_posts = 1  # first POST raises (crash between TG-receive and amux-persist)
u = owner_msg(5, "hello world", topic_id=100)
# simulate the inbound loop body for one update:
try:
    bot.handle_update(u)
    advanced = True
except tg.AmuxError:
    advanced = False
assert not advanced, "must NOT advance on durable-ack failure"
assert off.get() == 0, f"offset advanced despite failure: {off.get()}"
assert ma.posted == [], "no message should be persisted after a failed POST"
# recovery: the SAME update is re-delivered; POST now succeeds; offset advances.
bot.handle_update(u)
off.advance_to(u["update_id"])
assert off.get() == 6, f"offset should advance to 6 after ack, got {off.get()}"
assert ma.posted == [("sessA", "hello world", "telegram", "tg-5")], ma.posted
# a further redelivery (double-fetch) dedups server-side, no second enqueue:
bot.handle_update(u)
assert len(ma.posted) == 1, f"redelivery must dedup by stable id: {ma.posted}"
print("offset ok — advances only after durable ack; crash re-delivers; server dedups by id")


# ── 3. non-owner ignored (no amux write) ────────────────────────────────────────
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessA": 100}})
bot.handle_update(owner_msg(9, "rm -rf /", topic_id=100, from_id=999))
assert ma.posted == [], "non-owner message must never reach amux"
print("owner-gate ok — non-owner message ignored, no write to amux")


# ── 4. outbound exactly-once + transcript order ─────────────────────────────────
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessA": 100}},
                            outbound_state={"sessA": {"last_seq": 0, "seen": []}})
# a merged thread: owner (skip), two session replies + one system, out of ts order
ma.threads["sessA"] = {"cursor": 2, "thread": [
    {"id": "owner-1", "role": "owner", "text": "go", "ts": 10, "seq": None},
    {"id": "C:2", "role": "session", "text": "second", "ts": 30, "seq": 2},
    {"id": "sys-1", "role": "system", "text": "usage limit", "ts": 25, "seq": None},
    {"id": "C:1", "role": "session", "text": "first", "ts": 20, "seq": 1},
]}
bot.forward_session("sessA")
forwarded = [t for (_c, t, _tid) in mt.sent]
assert forwarded == ["first", "⚙️ [sessA] usage limit", "second"], f"order wrong: {forwarded}"
assert all(tid == 100 for (_c, _t, tid) in mt.sent), "must post to the mapped topic"
# owner rows are never forwarded (they are inputs, not replies)
assert "go" not in forwarded, "owner input must not be forwarded outbound"
# re-poll same feed -> nothing new (exactly-once via stable-id dedup)
n_before = len(mt.sent)
bot.forward_session("sessA")
assert len(mt.sent) == n_before, "re-poll must not re-forward (exactly-once)"
print("outbound ok — session+system rows forwarded once, in transcript order, owner skipped")


# ── 5. cache-rebuild simulation (rowid_seq renumbered, stable ids same) ─────────
# Continue from state where C:1,C:2 already forwarded (seen). A rebuild renumbers
# seqs DOWNWARD (2,1) but stable ids are unchanged and cursor drops below our
# high-water. Must: no re-post (dedup by id) AND no stall (refetch-from-0).
assert bot.outbound.fetch_since("sessA") == 2, "high-water should be 2"
ma.threads["sessA"] = {"cursor": 1, "thread": [  # cursor 1 < our last_seq 2 -> rebuild
    {"id": "C:1", "role": "session", "text": "first", "ts": 20, "seq": 1},
    {"id": "C:2", "role": "session", "text": "second", "ts": 30, "seq": 2},
]}
n_before = len(mt.sent)
bot.forward_session("sessA")
assert len(mt.sent) == n_before, "rebuild must NOT re-forward already-seen replies"
# and a genuinely NEW reply after the rebuild still gets through (no stall):
ma.threads["sessA"] = {"cursor": 3, "thread": [
    {"id": "C:1", "role": "session", "text": "first", "ts": 20, "seq": 1},
    {"id": "C:2", "role": "session", "text": "second", "ts": 30, "seq": 2},
    {"id": "C:3", "role": "session", "text": "third", "ts": 40, "seq": 3},
]}
bot.forward_session("sessA")
assert [t for (_c, t, _tid) in mt.sent][-1] == "third", "post-rebuild new reply must forward (no stall)"
print("rebuild ok — renumbered seqs: no re-post (id dedup), no stall (new reply forwards)")


# ── 6. mute suppresses forwarding ───────────────────────────────────────────────
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessB": 200}, "muted": ["sessB"]},
                            outbound_state={"sessB": {"last_seq": 0, "seen": []}})
ma.threads["sessB"] = {"cursor": 1, "thread": [
    {"id": "D:1", "role": "session", "text": "hi", "ts": 10, "seq": 1}]}
bot.forward_session("sessB")
assert mt.sent == [], "muted session must not forward"
bot.topics.unmute("sessB")
bot.forward_session("sessB")
assert [t for (_c, t, _tid) in mt.sent] == ["hi"], "unmute resumes forwarding"
print("mute ok — muted topic suppresses forwarding; unmute resumes")


# ── 7. first-sight baseline: pre-existing history is NOT flooded ─────────────────
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessC": 300}})  # outbound empty
ma.threads["sessC"] = {"cursor": 2, "thread": [
    {"id": "E:1", "role": "session", "text": "old1", "ts": 10, "seq": 1},
    {"id": "E:2", "role": "session", "text": "old2", "ts": 20, "seq": 2}]}
bot.forward_session("sessC")
assert mt.sent == [], "first sight must baseline (not flood) pre-existing history"
# a new reply after baseline forwards normally:
ma.threads["sessC"] = {"cursor": 3, "thread": ma.threads["sessC"]["thread"] + [
    {"id": "E:3", "role": "session", "text": "new3", "ts": 30, "seq": 3}]}
bot.forward_session("sessC")
assert [t for (_c, t, _tid) in mt.sent] == ["new3"], "post-baseline new reply forwards once"
print("baseline ok — startup does not flood history; only new replies forward")


# ── 8. topic mapping persistence round-trip ─────────────────────────────────────
td = tempfile.mkdtemp()
p = os.path.join(td, "topics.json")
ts1 = tg.TopicStore(p, {})
ts1.set("sX", 55)
ts1.mute("sX")
ts1.save()
ts2 = tg.TopicStore.load(p)
assert ts2.topic_for_session("sX") == 55
assert ts2.session_for_topic(55) == "sX"
assert ts2.is_muted("sX")
print("topics ok — session<->topic map + mute persist and reload")


# ── 9. config perms enforcement ─────────────────────────────────────────────────
td = tempfile.mkdtemp()
cfgp = os.path.join(td, "telegram.env")
open(cfgp, "w").write("TG_BOT_TOKEN=t\nTG_OWNER_ID=42\n")
os.chmod(cfgp, 0o644)  # insecure
try:
    tg.load_config(cfgp, write_token_path=os.path.join(td, "nope"), environ={})
    raise AssertionError("insecure perms must be refused")
except tg.ConfigError as e:
    assert "insecure perms" in str(e)
os.chmod(cfgp, 0o600)
c = tg.load_config(cfgp, write_token_path=os.path.join(td, "nope"), environ={})
assert c["owner_id"] == 42 and c["bot_token"] == "t"
print("config ok — insecure perms refused; 0600 config parses")


print("\nALL TELEGRAM-SIDECAR CHECKS PASSED")


def test_telegram_sidecar():
    """The scenarios above execute at import time and raise on any regression."""
    assert True
