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
        self.raw_sent = []        # (session, text) via /type
        self.keys_sent = []       # (session, key) via /keys, one entry per key

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

    def raw_send(self, session, text):
        self.raw_sent.append((session, text))
        return {"ok": True}

    def send_key(self, session, key):
        self.keys_sent.append((session, key))
        return {"ok": True}


def make_bot(topics_state=None, outbound_state=None, offset=0, chat_id="-100999",
             summarizer=None, default_mode=None):
    td = tempfile.mkdtemp()
    topics = tg.TopicStore(os.path.join(td, "topics.json"), topics_state or {})
    outbound = tg.OutboundTracker(os.path.join(td, "out.json"), outbound_state or {})
    off = tg.OffsetStore(os.path.join(td, "offset"), offset)
    cfg = {"owner_id": 42, "chat_id": chat_id, "amux_base": "x", "tg_api_base": "y",
           "write_token": "wt", "poll_secs": 0.01, "long_poll_secs": 1}
    if default_mode:
        cfg["default_mode"] = default_mode
    mt, ma = MockTelegram(), MockAmux()
    bot = tg.Bot(cfg, mt, ma, topics, off, outbound, summarizer=summarizer)
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
assert c["default_mode"] == "smart", "TG_DEFAULT_MODE must default to smart for ALL topics"
assert c["summary_model"] == "haiku"
assert c["summary_timeout"] == 90.0
assert c["summary_config_dir"] is None
print("config ok — insecure perms refused; 0600 config parses")

# TG_DEFAULT_MODE / TG_SUMMARY_* overrides (env wins over file, per load_config's get())
c2 = tg.load_config(cfgp, write_token_path=os.path.join(td, "nope"), environ={
    "TG_DEFAULT_MODE": "full", "TG_SUMMARY_MODEL": "sonnet",
    "TG_SUMMARY_TIMEOUT": "30", "TG_SUMMARY_CONFIG_DIR": "/tmp/some-dir"})
assert c2["default_mode"] == "full"
assert c2["summary_model"] == "sonnet"
assert c2["summary_timeout"] == 30.0
assert c2["summary_config_dir"] == "/tmp/some-dir"

# invalid TG_DEFAULT_MODE falls back to smart rather than raising
c3 = tg.load_config(cfgp, write_token_path=os.path.join(td, "nope"),
                    environ={"TG_DEFAULT_MODE": "loud"})
assert c3["default_mode"] == "smart"
print("config ok — TG_DEFAULT_MODE / TG_SUMMARY_* env overrides parse; invalid mode falls back to smart")


# ── 10. /type: raw-inject preserves the exact argument, bypasses steering ──────
assert tg.command_raw_arg("/type  AB  CD!@#  123") == "AB  CD!@#  123", \
    "must preserve internal spacing/punctuation, not collapse to the first token"
assert tg.command_raw_arg("/type") == "", "no argument -> empty"
assert tg.command_raw_arg("/type\ttabbed\targ") == "tabbed\targ"
print("command_raw_arg ok — spacing/punctuation/tabs preserved verbatim, no-arg -> empty")

bot, mt, ma, off = make_bot(topics_state={"topics": {"sessA": 100}})
bot.handle_update(owner_msg(20, "/type  AB1-code_23!  ", topic_id=100))
assert ma.raw_sent == [("sessA", "AB1-code_23!")], ma.raw_sent
assert [t for (_c, t, _tid) in mt.sent] == ["typed ✓"], mt.sent
print("type ok — raw text injected verbatim (spaces/punctuation preserved), confirms in-topic")

# General topic (no message_thread_id) -> unmapped, refuse with an error, no amux write
bot2, mt2, ma2, off2 = make_bot(topics_state={"topics": {"sessA": 100}})
bot2.handle_update(owner_msg(21, "/type hello", topic_id=None))
assert ma2.raw_sent == [], "General/unmapped topic must not raw-inject"
assert any("mapped session topic" in t for (_c, t, _tid) in mt2.sent), mt2.sent
print("type ok — General/unmapped topic refuses with an error, no amux write")

# non-owner -> ignored entirely (never reaches the command dispatcher)
bot3, mt3, ma3, off3 = make_bot(topics_state={"topics": {"sessA": 100}})
bot3.handle_update(owner_msg(22, "/type sneaky", topic_id=100, from_id=999))
assert ma3.raw_sent == [], "non-owner /type must never reach amux"
print("type ok — non-owner /type ignored")


# ── 11. /keys: multiple keys sent in order, one call per key, bypasses steering ─
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessA": 100}})
bot.handle_update(owner_msg(23, "/keys C-c Tab Enter", topic_id=100))
assert ma.keys_sent == [("sessA", "C-c"), ("sessA", "Tab"), ("sessA", "Enter")], ma.keys_sent
assert [t for (_c, t, _tid) in mt.sent] == ["keys sent ✓"], mt.sent
print("keys ok — multiple keys sent in order, one call per key, confirms in-topic")

# unmapped (non-General) topic id -> refuse with an error, no amux write
bot2, mt2, ma2, off2 = make_bot()
bot2.handle_update(owner_msg(24, "/keys Enter", topic_id=777))
assert ma2.keys_sent == [], "unmapped topic must not send keys"
assert any("mapped session topic" in t for (_c, t, _tid) in mt2.sent), mt2.sent
print("keys ok — unmapped topic refuses with an error, no amux write")

# non-owner -> ignored entirely
bot3, mt3, ma3, off3 = make_bot(topics_state={"topics": {"sessA": 100}})
bot3.handle_update(owner_msg(25, "/keys Enter", topic_id=100, from_id=999))
assert ma3.keys_sent == [], "non-owner /keys must never reach amux"
print("keys ok — non-owner /keys ignored")


# ── 12. AmuxClient payload shapes for /type + /keys (real client, no mock) ──────
class _RecordingCall:
    def __init__(self):
        self.calls = []

    def __call__(self, method, path, params=None, body=None, timeout=20):
        self.calls.append((method, path, body))
        return 200, {"ok": True}


client = tg.AmuxClient("https://localhost:1", "wt")
rec = _RecordingCall()
client._call = rec
client.raw_send("sessA", "the-code")
client.send_key("sessA", "Enter")
assert rec.calls[0] == ("POST", "/api/sessions/sessA/send",
                        {"text": "the-code", "record_history": True}), rec.calls[0]
assert rec.calls[1] == ("POST", "/api/sessions/sessA/keys", {"keys": "Enter"}), rec.calls[1]
print("payload ok — raw_send body {'text','record_history':True}; send_key body {'keys':<one key>}")


# ── 13. "//" slash-command pass-through ─────────────────────────────────────────
# "//ralph fix  double  spaces" -> forwards "/ralph fix  double  spaces" verbatim
# (internal spacing preserved, no whitespace-collapsing parse) via the SAME chat
# pipeline as a plain message: same session, same idempotent id derivation.
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessA": 100}})
u = owner_msg(30, "//ralph fix  double  spaces", topic_id=100)
bot.handle_update(u)
assert ma.posted == [("sessA", "/ralph fix  double  spaces", "telegram", "tg-30")], ma.posted
bot.offset.advance_to(u["update_id"])
print("// ok — pass-through forwards with exactly one leading slash stripped, spacing preserved")

# bare "//" alone -> usage hint, no forward (not even an empty command)
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessA": 100}})
bot.handle_update(owner_msg(31, "//", topic_id=100))
assert ma.posted == [], "bare // must not forward"
assert any("usage:" in t for (_c, t, _tid) in mt.sent), mt.sent
print("// ok — bare // replies a usage hint, forwards nothing")

# single "/" (e.g. "/ralph") is NOT the pass-through — it is an unknown bot command
# and hits the help path, never the session.
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessA": 100}})
bot.handle_update(owner_msg(32, "/ralph fix tests", topic_id=100))
assert ma.posted == [], "single-slash command must never reach amux as a pass-through"
assert any("commands:" in t for (_c, t, _tid) in mt.sent), mt.sent
print("// ok — single-slash /ralph still parses as an (unknown) bot command, not pass-through")

# non-owner "//x" ignored entirely, same gate as everything else
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessA": 100}})
bot.handle_update(owner_msg(33, "//ralph fix tests", topic_id=100, from_id=999))
assert ma.posted == [], "non-owner // pass-through must never reach amux"
print("// ok — non-owner // message ignored")

# unmapped topic -> same "no session mapped" behavior as plain text there
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessA": 100}})
bot.handle_update(owner_msg(34, "//ralph fix tests", topic_id=777))
assert ma.posted == [], "unmapped topic must not forward"
assert any("No session is mapped" in t for (_c, t, _tid) in mt.sent), mt.sent
print("// ok — unmapped topic behaves like plain text (no session mapped reply)")


# ── 14. smart mode (default): long reply summarized, carries ≡ prefix/suffix ───
LONG_REPLY = "Implementoval jsem novou funkci a opravil několik chyb. " * 20
assert len(LONG_REPLY) > tg.SHORT_REPLY_CHARS, "fixture must exceed the short-reply bypass"


def mock_summarize_ok(text):
    assert text == LONG_REPLY, "summarizer must receive the full reply text (under the stdin cap)"
    return "Úkol dokončen, vše v pořádku."


bot, mt, ma, off = make_bot(topics_state={"topics": {"sessA": 100}},
                            outbound_state={"sessA": {"last_seq": 0, "seen": []}},
                            summarizer=mock_summarize_ok)
ma.threads["sessA"] = {"cursor": 1, "thread": [
    {"id": "S:1", "role": "session", "text": LONG_REPLY, "ts": 10, "seq": 1}]}
bot.forward_session("sessA")
sent = [t for (_c, t, _tid) in mt.sent]
assert sent == [tg.SMART_PREFIX + "Úkol dokončen, vše v pořádku." + tg.SMART_SUFFIX], sent
print("smart mode ok — long reply summarized, carries ≡ prefix + /last suffix")


# ── 15. summarizer failure (None or raise) -> brief fallback, exactly-once ─────
def mock_summarize_none(text):
    return None  # mirrors Summarizer.summarize()'s own contract on any internal failure


bot, mt, ma, off = make_bot(topics_state={"topics": {"sessB": 200}},
                            outbound_state={"sessB": {"last_seq": 0, "seen": []}},
                            summarizer=mock_summarize_none)
ma.threads["sessB"] = {"cursor": 1, "thread": [
    {"id": "S:1", "role": "session", "text": LONG_REPLY, "ts": 10, "seq": 1}]}
bot.forward_session("sessB")
sent = [t for (_c, t, _tid) in mt.sent]
assert sent == [tg.brief_truncate(LONG_REPLY)], sent
assert tg.SMART_PREFIX not in sent[0], "a fallback message must not carry the smart-mode prefix"
n_before = len(mt.sent)
bot.forward_session("sessB")
assert len(mt.sent) == n_before, "fallback-delivered reply must still be exactly-once"


def mock_summarize_raises(text):
    raise RuntimeError("boom")


bot2, mt2, ma2, off2 = make_bot(topics_state={"topics": {"sessB2": 201}},
                                outbound_state={"sessB2": {"last_seq": 0, "seen": []}},
                                summarizer=mock_summarize_raises)
ma2.threads["sessB2"] = {"cursor": 1, "thread": [
    {"id": "S:1", "role": "session", "text": LONG_REPLY, "ts": 10, "seq": 1}]}
bot2.forward_session("sessB2")
assert [t for (_c, t, _tid) in mt2.sent] == [tg.brief_truncate(LONG_REPLY)]
print("smart fallback ok — summarizer failure (None / exception) falls back to brief, exactly-once, "
      "content still delivered")


# ── 16. short replies bypass the summarizer entirely ────────────────────────────
_calls = []


def mock_summarize_track(text):
    _calls.append(text)
    return "should not be used"


bot, mt, ma, off = make_bot(topics_state={"topics": {"sessC": 300}},
                            outbound_state={"sessC": {"last_seq": 0, "seen": []}},
                            summarizer=mock_summarize_track)
SHORT_REPLY = "all done, quick fix applied."
assert len(SHORT_REPLY) < tg.SHORT_REPLY_CHARS
ma.threads["sessC"] = {"cursor": 1, "thread": [
    {"id": "S:1", "role": "session", "text": SHORT_REPLY, "ts": 10, "seq": 1}]}
bot.forward_session("sessC")
assert _calls == [], "short reply must bypass the summarizer entirely"
assert [t for (_c, t, _tid) in mt.sent] == [SHORT_REPLY], mt.sent
print("short-reply bypass ok — replies under the threshold skip the summarizer, forwarded verbatim")


# ── 17. system rows always verbatim — never summarized, even when long ─────────
_calls2 = []


def mock_summarize_track2(text):
    _calls2.append(text)
    return "x"


LONG_SYSTEM = "usage limit details: " + ("detail " * 100)
assert len(LONG_SYSTEM) > tg.SHORT_REPLY_CHARS
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessD": 400}},
                            outbound_state={"sessD": {"last_seq": 0, "seen": []}},
                            summarizer=mock_summarize_track2)
ma.threads["sessD"] = {"cursor": 1, "thread": [
    {"id": "SYS:1", "role": "system", "text": LONG_SYSTEM, "ts": 10, "seq": None}]}
bot.forward_session("sessD")
assert _calls2 == [], "system rows must never be summarized"
assert [t for (_c, t, _tid) in mt.sent] == [f"⚙️ [sessD] {LONG_SYSTEM}"], mt.sent
print("system-row ok — system rows always forwarded verbatim, never summarized/truncated")


# ── 18. brief mode: deterministic truncation, summarizer never invoked ─────────
_calls3 = []


def mock_summarize_track3(text):
    _calls3.append(text)
    return "x"


bot, mt, ma, off = make_bot(topics_state={"topics": {"sessE": 500}, "modes": {"sessE": "brief"}},
                            outbound_state={"sessE": {"last_seq": 0, "seen": []}},
                            summarizer=mock_summarize_track3)
ma.threads["sessE"] = {"cursor": 1, "thread": [
    {"id": "S:1", "role": "session", "text": LONG_REPLY, "ts": 10, "seq": 1}]}
bot.forward_session("sessE")
assert _calls3 == [], "brief mode must never call the summarizer"
assert [t for (_c, t, _tid) in mt.sent] == [tg.brief_truncate(LONG_REPLY)], mt.sent
print("brief mode ok — deterministic truncation, summarizer never invoked")


# ── 19. full mode: verbatim, summarizer never invoked, no truncation ───────────
_calls4 = []


def mock_summarize_track4(text):
    _calls4.append(text)
    return "x"


bot, mt, ma, off = make_bot(topics_state={"topics": {"sessF": 600}, "modes": {"sessF": "full"}},
                            outbound_state={"sessF": {"last_seq": 0, "seen": []}},
                            summarizer=mock_summarize_track4)
ma.threads["sessF"] = {"cursor": 1, "thread": [
    {"id": "S:1", "role": "session", "text": LONG_REPLY, "ts": 10, "seq": 1}]}
bot.forward_session("sessF")
assert _calls4 == [], "full mode must never call the summarizer"
assert [t for (_c, t, _tid) in mt.sent] == [LONG_REPLY], mt.sent
print("full mode ok — verbatim forward, summarizer never invoked, no truncation")


# ── 20. smart mode: server-provided summary is preferred, local summarizer skipped ─
# (docs/reply-summary.md) — the reply item carries a server `summary` (either the
# owner's own "⌁" marker or the background Haiku fill-in); the sidecar must use it
# directly and never spend a local `claude -p` call on the same reply.
_calls5 = []


def mock_summarize_track5(text):
    _calls5.append(text)
    return "should not be used — server summary must win"


bot, mt, ma, off = make_bot(topics_state={"topics": {"sessH": 800}},
                            outbound_state={"sessH": {"last_seq": 0, "seen": []}},
                            summarizer=mock_summarize_track5)
ma.threads["sessH"] = {"cursor": 1, "thread": [
    {"id": "S:1", "role": "session", "text": LONG_REPLY, "ts": 10, "seq": 1,
     "summary": "Hotovo, vse v poradku."}]}
bot.forward_session("sessH")
assert _calls5 == [], "server-provided summary must skip the local summarizer entirely"
assert [t for (_c, t, _tid) in mt.sent] == [tg.SMART_PREFIX + "Hotovo, vse v poradku." + tg.SMART_SUFFIX], mt.sent
print("server-summary ok — server summary preferred over local summarizer, carries same prefix/suffix")


# ── 21. smart mode: no server summary -> local summarizer still called as before ──
def mock_summarize_track6(text):
    assert text == LONG_REPLY
    return "Lokalni shrnuti."


bot, mt, ma, off = make_bot(topics_state={"topics": {"sessI": 801}},
                            outbound_state={"sessI": {"last_seq": 0, "seen": []}},
                            summarizer=mock_summarize_track6)
# No "summary" key at all — mirrors an older server / a reply the server hasn't
# filled in yet.
ma.threads["sessI"] = {"cursor": 1, "thread": [
    {"id": "S:1", "role": "session", "text": LONG_REPLY, "ts": 10, "seq": 1}]}
bot.forward_session("sessI")
assert [t for (_c, t, _tid) in mt.sent] == [tg.SMART_PREFIX + "Lokalni shrnuti." + tg.SMART_SUFFIX], mt.sent
print("no-server-summary ok — absent server summary falls back to the local summarizer as before")


# ── 22. /mode: show/set per-topic override, persists, validates, owner+mapped ──
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessG": 700}})
bot.handle_update(owner_msg(40, "/mode", topic_id=700))
assert [t for (_c, t, _tid) in mt.sent] == ["mode: smart (default)"], mt.sent

bot.handle_update(owner_msg(41, "/mode brief", topic_id=700))
assert bot.topics.mode_for_session("sessG") == "brief"
assert [t for (_c, t, _tid) in mt.sent][-1] == "mode set to brief for sessG", mt.sent

reloaded = tg.TopicStore.load(bot.topics.path)
assert reloaded.mode_for_session("sessG") == "brief", "mode override must persist to disk"

bot.handle_update(owner_msg(42, "/mode", topic_id=700))
assert [t for (_c, t, _tid) in mt.sent][-1] == "mode: brief", mt.sent

bot.handle_update(owner_msg(43, "/mode loud", topic_id=700))
assert bot.topics.mode_for_session("sessG") == "brief", "invalid mode must not change stored mode"
assert [t for (_c, t, _tid) in mt.sent][-1] == "usage: /mode smart|brief|full", mt.sent

bot2, mt2, ma2, off2 = make_bot()
bot2.handle_update(owner_msg(44, "/mode smart", topic_id=999))
assert any("mapped session topic" in t for (_c, t, _tid) in mt2.sent), mt2.sent

bot3, mt3, ma3, off3 = make_bot(topics_state={"topics": {"sessG": 700}})
bot3.handle_update(owner_msg(45, "/mode brief", topic_id=700, from_id=999))
assert bot3.topics.mode_for_session("sessG") is None, "non-owner /mode must not change stored mode"
print("mode ok — /mode shows/sets per-topic override, persists, validates, owner+mapped gated")


# ── 23. /last: full (unsummarized) text of the n-th most recent reply ──────────
bot, mt, ma, off = make_bot(topics_state={"topics": {"sessH": 800}})
LONG_SECOND = "second reply full text " * 50
ma.threads["sessH"] = {"cursor": 3, "thread": [
    {"id": "S:1", "role": "session", "text": "first reply full text", "ts": 10, "seq": 1},
    {"id": "SYS:1", "role": "system", "text": "usage note", "ts": 15, "seq": None},
    {"id": "S:2", "role": "session", "text": LONG_SECOND, "ts": 20, "seq": 2},
    {"id": "S:3", "role": "session", "text": "third reply full text", "ts": 30, "seq": 3},
]}
bot.handle_update(owner_msg(50, "/last", topic_id=800))
assert [t for (_c, t, _tid) in mt.sent] == ["third reply full text"], mt.sent

bot.handle_update(owner_msg(51, "/last 2", topic_id=800))
assert [t for (_c, t, _tid) in mt.sent][-1] == LONG_SECOND, "must return the FULL text, not truncated"

bot.handle_update(owner_msg(52, "/last 99", topic_id=800))
assert "available" in [t for (_c, t, _tid) in mt.sent][-1]

bot.handle_update(owner_msg(53, "/last abc", topic_id=800))
assert [t for (_c, t, _tid) in mt.sent][-1] == "usage: /last [n]"

bot2, mt2, ma2, off2 = make_bot()
bot2.handle_update(owner_msg(54, "/last", topic_id=999))
assert any("mapped session topic" in t for (_c, t, _tid) in mt2.sent), mt2.sent

bot3, mt3, ma3, off3 = make_bot(topics_state={"topics": {"sessH": 800}})
ma3.threads["sessH"] = ma.threads["sessH"]
bot3.handle_update(owner_msg(55, "/last", topic_id=800, from_id=999))
assert mt3.sent == [], "non-owner /last must be ignored"
print("last ok — /last returns full (unsummarized) text of the n-th most recent reply, validated, gated")


# ── 24. Summarizer: injected runner — never invokes a real subprocess ──────────
class _FakeCompletedProcess:
    def __init__(self, returncode=0, stdout=""):
        self.returncode = returncode
        self.stdout = stdout


class _FakeRunner:
    """Records every call; returns (or raises) whatever result is queued next."""
    def __init__(self):
        self.calls = []
        self._queue = []

    def queue(self, result):
        self._queue.append(result)

    def __call__(self, cmd, input, capture_output, text, timeout, env):
        self.calls.append({"cmd": cmd, "input": input, "timeout": timeout, "env": env})
        result = self._queue.pop(0)
        if isinstance(result, Exception):
            raise result
        return result


runner = _FakeRunner()
summ = tg.Summarizer(model="haiku", timeout=42, config_dir="/tmp/fake-cfg",
                     claude_bin="/tmp/fake/claude", runner=runner)

runner.queue(_FakeCompletedProcess(0, "  Krátké shrnutí.  \n"))
assert summ.summarize("some long reply text") == "Krátké shrnutí."
call = runner.calls[-1]
assert call["cmd"] == ["/tmp/fake/claude", "-p", tg.SUMMARY_PROMPT, "--model", "haiku"], call["cmd"]
assert call["timeout"] == 42
assert call["env"]["CLAUDE_CONFIG_DIR"] == "/tmp/fake-cfg"
assert call["env"]["PATH"].startswith("/tmp/fake:"), call["env"]["PATH"]
assert call["env"].get("USER"), "USER must be populated even under a minimal (launchd-style) env"
assert "CLAUDECODE" not in call["env"] and "CLAUDE_CODE_ENTRYPOINT" not in call["env"], \
    "child env is built from scratch — no recursive-invocation vars leak through"

runner.queue(_FakeCompletedProcess(1, ""))            # nonzero exit
assert summ.summarize("x") is None
runner.queue(_FakeCompletedProcess(0, "   \n"))        # empty output
assert summ.summarize("x") is None
runner.queue(tg.subprocess.TimeoutExpired(cmd="claude", timeout=42))  # timeout
assert summ.summarize("x") is None
runner.queue(OSError("no such file"))                  # binary missing / exec failure
assert summ.summarize("x") is None
print("Summarizer ok — env built explicitly (USER populated, no CLAUDECODE leak); "
      "ANY failure mode (nonzero/empty/timeout/OSError) returns None, never raises")

big = ("A" * 9000) + "MIDDLE" + ("B" * 9000)
capped = tg._cap_stdin(big)
assert capped.startswith("A" * 8000) and capped.endswith("B" * 4000)
assert len(capped) == 8000 + len("\n…\n") + 4000
assert tg._cap_stdin("short text") == "short text"
print("cap_stdin ok — oversized input capped to first 8k + elision + last 4k; short input untouched")


print("\nALL TELEGRAM-SIDECAR CHECKS PASSED")


def test_telegram_sidecar():
    """The scenarios above execute at import time and raise on any regression."""
    assert True
