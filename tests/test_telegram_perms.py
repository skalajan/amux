"""Deterministic tests for the Telegram permission-prompt notify + remote answer
(plan .omc/plans/telegram-permissions.md, Phase B).

Imports amux-telegram.py via importlib (hyphenated filename, same style as
tests/test_telegram_sidecar.py) and drives the pure classifier / fingerprint /
callback-data / grace-window logic plus the Bot orchestration with mocked
network clients. No real Telegram token, no server. Assertions execute at import
time and raise on any regression; a trailing test_telegram_perms() stub exists
for collection if pytest is ever available.

Covers:
  * prompt classifier: 3-option permission menu, 2-option y/n, MCP "Allow X to Y",
    AskUserQuestion multi-option (open-ended, NOT menu-answerable), rate-limit
    menu (no buttons), plain busy text (not a prompt)
  * fingerprint stability across ANSI / timestamp / elapsed-spinner / whitespace,
    and distinct prompt -> distinct fp
  * callback_data round-trip + parse rejection + callback owner-check
  * grace-window state machine: waiting start/stop/re-enter
  * notify: no notify before grace, exactly one after, correct topic, rings even
    with /ring off, suppressed by /mute; dedup on unchanged fp; supersede on change
  * callbacks: Allow -> "1", Always -> "2", Deny -> Escape; stale-fp rejection ->
    no injection; idempotency (a second tap after answer never re-injects);
    inject-failure resolves cleanly; a failing callback advances the offset and
    never stalls a following message's durable ack
"""
import importlib.util
import os
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SIDECAR = os.path.join(os.path.dirname(HERE), "amux-telegram.py")

_spec = importlib.util.spec_from_file_location("amux_telegram", SIDECAR)
tg = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tg)


# ── 1. prompt classifier ────────────────────────────────────────────────────────
MENU_3 = (
    "Bash(rm -rf build/)\n\n"
    "Do you want to proceed?\n"
    "❯ 1. Yes\n"
    "  2. Yes, and don't ask again for rm commands in /repo\n"
    "  3. No, and tell Claude what to do differently (esc)\n")
c = tg.classify_prompt(MENU_3)
assert c == {"kind": "menu", "options": 3, "always": True}, c

MENU_2 = (
    "Do you want to proceed?\n"
    "❯ 1. Yes\n"
    "  2. No (esc)\n")
c = tg.classify_prompt(MENU_2)
assert c == {"kind": "menu", "options": 2, "always": False}, c

MCP_ALLOW = (
    "Allow amux-telegram to run the tool `send_text`?\n"
    "❯ 1. Yes\n"
    "  2. No (esc)\n")
c = tg.classify_prompt(MCP_ALLOW)
assert c["kind"] == "menu" and c["always"] is False, c

# AskUserQuestion renders a ❯ menu too, but carries NO permission safety-chrome —
# it must be open-ended (no menu buttons), never mistaken for a permission prompt.
ASK_USER = (
    "Which approach should I take for the refactor?\n"
    "❯ 1. Extract a helper module\n"
    "  2. Inline everything\n"
    "  3. Leave it as-is\n")
c = tg.classify_prompt(ASK_USER)
assert c == {"kind": "open", "options": 0, "always": False}, c

# a rate-limit menu must NEVER be offered permission buttons
RATE_LIMIT = (
    "Claude usage limit reached. Your limit will reset at 3pm.\n"
    "❯ 1. Continue anyway\n"
    "  2. Cancel\n")
c = tg.classify_prompt(RATE_LIMIT)
assert c == {"kind": "none", "options": 0, "always": False}, c

# plain busy/generating text is not a prompt at all
BUSY = "Running the test suite...\n✻ Thinking (12s)\n  · updated 3 files\n"
assert tg.classify_prompt(BUSY)["kind"] == "none", tg.classify_prompt(BUSY)

# a free-text open question (no menu) -> open-ended
OPEN_Q = "I need the staging DB password before I can continue. What is it?"
assert tg.classify_prompt(OPEN_Q)["kind"] == "open", tg.classify_prompt(OPEN_Q)
print("classifier ok — 3-opt menu(+always), 2-opt menu(no always), MCP allow-to, "
      "AskUserQuestion=open, rate-limit/busy=none, free-text question=open")


# ── 2. fingerprint stability + distinctness ─────────────────────────────────────
BASE = "Do you want to proceed?\n❯ 1. Yes\n  2. No\n"
ANSI = ("\x1b[38;5;239mDo you want to proceed?\x1b[0m\n"
        "\x1b[1m❯ 1. Yes\x1b[0m\n   2. No   \n")   # ANSI + extra whitespace
assert tg.prompt_fingerprint(BASE) == tg.prompt_fingerprint(ANSI), \
    "ANSI + whitespace differences must normalize to the same fp"

# a volatile clock timestamp + elapsed spinner must not change the fp
CLOCK_A = "[3:04 PM] Do you want to proceed? (5s · esc to interrupt)\n❯ 1. Yes\n  2. No\n"
CLOCK_B = "[9:57 AM] Do you want to proceed? (48s · esc to interrupt)\n❯ 1. Yes\n  2. No\n"
assert tg.prompt_fingerprint(CLOCK_A) == tg.prompt_fingerprint(CLOCK_B), \
    "clock timestamps + elapsed spinners must be normalized out of the fp"

# a genuinely different prompt hashes differently
assert tg.prompt_fingerprint(BASE) != tg.prompt_fingerprint(
    "Do you want to proceed?\n❯ 1. Yes\n  2. Yes, and don't ask again\n  3. No\n"), \
    "a distinct prompt (extra option) must hash differently"
assert len(tg.prompt_fingerprint(BASE)) == 8, "fp is 8 hex chars"
print("fp ok — stable across ANSI/timestamp/spinner/whitespace; distinct prompt distinct; 8 hex")


# ── 3. callback_data round-trip + parse rejection ───────────────────────────────
assert tg.build_callback_data("allow", "sessA", "a1b2c3d4") == "perm:allow:sessA:a1b2c3d4"
assert tg.parse_callback_data("perm:allow:sessA:a1b2c3d4") == ("allow", "sessA", "a1b2c3d4")
assert tg.parse_callback_data("perm:always:s:ff00ff00") == ("always", "s", "ff00ff00")
assert tg.parse_callback_data("perm:deny:s:ff00ff00") == ("deny", "s", "ff00ff00")
assert tg.parse_callback_data("perm:peek:s:ff00ff00") == ("peek", "s", "ff00ff00")
# a session name containing ':' still round-trips (fp split off the tail first)
assert tg.build_callback_data("allow", "a:b", "dead") == "perm:allow:a:b:dead"
assert tg.parse_callback_data("perm:allow:a:b:dead") == ("allow", "a:b", "dead")
# malformed / hostile payloads reject
assert tg.parse_callback_data("") is None
assert tg.parse_callback_data("nope:allow:s:fp") is None
assert tg.parse_callback_data("perm:bogus:s:fp") is None, "unknown action rejected"
assert tg.parse_callback_data("perm:allow:s") is None, "missing fp rejected"
assert tg.parse_callback_data("perm:allow::fp") is None, "empty session rejected"
print("callback_data ok — round-trips (incl. ':' in session), rejects malformed/unknown-action")


# ── 4. callback owner-check ─────────────────────────────────────────────────────
def cb_update(update_id, action, session, fp, from_id=42, message_id=555, cb_id="cb1"):
    return {"update_id": update_id, "callback_query": {
        "id": cb_id, "from": {"id": from_id},
        "data": tg.build_callback_data(action, session, fp),
        "message": {"message_id": message_id, "message_thread_id": 100}}}


assert tg.is_callback_owner(cb_update(1, "allow", "s", "fp", from_id=42), 42) is True
assert tg.is_callback_owner(cb_update(1, "allow", "s", "fp", from_id=999), 42) is False
assert tg.is_callback_owner({"callback_query": {}}, 42) is False, "no from -> not owner"
print("owner-check ok — callback_query.from gates; non-owner and malformed rejected")


# ── 5. grace-window state machine ───────────────────────────────────────────────
wt = tg.WaitingTracker()
wt.observe("s", "active", 0.0)
assert wt.waiting_since("s") is None, "active does not start the timer"
wt.observe("s", "waiting", 100.0)
assert wt.waiting_since("s") == 100.0
wt.observe("s", "waiting", 105.0)
assert wt.waiting_since("s") == 100.0, "staying waiting keeps the original start"
assert not wt.due("s", 109.0, 10.0), "9s < 10s grace -> not due"
assert wt.due("s", 111.0, 10.0), "11s >= 10s grace -> due"
wt.observe("s", "idle", 112.0)
assert wt.waiting_since("s") is None, "leaving waiting resets the timer"
wt.observe("s", "waiting", 113.0)
assert wt.waiting_since("s") == 113.0, "re-entering waiting starts a fresh window"
assert not wt.due("s", 120.0, 10.0), "fresh window: 7s in, not due"
print("grace ok — timer starts on waiting, holds across polls, resets on leave, re-enters fresh")


# ── mock network clients (extended for callbacks/edits/peek text) ───────────────
class MockTelegram:
    def __init__(self):
        self.sent = []        # (chat_id, text, topic_id, reply_markup)
        self.silent = []      # disable_notification per send
        self.edits = []       # (message_id, text)
        self.answers = []     # (callback_id, text)
        self._mid = 1000
        self.fail_answer = False
        self.fail_edit = False

    def get_me(self):
        return {"id": 1, "username": "amux_test_bot"}

    def send_message(self, chat_id, text, topic_id=None, disable_notification=False,
                     reply_markup=None):
        self._mid += 1
        self.sent.append((chat_id, text, topic_id, reply_markup))
        self.silent.append(bool(disable_notification))
        return {"message_id": self._mid}

    def edit_message_text(self, chat_id, message_id, text, reply_markup=None):
        if self.fail_edit:
            raise tg.TelegramError("edit boom")
        self.edits.append((message_id, text))
        return {"message_id": message_id}

    def answer_callback(self, callback_id, text="", show_alert=False):
        if self.fail_answer:
            raise tg.TelegramError("answer boom")
        self.answers.append((callback_id, text))
        return {"ok": True}

    def create_forum_topic(self, chat_id, name):
        self._mid += 1
        return self._mid


class MockAmux:
    def __init__(self):
        self.sessions = []          # /api/sessions rows
        self.peek_text = {}         # session -> pane text
        self.raw_sent = []          # (session, text)
        self.keys_sent = []         # (session, key)
        self.posted = []            # (session, text, origin, msg_id)
        self._seen_ids = set()
        self.raw_should_fail = set()
        self.key_should_fail = set()

    def list_sessions(self):
        return self.sessions

    def peek(self, session, lines=40):
        return self.peek_text.get(session, "")

    def raw_send(self, session, text):
        if session in self.raw_should_fail:
            raise tg.AmuxError("session gone")
        self.raw_sent.append((session, text))
        return {"ok": True}

    def send_key(self, session, key):
        if session in self.key_should_fail:
            raise tg.AmuxError("session gone")
        self.keys_sent.append((session, key))
        return {"ok": True}

    def post_chat(self, session, text, origin="telegram", msg_id=""):
        deduped = msg_id in self._seen_ids
        if not deduped:
            self._seen_ids.add(msg_id)
            self.posted.append((session, text, origin, msg_id))
        return {"ok": True, "deduped": deduped}

    def get_chat(self, session, since=0):
        return {"session": session, "thread": [], "cursor": 0}


def make_bot(topics_state=None, prompts_state=None, chat_id="-100999"):
    td = tempfile.mkdtemp()
    topics = tg.TopicStore(os.path.join(td, "topics.json"), topics_state or {})
    outbound = tg.OutboundTracker(os.path.join(td, "out.json"), {})
    off = tg.OffsetStore(os.path.join(td, "offset"), 0)
    prompts = tg.PromptStore(os.path.join(td, "prompts.json"), prompts_state or {})
    cfg = {"owner_id": 42, "chat_id": chat_id, "amux_base": "x", "tg_api_base": "y",
           "write_token": "wt", "poll_secs": 0.01, "long_poll_secs": 1,
           "machine_label": "testbox"}
    mt, ma = MockTelegram(), MockAmux()
    bot = tg.Bot(cfg, mt, ma, topics, off, outbound, summarizer=None, prompts=prompts)
    return bot, mt, ma, off


def sess_row(name, status="waiting", **extra):
    return dict({"name": name, "status": status}, **extra)


# ── 6. notify: grace gating, single fire, topic, ring, dedup ─────────────────────
bot, mt, ma = make_bot(topics_state={"topics": {"sessA": 100}})[:3]
ma.peek_text["sessA"] = MENU_3
ma.sessions = [sess_row("sessA", "waiting")]

# below grace -> no notify
bot.waiting.observe("sessA", "waiting", 1000.0)  # seed a known start
# manually drive one detection cycle "now" just under grace
bot._maybe_notify_prompt  # sanity: exists
# simulate the poll pass at t = start + 5s (not due)
bot.waiting._since["sessA"] = 1000.0
# not due -> _check_permission_prompts must not notify
_saved_time = tg.time.time
tg.time.time = lambda: 1005.0
bot._check_permission_prompts([sess_row("sessA", "waiting")])
assert mt.sent == [], "must NOT notify before the grace window elapses"

# past grace -> exactly one notify, to the mapped topic, ringing, with a keyboard
tg.time.time = lambda: 1011.0
bot._check_permission_prompts([sess_row("sessA", "waiting")])
assert len(mt.sent) == 1, f"exactly one notify after grace: {len(mt.sent)}"
chat_id, text, topic_id, kb = mt.sent[0]
assert topic_id == 100, "notify must go to the mapped session topic"
assert mt.silent[0] is False, "a permission notify must RING"
assert "🔐 sessA" in text and "testbox" in text, text
assert kb and "inline_keyboard" in kb, "menu prompt must carry an inline keyboard"
# 3-option menu -> Allow + Always present
labels = [b["text"] for row in kb["inline_keyboard"] for b in row]
assert "✅ Allow" in labels and "✅ Always" in labels and "⛔ Deny" in labels and "👁 Peek" in labels, labels

# re-poll, same prompt (unchanged fp) -> no second notify (dedup)
tg.time.time = lambda: 1013.0
bot._check_permission_prompts([sess_row("sessA", "waiting")])
assert len(mt.sent) == 1, "unchanged fp must not re-notify"
tg.time.time = _saved_time
print("notify ok — silent before grace, one ringing notify after (mapped topic, keyboard), fp dedup")


# ── 7. 2-option menu -> no Always button; open-ended -> peek-only, no menu ───────
bot, mt, ma = make_bot(topics_state={"topics": {"s2": 200}})[:3]
ma.peek_text["s2"] = MENU_2
_t = tg.time.time
tg.time.time = lambda: 5000.0
bot.waiting._since["s2"] = 4980.0  # already past grace
bot._check_permission_prompts([sess_row("s2", "waiting")])
kb = mt.sent[-1][3]
labels = [b["text"] for row in kb["inline_keyboard"] for b in row]
assert "✅ Always" not in labels, "2-option menu must NOT offer Always (option 2 = deny)"
assert "✅ Allow" in labels and "⛔ Deny" in labels, labels

bot2, mt2, ma2 = make_bot(topics_state={"topics": {"s3": 300}})[:3]
ma2.peek_text["s3"] = ASK_USER
bot2.waiting._since["s3"] = 4980.0
bot2._check_permission_prompts([sess_row("s3", "waiting")])
kb2 = mt2.sent[-1][3]
labels2 = [b["text"] for row in kb2["inline_keyboard"] for b in row]
assert labels2 == ["👁 Peek"], f"open-ended prompt must be peek-only (no menu buttons): {labels2}"
assert "/type" in mt2.sent[-1][1], "open-ended notify must hint /type"
tg.time.time = _t
print("shape ok — 2-opt menu drops Always; AskUserQuestion is peek-only with a /type hint")


# ── 8. /mute suppresses; a rate-limit / busy pane never notifies ────────────────
bot, mt, ma = make_bot(topics_state={"topics": {"sM": 400}, "muted": ["sM"]})[:3]
ma.peek_text["sM"] = MENU_3
_t = tg.time.time
tg.time.time = lambda: 6000.0
bot.waiting._since["sM"] = 5980.0
bot._check_permission_prompts([sess_row("sM", "waiting")])
assert mt.sent == [], "/mute must suppress the permission notify entirely"

bot2, mt2, ma2 = make_bot(topics_state={"topics": {"sR": 401}})[:3]
ma2.peek_text["sR"] = RATE_LIMIT
bot2.waiting._since["sR"] = 5980.0
bot2._check_permission_prompts([sess_row("sR", "waiting")])
assert mt2.sent == [], "a rate-limit pane must never produce a permission notify"
tg.time.time = _t
print("suppress ok — /mute suppresses; rate-limit/none-shape pane produces no notify")


# ── 9. callbacks: Allow -> "1", Always -> "2", Deny -> Escape ───────────────────
def fresh_bot_with_pending(session, topic, pane, fp=None):
    bot, mt, ma = make_bot(topics_state={"topics": {session: topic}})[:3]
    ma.peek_text[session] = pane
    ma.sessions = [sess_row(session, "waiting")]
    fp = fp or tg.prompt_fingerprint(pane)
    bot.prompts.set(session, fp, 900, 1.0, "menu", body=f"🔐 {session} needs a decision")
    return bot, mt, ma, fp


bot, mt, ma, fp = fresh_bot_with_pending("sA", 100, MENU_3)
bot.handle_callback(cb_update(1, "allow", "sA", fp, message_id=900))
assert ma.raw_sent == [("sA", "1")], f"Allow must send '1': {ma.raw_sent}"
assert ma.keys_sent == []
assert mt.answers and "Allowed" in mt.answers[-1][1], mt.answers
assert mt.edits and "Allowed" in mt.edits[-1][1], mt.edits
assert bot.prompts.get("sA")["answered"] is True, "answered flag set (dedup + quiet resolve)"

bot, mt, ma, fp = fresh_bot_with_pending("sB", 101, MENU_3)
bot.handle_callback(cb_update(2, "always", "sB", fp, message_id=900))
assert ma.raw_sent == [("sB", "2")], f"Always must send '2': {ma.raw_sent}"

bot, mt, ma, fp = fresh_bot_with_pending("sC", 102, MENU_3)
bot.handle_callback(cb_update(3, "deny", "sC", fp, message_id=900))
assert ma.keys_sent == [("sC", "Escape")], f"Deny must send Escape: {ma.keys_sent}"
assert ma.raw_sent == []
print("callback-inject ok — Allow->send '1', Always->send '2', Deny->keys Escape")


# ── 10. non-owner callback: no injection ────────────────────────────────────────
bot, mt, ma, fp = fresh_bot_with_pending("sD", 103, MENU_3)
bot.handle_callback(cb_update(4, "allow", "sD", fp, from_id=999, message_id=900))
assert ma.raw_sent == [] and ma.keys_sent == [], "non-owner tap must never inject"
assert mt.answers and "authorized" in mt.answers[-1][1].lower(), mt.answers
print("callback-owner ok — non-owner tap answered 'not authorized', no injection")


# ── 11. stale-fp rejection: prompt changed between notify and tap -> no inject ───
bot, mt, ma, fp = fresh_bot_with_pending("sE", 104, MENU_3)
# the pane changes (a different prompt now) -> live fp != tapped fp
ma.peek_text["sE"] = MENU_2
bot.handle_callback(cb_update(5, "allow", "sE", fp, message_id=900))
assert ma.raw_sent == [] and ma.keys_sent == [], "stale tap must NOT inject"
assert mt.answers and "resolved" in mt.answers[-1][1].lower(), mt.answers
assert mt.edits and "elsewhere" in mt.edits[-1][1].lower(), mt.edits
print("stale-fp ok — a changed prompt rejects the tap ('already resolved'), no injection")


# ── 12. stale by status: session no longer waiting -> no inject ─────────────────
bot, mt, ma, fp = fresh_bot_with_pending("sF", 105, MENU_3)
ma.sessions = [sess_row("sF", "active")]   # left waiting
bot.handle_callback(cb_update(6, "allow", "sF", fp, message_id=900))
assert ma.raw_sent == [] and ma.keys_sent == [], "tap on a no-longer-waiting session must not inject"
assert mt.answers and "resolved" in mt.answers[-1][1].lower()
print("stale-status ok — session left waiting rejects the tap, no injection")


# ── 13. idempotency: a second tap after an answer never re-injects ──────────────
bot, mt, ma, fp = fresh_bot_with_pending("sG", 106, MENU_3)
bot.handle_callback(cb_update(7, "allow", "sG", fp, message_id=900))
assert ma.raw_sent == [("sG", "1")]
# simulate Telegram (or the user) re-delivering the SAME tap. The session is still
# waiting with the SAME fp, but this is a double-fire; the answered pending +
# fp-freshness path must make it a no-op. (First: pane still shows the same prompt.)
n_before = len(ma.raw_sent)
bot.handle_callback(cb_update(7, "allow", "sG", fp, message_id=900))
assert len(ma.raw_sent) == n_before, "a repeated tap must never inject twice"
print("idempotency ok — repeated tap after an answer never double-injects")


# ── 14. inject failure (session killed) resolves cleanly, no half-state ─────────
bot, mt, ma, fp = fresh_bot_with_pending("sH", 107, MENU_3)
ma.raw_should_fail.add("sH")
bot.handle_callback(cb_update(8, "allow", "sH", fp, message_id=900))
assert ma.raw_sent == [], "failed inject records nothing"
assert bot.prompts.get("sH") is None, "pending cleared after a failed inject"
assert mt.answers and "running" in mt.answers[-1][1].lower(), mt.answers
print("inject-fail ok — killed session answered 'no longer running', pending cleared, no half-state")


# ── 15. Peek callback: no state change, posts the pane tail to the topic ────────
bot, mt, ma, fp = fresh_bot_with_pending("sP", 108, MENU_3)
bot.handle_callback(cb_update(9, "peek", "sP", fp, message_id=900))
assert ma.raw_sent == [] and ma.keys_sent == [], "Peek must not inject or change state"
assert bot.prompts.get("sP") is not None, "Peek leaves the pending prompt intact"
assert any("peek sP" in t for (_c, t, _tid, _kb) in mt.sent), mt.sent
print("peek ok — Peek posts the pane tail to the topic, no injection, prompt still pending")


# ── 16. offset-advance-on-callback-error + message durable-ack not stalled ──────
# A failing callback (inject raises AND answer/edit raise) must (a) not crash the
# inbound loop, and (b) not stall a following owner message's durable ack — the
# offset advances past BOTH, and the message reaches amux.
class _LoopTelegram(MockTelegram):
    def __init__(self, batches):
        super().__init__()
        self._batches = list(batches)

    def get_updates(self, offset, timeout):
        if self._batches:
            return self._batches.pop(0)
        _bot.stop()   # end the loop once the queue drains
        return []


bot, _mt, ma = make_bot(topics_state={"topics": {"sK": 500}})[:3]
_bot = bot
fp = tg.prompt_fingerprint(MENU_3)
bot.prompts.set("sK", fp, 900, 1.0, "menu", body="🔐 sK")
ma.peek_text["sK"] = MENU_3
ma.sessions = [sess_row("sK", "waiting")]
ma.raw_should_fail.add("sK")     # inject fails
cb = cb_update(10, "allow", "sK", fp, message_id=900)
msg = {"update_id": 11, "message": {"from": {"id": 42}, "text": "carry on",
                                     "message_thread_id": 500}}
loop_tg = _LoopTelegram([[cb, msg]])
loop_tg.fail_answer = True       # answer + edit also fail -> total callback failure
loop_tg.fail_edit = True
bot.tg = loop_tg
bot.inbound_loop()
assert ma.raw_sent == [], "failed inject stays failed"
assert ma.posted == [("sK", "carry on", "telegram", "tg-11")], \
    f"the message AFTER a failing callback must still durably reach amux: {ma.posted}"
assert bot.offset.get() == 12, f"offset must advance past both updates: {bot.offset.get()}"
print("offset ok — a fully-failing callback advances the offset and never stalls the next message's ack")


# ── 17. outbound cleanup: leaving waiting resolves; a killed session resolves ───
bot, mt, ma = make_bot(topics_state={"topics": {"sX": 600}})[:3]
fp = tg.prompt_fingerprint(MENU_3)
bot.prompts.set("sX", fp, 901, 1.0, "menu", body="🔐 sX needs a decision")
# session now reports active -> pending resolved + message annotated
bot._check_permission_prompts([sess_row("sX", "active")])
assert bot.prompts.get("sX") is None, "leaving waiting clears the pending prompt"
assert mt.edits and "continued" in mt.edits[-1][1].lower(), mt.edits

# a killed session (vanished from the list) resolves too
bot2, mt2, ma2 = make_bot(topics_state={"topics": {"sY": 601}})[:3]
bot2.prompts.set("sY", "abcd1234", 902, 1.0, "menu", body="🔐 sY")
bot2._check_permission_prompts([])   # sY not in the list at all
assert bot2.prompts.get("sY") is None, "a vanished session's pending prompt is cleared"
assert mt2.edits and "ended" in mt2.edits[-1][1].lower(), mt2.edits
print("cleanup ok — leaving waiting / disappearing resolves the pending prompt + annotates the message")


# ── 18. supersede: a distinct prompt replaces the old message ───────────────────
bot, mt, ma = make_bot(topics_state={"topics": {"sZ": 700}})[:3]
ma.peek_text["sZ"] = MENU_2
_t = tg.time.time
tg.time.time = lambda: 7000.0
bot.waiting._since["sZ"] = 6980.0
bot._check_permission_prompts([sess_row("sZ", "waiting")])
first_fp = bot.prompts.get("sZ")["fp"]
n1 = len(mt.sent)
# the prompt changes to a distinct one -> supersede old message + notify new
ma.peek_text["sZ"] = MENU_3
tg.time.time = lambda: 7002.0
bot._check_permission_prompts([sess_row("sZ", "waiting")])
assert bot.prompts.get("sZ")["fp"] != first_fp, "a distinct prompt updates the pending fp"
assert len(mt.sent) == n1 + 1, "a distinct prompt fires a new notify"
assert mt.edits and "superseded" in mt.edits[-1][1].lower(), mt.edits
tg.time.time = _t
print("supersede ok — a distinct prompt edits the old message to 'superseded' and notifies anew")


print("\nALL TELEGRAM-PERMS CHECKS PASSED")


def test_telegram_perms():
    """The scenarios above execute at import time and raise on any regression;
    reaching here means the prompt classifier, fingerprint normalization,
    callback-data round-trip + owner gate, grace-window state machine, notify
    gating/dedup/supersede, and callback inject/stale/idempotency/offset
    semantics all hold."""
    assert True
