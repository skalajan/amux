"""Deterministic tests for the Telegram silent-updates + presence layer
(plan .omc/plans/telegram-silent-updates.md, Milestone M1).

Imports amux-telegram.py via importlib (hyphenated filename, same style as
tests/test_telegram_sidecar.py / test_telegram_perms.py) and drives the pure
decision functions (route_reply, should_type, elapsed_bucket, FinalityTracker,
LiveStore, live_trim) plus the Bot orchestration (forward_session's Option-B
routing, the promotion tail, the live box, and the presence header/typing) with
mocked network clients. No real Telegram token, no server. Assertions execute at
import time and raise on any regression; a trailing test_telegram_silent() stub
exists for collection if pytest is ever available.

Covers:
  * route_reply full truth table incl. the (¬final, *, ring_off) -> suppress rows
    (the finality gate precedes ring_off)
  * should_type predicate (telegram-origin active only)
  * elapsed_bucket 30s coarsening + header-string stability within a bucket
  * FinalityTracker settle/reset/re-arm + the autonomous rapid-fire case
  * LiveStore persist/reload round-trip (0600)
  * live_trim + _live_render hard <=3900 trim (Hazard 3) and header-only N-a case
  * Hazard 1: a poll with NO new rows still promotes a settled candidate; the
    rung guard blocks a second ring; a newer row before settle replaces the
    candidate (no ring)
  * Hazard 2: live-box edit wrapper — recreate-once on 'not found', skip-and-retry
    on any other error
  * E2 creation-on-inject: one silent create, no re-create on a second inject, a
    later telegram-origin final rings SEPARATELY with a ✅ header + breadcrumb
  * TG_PRESENCE=0 and TG_PRESENCE_REACT off/on paths
  * autonomous-loop: idle gaps < settle -> no ring; one ring after the final settle
  * out-of-scope: system/alert rows keep the immediate origin-routed forward path
    (never routed through route_reply), and a non-final reply is fully suppressed
"""
import importlib.util
import os
import stat
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SIDECAR = os.path.join(os.path.dirname(HERE), "amux-telegram.py")

_spec = importlib.util.spec_from_file_location("amux_telegram", SIDECAR)
tg = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tg)


# ── 1. route_reply full truth table (the pure Option-B core) ────────────────────
# Finality gate FIRST: a non-final reply suppresses regardless of origin/ring_off.
assert tg.route_reply(False, True, False) == "suppress"
assert tg.route_reply(False, True, True) == "suppress", "ring_off must NOT resurrect a non-final"
assert tg.route_reply(False, False, False) == "suppress"
assert tg.route_reply(False, False, True) == "suppress", "ring_off must NOT resurrect a non-final"
# Finals:
assert tg.route_reply(True, True, False) == "ring", "final telegram-origin, ring on -> RING"
assert tg.route_reply(True, True, True) == "live", "ring off diverts a telegram final to the live box"
assert tg.route_reply(True, False, False) == "live", "origin-muted final -> live box (silent)"
assert tg.route_reply(True, False, True) == "live", "origin-muted + ring off -> live box"
print("route_reply ok — non-final always suppresses (finality gate precedes ring_off); "
      "only telegram+ring-on finals ring; every other final -> live box")


# ── 2. should_type predicate (telegram-origin active turns only) ────────────────
assert tg.should_type("active", True) is True
assert tg.should_type("active", False) is False, "desk-origin active -> no typing"
assert tg.should_type("idle", True) is False
assert tg.should_type("waiting", True) is False
assert tg.should_type("limit", True) is False
print("should_type ok — typing fires ONLY for a telegram-origin active turn")


# ── 3. elapsed_bucket: 30s coarsening ───────────────────────────────────────────
assert tg.elapsed_bucket(0) == "0s"
assert tg.elapsed_bucket(29) == "0s"
assert tg.elapsed_bucket(30) == "30s"
assert tg.elapsed_bucket(45) == "30s", "30 and 45 fall in the same 30s bucket"
assert tg.elapsed_bucket(59) == "30s"
assert tg.elapsed_bucket(60) == "1m"
assert tg.elapsed_bucket(119) == "1m"
assert tg.elapsed_bucket(120) == "2m"
assert tg.elapsed_bucket(-5) == "0s", "negative elapsed clamps to 0"
# stability within a bucket (drives the text_hash no-op-edit skip)
assert tg.elapsed_bucket(30) == tg.elapsed_bucket(45)
assert tg.elapsed_bucket(30) != tg.elapsed_bucket(60)
print("elapsed_bucket ok — coarsened to 30s; stable within a bucket, changes at boundaries")


# ── 4. FinalityTracker: settle / reset / re-arm / rapid-fire ────────────────────
ft = tg.FinalityTracker()
ft.observe("s", "active", 100.0)
assert ft.idle_since("s") is None, "active does not start the settle timer"
ft.observe("s", "idle", 100.0)
assert ft.idle_since("s") == 100.0
ft.observe("s", "idle", 102.0)
assert ft.idle_since("s") == 100.0, "staying idle keeps the original start"
assert not ft.settled("s", 103.0, 4.0), "3s < 4s settle -> not settled"
assert ft.settled("s", 104.0, 4.0), "4s >= 4s settle -> settled"
ft.observe("s", "waiting", 105.0)
assert ft.idle_since("s") is None, "any non-idle label resets the timer"
ft.observe("s", "idle", 106.0)
assert ft.idle_since("s") == 106.0, "re-entering idle re-arms a fresh window"
assert not ft.settled("s", 108.0, 4.0), "fresh window: 2s in, not settled"
# rapid-fire: idle gaps below settle never settle
ft2 = tg.FinalityTracker()
ft2.observe("x", "idle", 200.0)
ft2.observe("x", "active", 201.0)   # gap of 1s < settle -> reset
ft2.observe("x", "idle", 202.0)
assert not ft2.settled("x", 205.0, 4.0), "a sub-settle idle gap must never settle (autonomous loop)"
print("FinalityTracker ok — settles only on continuous idle >= settle; any non-idle resets; re-arms")


# ── 5. LiveStore persist/reload round-trip (0600) ───────────────────────────────
td = tempfile.mkdtemp()
lp = os.path.join(td, "live.json")
ls1 = tg.LiveStore(lp, {})
ls1.set_fields("sess", message_id=555, text_hash="abc", candidate_reply_id="C:2",
               rung_reply_id="C:1", read_ts="14:30", idle_phase="read", body="hello")
ls1.save()
assert stat.S_IMODE(os.stat(lp).st_mode) == 0o600, "live store must be 0600"
ls2 = tg.LiveStore.load(lp)
d = ls2.get("sess")
assert d["message_id"] == 555 and d["text_hash"] == "abc"
assert d["candidate_reply_id"] == "C:2" and d["rung_reply_id"] == "C:1"
assert d["read_ts"] == "14:30" and d["idle_phase"] == "read" and d["body"] == "hello"
assert ls2.get("nope") is None
print("LiveStore ok — fields persist and reload; file is 0600")


# ── 6. live_trim: hard <=3900 cap with a /last hint (Hazard 3) ──────────────────
short = "x" * 100
assert tg.live_trim(short) == short, "short body untouched"
big = "y" * 5000
trimmed = tg.live_trim(big)
assert len(trimmed) <= tg.LIVE_BODY_MAX, f"trimmed body must be <= {tg.LIVE_BODY_MAX}: {len(trimmed)}"
assert trimmed.endswith(tg.LIVE_TRIM_HINT), "a trimmed body must carry the /last hint"
print("live_trim ok — long body hard-capped to <=3900 with a /last hint; short body untouched")


# ── mock network clients ────────────────────────────────────────────────────────
class MockTelegram:
    def __init__(self):
        self.sent = []        # (chat_id, text, topic_id, disable_notification)
        self.edits = []       # (message_id, text)
        self.actions = []     # (topic_id, action)
        self.reactions = []   # (message_id, emoji)
        self._mid = 5000
        self.edit_error = None       # TelegramError to raise on the NEXT edit, then clear
        self.react_should_fail = False

    def get_me(self):
        return {"id": 1, "username": "amux_test_bot"}

    def send_message(self, chat_id, text, topic_id=None, disable_notification=False,
                     reply_markup=None):
        self._mid += 1
        self.sent.append((chat_id, text, topic_id, bool(disable_notification)))
        return {"message_id": self._mid}

    def edit_message_text(self, chat_id, message_id, text, reply_markup=None):
        if self.edit_error is not None:
            err = self.edit_error
            self.edit_error = None
            raise err
        self.edits.append((message_id, text))
        return {"message_id": message_id}

    def send_chat_action(self, chat_id, action, topic_id=None):
        self.actions.append((topic_id, action))
        return {"ok": True}

    def set_message_reaction(self, chat_id, message_id, emoji):
        if self.react_should_fail:
            raise tg.TelegramError("reaction on a too-old message")
        self.reactions.append((message_id, emoji))
        return {"ok": True}

    def create_forum_topic(self, chat_id, name):
        self._mid += 1
        return self._mid


class MockAmux:
    def __init__(self):
        self.threads = {}     # session -> {"thread": [...], "cursor": int}
        self.sessions = []
        self.posted = []
        self._seen_ids = set()

    def list_sessions(self):
        return self.sessions

    def get_chat(self, session, since=0):
        data = self.threads.get(session, {"thread": [], "cursor": 0})
        thread = data["thread"]
        if since:
            # mirror the server's since filter on rowid_seq (owner/system seq=None kept)
            thread = [it for it in thread
                      if it.get("seq") is None or (it.get("seq") or 0) > since]
        return {"session": session, "thread": thread, "cursor": data.get("cursor", 0)}

    def post_chat(self, session, text, origin="telegram", msg_id=""):
        deduped = msg_id in self._seen_ids
        if not deduped:
            self._seen_ids.add(msg_id)
            self.posted.append((session, text, origin, msg_id))
        return {"ok": True, "deduped": deduped}

    def peek(self, session, lines=40):
        return ""


def make_bot(topics=None, outbound_state=None, chat_id="-100999", summarizer=None, **cfg_over):
    d = tempfile.mkdtemp()
    tstore = tg.TopicStore(os.path.join(d, "topics.json"), {"topics": topics or {}})
    outbound = tg.OutboundTracker(os.path.join(d, "out.json"), outbound_state or {})
    off = tg.OffsetStore(os.path.join(d, "offset"), 0)
    live = tg.LiveStore(os.path.join(d, "live.json"), {})
    cfg = {"owner_id": 42, "chat_id": chat_id, "amux_base": "x", "tg_api_base": "y",
           "write_token": "wt", "poll_secs": 0.01, "long_poll_secs": 1,
           "machine_label": "testbox"}
    cfg.update(cfg_over)
    mt, ma = MockTelegram(), MockAmux()
    bot = tg.Bot(cfg, mt, ma, tstore, off, outbound, summarizer=summarizer, live=live)
    return bot, mt, ma


def owner_item(iid, origin, ts):
    return {"id": iid, "role": "owner", "origin": origin, "text": "in", "ts": ts, "seq": None}


def reply_item(iid, text, ts, seq):
    return {"id": iid, "role": "session", "text": text, "ts": ts, "seq": seq}


def owner_msg(update_id, text, topic_id, from_id=42, message_id=7):
    return {"update_id": update_id, "message": {
        "from": {"id": from_id}, "text": text, "message_thread_id": topic_id,
        "message_id": message_id}}


def seeded(session):
    return {session: {"last_seq": 0, "seen": [], "last_owner_origin": None}}


def _patch_time(value):
    saved = tg.time.time
    tg.time.time = lambda: value
    return saved


# ── 7. _live_render: header-only (N-a) + hard trim (Hazard 3) ───────────────────
# N-a: a box created on inject (item None) with a read receipt renders the header
# ALONE — never an empty edit.
bot, mt, ma = make_bot(topics={"s": 100})
bot.live.set_fields("s", read_ts="14:30", idle_phase="read")
hdr = bot._live_render("s", None, "idle", 1000.0)
assert hdr == "👀 přečteno 14:30", f"header-only render (N-a): {hdr!r}"
assert hdr, "header-only render must be non-empty (never an empty edit)"

# Hazard 3: a >3900-char full-mode reply -> body hard-trimmed; total still bounded.
bot2, mt2, ma2 = make_bot(topics={"s": 100})
bot2.topics.set_mode("s", "full")
big_item = reply_item("B:1", "z" * 6000, 10, 1)
# presence OFF isolates the body so we can assert the <=3900 body cap exactly.
bot2.cfg["presence"] = False
body_only = bot2._live_render("s", big_item, "idle", 1000.0)
assert len(body_only) <= tg.LIVE_BODY_MAX, f"live body must be <=3900: {len(body_only)}"
assert body_only.endswith(tg.LIVE_TRIM_HINT), "trimmed body carries the /last hint"
# presence ON: header + trimmed body, header first.
bot2.cfg["presence"] = True
full = bot2._live_render("s", big_item, "active", 1000.0)
assert full.startswith("▶ pracuje"), "presence header leads the render"
assert tg.LIVE_TRIM_HINT in full, "the trimmed body (with hint) sits under the header"
print("_live_render ok — header-only N-a case renders header alone; body hard-trimmed <=3900 (Hazard 3)")


# ── 8. header-string stability within a 30s bucket (no edit churn) ──────────────
bot, mt, ma = make_bot(topics={"s": 100})
bot.outbound.observe_owner("s", "telegram")
bot.live.set_fields("s", message_id=9001, text_hash="")   # a live box already exists
t0 = 1000.0
saved = _patch_time(t0)
bot._presence_tail("s", "active", t0)               # active_since = 1000, header "(0s)"
assert len(mt.edits) == 1, f"first active poll edits the header once: {mt.edits}"
bot._presence_tail("s", "active", t0 + 20)          # elapsed 20 -> still 0s bucket -> no edit
assert len(mt.edits) == 1, "a within-bucket poll must NOT re-edit (text_hash skip)"
bot._presence_tail("s", "active", t0 + 35)          # elapsed 35 -> 30s bucket -> a new edit
assert len(mt.edits) == 2, f"a bucket-boundary poll edits again: {mt.edits}"
assert "▶ pracuje (30s)" in mt.edits[-1][1], mt.edits[-1]
# telegram-origin active also drove the typing indicator (>=1, on the ~4s cadence)
assert mt.actions and mt.actions[0][1] == "typing", "telegram-origin active turn re-sends typing"
tg.time.time = saved
print("header-hash ok — elapsed coarsened to 30s: no edit within a bucket, one edit at the boundary")


# ── 9. Hazard 2: live-box edit wrapper — recreate-once / skip-and-retry ─────────
# recreate-once on 'message to edit not found'
bot, mt, ma = make_bot(topics={"s": 100})
bot.live.set_fields("s", message_id=9100, text_hash="old")
mt.edit_error = tg.TelegramError("editMessageText: Bad Request: message to edit not found")
ok = bot._live_edit("s", "new content")
assert ok is True, "recreate path returns True"
assert len(mt.sent) == 1, "a not-found edit recreates the box with exactly one new send"
new_mid = bot.live.get("s")["message_id"]
assert new_mid != 9100 and new_mid is not None, f"the box id must be replaced: {new_mid}"
# skip-and-retry on any other error (e.g. 429) — no recreate, id unchanged
bot2, mt2, ma2 = make_bot(topics={"s": 100})
bot2.live.set_fields("s", message_id=9200, text_hash="old")
mt2.edit_error = tg.TelegramError("editMessageText: Too Many Requests: retry after 5")
ok2 = bot2._live_edit("s", "new content")
assert ok2 is False, "a non-recreatable error returns False (retry next poll)"
assert mt2.sent == [], "a 429 must NOT recreate the box"
assert bot2.live.get("s")["message_id"] == 9200, "the box id is preserved on a skip"
# a no-op edit (unchanged hash) is skipped entirely
bot3, mt3, ma3 = make_bot(topics={"s": 100})
bot3.live.set_fields("s", message_id=9300, text_hash=tg._text_hash("same"))
assert bot3._live_edit("s", "same") is True
assert mt3.edits == [] and mt3.sent == [], "an unchanged render must not edit (dodges 400 not-modified)"
print("Hazard 2 ok — 'not found' recreates once; other errors skip+retry; unchanged text skips the edit")


# ── 10. Hazard 1: a poll with NO new rows still promotes a settled candidate ────
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"))
bot.outbound.observe_owner("s", "telegram")
# poll 1 (active): a reply row appears -> recorded as candidate, NO send/ring
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("R:1", "the answer", 20, 1)]}
saved = _patch_time(1000.0)
bot.forward_session("s", status_label="active")
assert mt.sent == [], "a non-final (active) reply must not send/ring"
assert bot.live.get("s")["candidate_reply_id"] == "R:1", "the reply is recorded as the candidate"
# poll 2 (idle, NO new rows) — not yet settled
tg.time.time = lambda: 1001.0
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10)]}
bot.forward_session("s", status_label="idle")
assert mt.sent == [], "before the settle window elapses, still no ring"
# poll 3 (idle, NO new rows) — settle window elapsed -> promote on a ROW-LESS poll
tg.time.time = lambda: 1006.0
bot.forward_session("s", status_label="idle")
rings = [s for s in mt.sent if s[3] is False]
assert len(rings) == 1, f"a settled candidate rings on a no-new-row poll: {mt.sent}"
assert rings[0][1] == "the answer", "the ringing message carries the reply content"
assert bot.live.get("s")["rung_reply_id"] == "R:1", "the promoted reply is marked rung"
# poll 4: the rung guard blocks a second ring
tg.time.time = lambda: 1008.0
n = len(mt.sent)
bot.forward_session("s", status_label="idle")
assert len([s for s in mt.sent if s[3] is False]) == 1, "the rung guard blocks a repeat ring"
tg.time.time = saved
print("Hazard 1 ok — the promotion tail fires on a row-less poll after settle; rung_reply_id blocks a repeat")


# ── 11. a newer row before settle replaces the candidate (no premature ring) ────
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"))
bot.outbound.observe_owner("s", "telegram")
saved = _patch_time(1000.0)
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("R:1", "first", 20, 1)]}
bot.forward_session("s", status_label="active")          # candidate R:1, not settled
tg.time.time = lambda: 1001.0
ma.threads["s"] = {"cursor": 2, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("R:2", "second", 30, 2)]}
bot.forward_session("s", status_label="active")          # candidate replaced by R:2, still active
assert bot.live.get("s")["candidate_reply_id"] == "R:2"
assert [s for s in mt.sent if s[3] is False] == [], "no ring while active (candidate kept moving)"
tg.time.time = lambda: 1002.0
ma.threads["s"] = {"cursor": 2, "thread": [owner_item("o1", "telegram", 10)]}
bot.forward_session("s", status_label="idle")            # idle timer starts here
tg.time.time = lambda: 1007.0
bot.forward_session("s", status_label="idle")            # settle -> ring the LATEST candidate
rings = [s for s in mt.sent if s[3] is False]
assert len(rings) == 1 and rings[0][1] == "second", f"only the latest settled candidate rings: {rings}"
tg.time.time = saved
print("candidate-replace ok — a newer row before settle supersedes the candidate; only the latest rings")


# ── 12. origin-muted final -> silent live-box create/edit, ZERO ring ───────────
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"), presence=False)
bot.outbound.observe_owner("s", "dashboard")   # governing origin != telegram
saved = _patch_time(1000.0)
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "dashboard", 10),
                                           reply_item("R:1", "desk answer", 20, 1)]}
bot.forward_session("s", status_label="active")
tg.time.time = lambda: 1001.0
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "dashboard", 10)]}
bot.forward_session("s", status_label="idle")            # idle timer starts
tg.time.time = lambda: 1006.0
bot.forward_session("s", status_label="idle")            # settle -> silent live edit
assert [s for s in mt.sent if s[3] is False] == [], "an origin-muted final must NEVER ring"
creates = [s for s in mt.sent if s[3] is True]
assert len(creates) == 1, f"exactly one silent live-box creation: {mt.sent}"
assert creates[0][1] == "desk answer", "the live box holds the origin-muted final's content"
assert bot.live.get("s")["rung_reply_id"] == "R:1"
tg.time.time = saved
print("origin-muted ok — a desk-origin final lands as one silent live-box send, zero ring")


# ── 13. E2: creation-on-inject (silent, once) + later separate ringing final ────
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"))   # presence on (default)
saved = _patch_time(1000.0)
# first inject -> one silent live-box creation with a 👀 read receipt, no ring
bot.handle_update(owner_msg(1, "do it", topic_id=100, message_id=11))
creates = [s for s in mt.sent if s[3] is True]
assert len(creates) == 1, f"one silent creation on the first inject: {mt.sent}"
assert "👀 přečteno" in creates[0][1], "the created box shows the read receipt"
assert [s for s in mt.sent if s[3] is False] == [], "creation must not ring"
assert mt.reactions == [], "TG_PRESENCE_REACT defaults off -> no setMessageReaction"
mid = bot.live.get("s")["message_id"]
# second inject to the SAME session -> NO new creation (persisted message_id guard)
bot.handle_update(owner_msg(2, "and this", topic_id=100, message_id=12))
assert bot.live.get("s")["message_id"] == mid, "a second inject must not re-create the box"
assert len([s for s in mt.sent if s[3] is True]) == 1, "still exactly one creation (no double-badge)"
# the session runs and settles with a telegram-origin final -> rings SEPARATELY
tg.time.time = lambda: 1001.0
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("R:1", "phone answer", 20, 1)]}
bot.forward_session("s", status_label="active")
tg.time.time = lambda: 1002.0
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10)]}
bot.forward_session("s", status_label="idle")            # idle timer starts
tg.time.time = lambda: 1007.0
bot.forward_session("s", status_label="idle")            # settle -> ring
rings = [s for s in mt.sent if s[3] is False]
assert len(rings) == 1 and rings[0][1] == "phone answer", f"the final rings as a fresh message: {rings}"
# the live box (a distinct message) flips to a ✅ hotovo breadcrumb
breadcrumbs = [e for e in mt.edits if e[0] == mid and "viz odpověď" in e[1]]
assert breadcrumbs, f"the live box gets a settled breadcrumb: {mt.edits}"
assert "✅ hotovo" in breadcrumbs[-1][1], "the breadcrumb header shows done"
tg.time.time = saved
print("E2 ok — one silent create per session, no re-create on re-inject; a later telegram final rings "
      "separately and the box flips to ✅ + breadcrumb")


# ── 14. TG_PRESENCE=0 — no header, no typing, no on-inject box ──────────────────
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"), presence=False)
bot.handle_update(owner_msg(1, "do it", topic_id=100))
assert mt.sent == [], "presence off: no live box is created on inject"
assert bot.live.get("s") is None or not bot.live.get("s").get("message_id")
bot.outbound.observe_owner("s", "telegram")
saved = _patch_time(1000.0)
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("R:1", "x", 20, 1)]}
bot.forward_session("s", status_label="active")
assert mt.actions == [], "presence off: no typing indicator"
assert mt.edits == [], "presence off: no status-header edits"
tg.time.time = saved
print("TG_PRESENCE=0 ok — no on-inject box, no typing, no header edits (r3 Option-B content only)")


# ── 15. TG_PRESENCE_REACT — off (default) vs on, error swallowed ────────────────
# default off already asserted in E2. On: one reaction on inject success.
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"), presence_react=True)
bot.handle_update(owner_msg(1, "do it", topic_id=100, message_id=77))
assert mt.reactions == [(77, "👀")], f"react on: one 👀 reaction on Jan's message: {mt.reactions}"
# a failing reaction (too-old message) is swallowed, inject still succeeds
bot2, mt2, ma2 = make_bot(topics={"s": 100}, outbound_state=seeded("s"), presence_react=True)
mt2.react_should_fail = True
bot2.handle_update(owner_msg(2, "do it", topic_id=100, message_id=88))
assert ma2.posted and ma2.posted[-1][3] == "tg-2", "a failing reaction must not break the inject"
print("TG_PRESENCE_REACT ok — off: no reaction; on: one 👀; a reaction error is swallowed (cosmetic)")


# ── 16. autonomous loop: idle gaps < settle -> no ring; one ring after settle ───
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"), presence=False)
bot.outbound.observe_owner("s", "telegram")
saved = _patch_time(1000.0)
# a sequence of turns with sub-settle idle gaps
steps = [
    (1000.0, "active", [reply_item("R:1", "turn1", 20, 1)]),
    (1001.0, "idle",   []),
    (1002.0, "active", [reply_item("R:2", "turn2", 30, 2)]),
    (1003.0, "idle",   []),
    (1004.0, "active", [reply_item("R:3", "turn3", 40, 3)]),
    (1005.0, "idle",   []),
]
for now, label, replies in steps:
    tg.time.time = lambda v=now: v
    ma.threads["s"] = {"cursor": 3, "thread": [owner_item("o1", "telegram", 10)] + replies}
    bot.forward_session("s", status_label=label)
    assert [s for s in mt.sent if s[3] is False] == [], f"no ring mid-loop (t={now})"
# now the loop truly ends: idle continuously past settle
tg.time.time = lambda: 1010.0
ma.threads["s"] = {"cursor": 3, "thread": [owner_item("o1", "telegram", 10)]}
bot.forward_session("s", status_label="idle")
rings = [s for s in mt.sent if s[3] is False]
assert len(rings) == 1 and rings[0][1] == "turn3", f"one ring after settle, latest turn only: {rings}"
tg.time.time = saved
print("autonomous-loop ok — sub-settle idle gaps never ring; exactly one ring (latest turn) after settle")


# ── 17. out-of-scope: system/alert rows keep the immediate origin-routed path ───
# Even WITH a status label (the new path), a system row is forwarded immediately
# and rings per its governing origin — it is NEVER routed through route_reply /
# suppressed / deferred (plan: this reworks role=='session' only).
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"))
saved = _patch_time(1000.0)
ma.threads["s"] = {"cursor": 0, "thread": [
    owner_item("o1", "telegram", 10),
    {"id": "sys-1", "role": "system", "text": "usage limit", "ts": 20, "seq": None},
]}
bot.forward_session("s", status_label="idle")
sysrows = [s for s in mt.sent if "usage limit" in s[1]]
assert len(sysrows) == 1, f"a system row is forwarded immediately, even in the new path: {mt.sent}"
assert sysrows[0][3] is False, "a system row after a telegram-origin owner rings (unchanged path)"
tg.time.time = saved
print("out-of-scope ok — system/alert rows keep the immediate origin-routed forward (not route_reply'd)")


# ── 18. a non-final reply is fully suppressed (no send AND no edit) ─────────────
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"), presence=False)
bot.outbound.observe_owner("s", "telegram")
saved = _patch_time(1000.0)
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("R:1", "mid-run", 20, 1)]}
bot.forward_session("s", status_label="active")   # active -> non-final
assert mt.sent == [] and mt.edits == [], "a non-final reply produces no send and no edit (full suppress)"
tg.time.time = saved
print("suppress ok — an active (non-final) reply is fully suppressed: no send, no edit")


print("\nALL TELEGRAM-SILENT CHECKS PASSED")


def test_telegram_silent():
    """The scenarios above execute at import time and raise on any regression;
    reaching here means route_reply's truth table, should_type, elapsed_bucket,
    FinalityTracker, LiveStore, live_trim/_live_render trimming, the promotion
    tail (Hazard 1), the live-box edit wrapper (Hazard 2), creation-on-inject
    (E2), the presence toggles, the autonomous-loop guard, and the out-of-scope
    system-row path all hold."""
    assert True
