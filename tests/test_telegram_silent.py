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
  * elapsed_bucket growing thresholds + header-string stability within a bucket
  * FinalityTracker settle/reset/re-arm + the autonomous rapid-fire case
  * LiveStore persist/reload round-trip (0600)
  * live_trim + _live_render hard LIVE_BODY_MAX trim (Hazard 3) and header-only N-a case
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


# ── 3. elapsed_bucket: GROWING thresholds, silent under 5 minutes ──────────────
# Changed 2026-08-18 (plan chat-improvement.md C4). This block used to pin 30s
# coarsening — but (secs//30)*30//60 collapses to whole minutes past 60s, so the
# header actually changed once a MINUTE forever: 11 in-place rewrites of one
# Telegram message during a 10-minute hold, which is Jan's "awkward existing
# message changes". The assertions are rewritten rather than deleted, because the
# rewrite is the record of the decision: nothing under 5 minutes, then thresholds
# that GROW, so a typical turn produces zero mid-turn edits.
assert tg.elapsed_bucket(0) == "", "a fresh turn shows no timer at all"
assert tg.elapsed_bucket(299) == "", "under 5 minutes stays silent — the whole point"
assert tg.elapsed_bucket(300) == "5m+"
assert tg.elapsed_bucket(899) == "5m+", "the bucket is stable across its whole span"
assert tg.elapsed_bucket(900) == "15m+"
assert tg.elapsed_bucket(1800) == "30m+"
assert tg.elapsed_bucket(3600) == "1h+"
assert tg.elapsed_bucket(86400) == "1h+", "it stops growing — no unbounded churn"
assert tg.elapsed_bucket(-5) == "", "negative elapsed clamps to 0"
# The property that actually matters: how many distinct headers a hold produces.
def _churn(span):
    seen, out = None, 0
    for t in range(0, span + 1, 2):          # poll_secs=2.0
        v = tg.elapsed_bucket(t)
        if seen is not None and v != seen:
            out += 1
        seen = v
    return out
assert _churn(299) == 0, "a sub-5-minute turn must rewrite the live box ZERO times"
assert _churn(600) == 1, f"a 10-minute hold: 1 edit (was 11): {_churn(600)}"
assert _churn(3600) == 4, f"an hour: 4 edits total: {_churn(3600)}"
assert _churn(86400) == 4, "a day-long session still only ever edits 4 times"
# stability within a bucket (drives the text_hash no-op-edit skip). Retargeted to
# the new boundaries: 30s and 60s are now BOTH silent, so the old pair no longer
# discriminates anything — a test that cannot tell its two inputs apart is not
# testing stability, it is just passing.
assert tg.elapsed_bucket(400) == tg.elapsed_bucket(800), "stable inside the 5m+ bucket"
assert tg.elapsed_bucket(800) != tg.elapsed_bucket(1000), "changes at the 15m boundary"
assert tg.elapsed_bucket(30) == tg.elapsed_bucket(60) == "", "both silent now"
print("elapsed_bucket ok — silent under 5m; growing thresholds; 10-min hold = 1 edit (was 11), 1h = 4")


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


# ── 6. live_trim: hard LIVE_BODY_MAX cap with a /last hint (Hazard 3) ──────────
# The cap is TG_LIVE_BODY_MAX (default 1200, was a hardcoded 3900). Assertions
# read tg.LIVE_BODY_MAX rather than a literal, so lowering the default cannot
# leave a test asserting a bound three times looser than the shipped one.
short = "x" * 100
assert tg.live_trim(short) == short, "short body untouched"
big = "y" * 5000
trimmed = tg.live_trim(big)
assert len(trimmed) <= tg.LIVE_BODY_MAX, f"trimmed body must be <= {tg.LIVE_BODY_MAX}: {len(trimmed)}"
assert trimmed.endswith(tg.LIVE_TRIM_HINT), "a trimmed body must carry the /last hint"
print(f"live_trim ok — long body hard-capped to <={tg.LIVE_BODY_MAX} with a /last hint; short body untouched")


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
        # reply_markup is recorded because Telegram DROPS an inline keyboard on
        # any edit that omits it. Without capturing it here, no test could tell a
        # keyboard-preserving edit from a keyboard-destroying one — the instrument
        # could not express the failure.
        self.edits.append((message_id, text, reply_markup))
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

# Hazard 3: an over-cap full-mode reply -> body hard-trimmed; total still bounded.
bot2, mt2, ma2 = make_bot(topics={"s": 100})
bot2.topics.set_mode("s", "full")
big_item = reply_item("B:1", "z" * 6000, 10, 1)
# presence OFF isolates the body so we can assert the body cap exactly.
bot2.cfg["presence"] = False
body_only = bot2._live_render("s", big_item, "idle", 1000.0)
assert len(body_only) <= tg.LIVE_BODY_MAX, f"live body must be <={tg.LIVE_BODY_MAX}: {len(body_only)}"
assert body_only.endswith(tg.LIVE_TRIM_HINT), "trimmed body carries the /last hint"
# presence ON: header + trimmed body, header first.
bot2.cfg["presence"] = True
full = bot2._live_render("s", big_item, "active", 1000.0)
assert full.startswith("▶ pracuje"), "presence header leads the render"
assert tg.LIVE_TRIM_HINT in full, "the trimmed body (with hint) sits under the header"
print(f"_live_render ok — header-only N-a case renders header alone; body hard-trimmed <={tg.LIVE_BODY_MAX} (Hazard 3)")


# ── 8. header stability across a whole turn (the anti-churn property) ──────────
# Retargeted 2026-08-18 (plan C4). This used to walk 0s -> 30s and assert an edit
# at the 30s boundary. That boundary no longer exists: the header is silent below
# five minutes, precisely so an ordinary turn rewrites the live box ZERO times.
# The case now asserts the property Jan actually complained about — a message he
# may be reading does not change while nothing is happening.
bot, mt, ma = make_bot(topics={"s": 100})
bot.outbound.observe_owner("s", "telegram")
bot.live.set_fields("s", message_id=9001, text_hash="")   # a live box already exists
t0 = 1000.0
saved = _patch_time(t0)
bot._presence_tail("s", "active", t0)               # active_since = 1000, header "▶ pracuje"
assert len(mt.edits) == 1, f"first active poll edits the header once: {mt.edits}"
assert mt.edits[-1][1] == "▶ pracuje", f"no timer on a fresh turn: {mt.edits[-1]}"
for dt in (20, 35, 60, 120, 240, 299):              # a full sub-5-minute turn
    bot._presence_tail("s", "active", t0 + dt)
assert len(mt.edits) == 1, \
    f"a sub-5-minute turn must NOT re-edit the live box even once: {mt.edits}"
bot._presence_tail("s", "active", t0 + 300)         # crosses 5m -> one meaningful edit
assert len(mt.edits) == 2, f"crossing 5m edits once: {mt.edits}"
assert "▶ pracuje (5m+)" in mt.edits[-1][1], mt.edits[-1]
bot._presence_tail("s", "active", t0 + 899)         # still inside 5m+ -> no edit
assert len(mt.edits) == 2, "a within-bucket poll must NOT re-edit (text_hash skip)"
# telegram-origin active also drove the typing indicator (>=1, on the ~4s cadence)
assert mt.actions and mt.actions[0][1] == "typing", "telegram-origin active turn re-sends typing"
tg.time.time = saved
print("header-churn ok — a sub-5m turn edits the box ZERO times after creation; 5m+ edits once")


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
# quiet_default=0 isolates the Option-B promotion mechanics (settle/candidate/rung)
# under legacy route_reply routing; quiet-mode latch routing is covered separately
# in the "quiet mode" section below.
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"), quiet_default=0)
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
# quiet_default=0: legacy routing so the mechanics assertion (only the latest
# settled candidate rings) is isolated from quiet-mode latch gating.
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"), quiet_default=0)
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
# quiet_default=0: legacy routing isolates the finality-settle mechanics (sub-settle
# idle gaps never settle; one ring on the final continuous-idle). Under quiet mode an
# autonomous (non-latched) final goes SILENT instead — see the "quiet mode" section.
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"), presence=False,
                       quiet_default=0)
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


# ── 17. system/alert rows keep the immediate path, but land SILENTLY ───────────
# Even WITH a status label, a system row is forwarded immediately and is NEVER
# routed through route_reply / suppressed / deferred (that rework is role=='session'
# only). What CHANGED 2026-08-18 (plan chat-improvement.md C2c): it no longer RINGS.
#
# A system row is the usage-limit episode, and amux answers that menu itself
# (ethos D2) — so the phone buzzed for something that resolved without Jan. It now
# routes through notify_class(kind="failure", terminal=False) to the live box,
# which already renders "⛔ limit". The old assertion here was
# `sysrows[0][3] is False` ("rings"); it is inverted below rather than deleted,
# because the inversion IS the record of the decision. A TERMINAL failure still
# rings — see the notify_class grid in Q1.
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"))
saved = _patch_time(1000.0)
ma.threads["s"] = {"cursor": 0, "thread": [
    owner_item("o1", "telegram", 10),
    {"id": "sys-1", "role": "system", "text": "usage limit", "ts": 20, "seq": None},
]}
bot.forward_session("s", status_label="idle")
sysrows = [s for s in mt.sent if "usage limit" in s[1]]
assert len(sysrows) == 1, f"a system row is forwarded immediately, even in the new path: {mt.sent}"
assert sysrows[0][3] is True, \
    "C2c: a self-resolving usage-limit system row lands SILENTLY in the live box, never rings"
tg.time.time = saved
print("system-row ok — forwarded immediately (not route_reply'd) and SILENT (C2c: non-terminal failure -> live box)")


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


# ══ QUIET MODE (plan .omc/plans/telegram-quiet-mode.md, design A: latch-only) ═══
# Layers on top of Option B above. Default policy: while quiet, only questions,
# failures, and the latch-armed answer to a Telegram turn ring; every other final
# lands silently in the live box.

# ── Q1. notify_class grid — the single classifier ──────────────────────────────
NC = tg.notify_class
# Questions + failures ALWAYS ring — they precede the ring_off/finality checks, so
# /ring off never silences them (only /mute does, upstream). is_final is irrelevant.
for _ro in (True, False):
    for _lat in (True, False):
        for _win in (True, False):
            for _otg in (True, False):
                for _fin in (True, False):
                    assert NC("question", _fin, _ro, _lat, _win, _otg) == "ring"
                    assert NC("failure", _fin, _ro, _lat, _win, _otg) == "ring"
# Reply: finality gate precedes everything (a non-final always suppresses).
assert NC("reply", False, False, True, True, True) == "suppress"
assert NC("reply", False, True, False, False, False) == "suppress"
# /ring off diverts a reply to the live box and WINS over the latch.
assert NC("reply", True, True, True, False, True) == "live", "ring_off wins over the latch"
# The latch core: the answer rings after ANY delay, window closed, origin irrelevant.
assert NC("reply", True, False, True, False, False) == "ring", "latch rings even a desk-origin final"
assert NC("reply", True, False, True, False, True) == "ring"
# Latch consumed (armed False) + window closed -> autonomous final is SILENT.
assert NC("reply", True, False, False, False, True) == "live", "post-latch autonomous final is silent"
# Design-B window layer (only reachable via the shim/quiet-off): window ∧ telegram.
assert NC("reply", True, False, False, True, True) == "ring", "window+telegram-origin rings (shim path)"
assert NC("reply", True, False, False, True, False) == "live", "window but desk-origin -> silent"
print("notify_class ok — questions/failures always ring; reply gated by finality>ring_off>latch>window")


# ── Q2. route_reply shim == notify_class(reply, latch off, window open) ─────────
for _fin in (True, False):
    for _otg in (True, False):
        for _ro in (True, False):
            assert tg.route_reply(_fin, _otg, _ro) == NC(
                "reply", _fin, _ro, False, True, _otg), (_fin, _otg, _ro)
print("route_reply-shim ok — the legacy shim exactly equals notify_class with latch off, window open")


# ── Q3. latch lifecycle: arm -> slow answer rings + clears -> autonomous silent
#        -> re-arm -> rings again (quiet ON, the default) ────────────────────────
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"))   # quiet default ON
saved = _patch_time(1000.0)
bot.handle_update(owner_msg(1, "do it", topic_id=100, message_id=11))   # Telegram inbound -> arms
assert bot.live.get("s")["awaiting_tg_reply"] is True, "a Telegram inbound arms the latch"
# The agent works a LONG time (a wall-clock window would have expired), then settles.
tg.time.time = lambda: 1100.0
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("R:1", "the slow answer", 1050, 1)]}
bot.forward_session("s", status_label="active")
tg.time.time = lambda: 1101.0
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10)]}
bot.forward_session("s", status_label="idle")
tg.time.time = lambda: 1106.0
bot.forward_session("s", status_label="idle")            # settle -> latched ring
rings = [x for x in mt.sent if x[3] is False]
assert len(rings) == 1 and rings[0][1] == "the slow answer", f"the slow latched answer rings: {mt.sent}"
assert bot.live.get("s")["awaiting_tg_reply"] is False, "an effective latched ring clears the latch"
assert bot.live.get("s")["latch_arm_key"] is None
# A second AUTONOMOUS final (no new inbound) -> silent live box, no ring.
tg.time.time = lambda: 1110.0
ma.threads["s"] = {"cursor": 2, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("R:2", "autonomous chatter", 1108, 2)]}
bot.forward_session("s", status_label="active")
tg.time.time = lambda: 1111.0
ma.threads["s"] = {"cursor": 2, "thread": [owner_item("o1", "telegram", 10)]}
bot.forward_session("s", status_label="idle")
tg.time.time = lambda: 1116.0
bot.forward_session("s", status_label="idle")
assert len([x for x in mt.sent if x[3] is False]) == 1, "an autonomous final after the latch cleared is silent"
# Jan speaks again -> re-arms -> the next final rings again.
bot.handle_update(owner_msg(2, "now this", topic_id=100, message_id=12))
assert bot.live.get("s")["awaiting_tg_reply"] is True, "a new inbound re-arms the latch"
tg.time.time = lambda: 1120.0
ma.threads["s"] = {"cursor": 3, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("R:3", "second answer", 1118, 3)]}
bot.forward_session("s", status_label="active")
tg.time.time = lambda: 1121.0
ma.threads["s"] = {"cursor": 3, "thread": [owner_item("o1", "telegram", 10)]}
bot.forward_session("s", status_label="idle")
tg.time.time = lambda: 1126.0
bot.forward_session("s", status_label="idle")
rings = [x for x in mt.sent if x[3] is False]
assert len(rings) == 2 and rings[-1][1] == "second answer", f"the re-armed latch rings the next answer: {rings}"
tg.time.time = saved
print("latch-lifecycle ok — arm rings the slow answer & clears; autonomous finals stay silent; re-arm rings again")


# ── Q4. busy-session post-dating guard: an in-flight autonomous final recorded
#        at-or-before the arm boundary does NOT consume the latch; the later
#        post-inbound answer does (r3 race) ─────────────────────────────────────
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"), presence=False)
bot.outbound.observe_owner("s", "telegram")
saved = _patch_time(1000.0)
# (1) a busy autonomous session emits F0 -> recorded, advances latest_key.
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("F0", "in-flight autonomous", 20, 1)]}
bot.forward_session("s", status_label="active")
assert bot.live.get("s")["latest_key"] == [20, 1], "F0 advances the latest known key"
# (2) Jan's inbound arms AFTER F0 was recorded -> boundary = F0's key.
bot.handle_update(owner_msg(1, "answer me", topic_id=100, message_id=11))
assert bot.live.get("s")["latch_arm_key"] == [20, 1], "the arm boundary is the latest known key (F0's)"
assert bot.live.get("s")["awaiting_tg_reply"] is True
# (3) F0 settles + promotes -> it PREDATES the boundary -> must NOT ring, must NOT clear.
tg.time.time = lambda: 1001.0
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10)]}
bot.forward_session("s", status_label="idle")
tg.time.time = lambda: 1006.0
bot.forward_session("s", status_label="idle")
assert [x for x in mt.sent if x[3] is False] == [], "the predating in-flight final must NOT ring"
assert bot.live.get("s")["awaiting_tg_reply"] is True, "a predating final leaves the latch armed"
# (4) the real answer A (post-dates the boundary) rings once and clears.
tg.time.time = lambda: 1010.0
ma.threads["s"] = {"cursor": 2, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("A", "the real answer", 40, 2)]}
bot.forward_session("s", status_label="active")
tg.time.time = lambda: 1011.0
ma.threads["s"] = {"cursor": 2, "thread": [owner_item("o1", "telegram", 10)]}
bot.forward_session("s", status_label="idle")
tg.time.time = lambda: 1016.0
bot.forward_session("s", status_label="idle")
rings = [x for x in mt.sent if x[3] is False]
assert len(rings) == 1 and rings[0][1] == "the real answer", f"only the post-inbound answer rings: {rings}"
assert bot.live.get("s")["awaiting_tg_reply"] is False, "the effective post-boundary answer clears the latch"
tg.time.time = saved
print("post-dating ok — an in-flight final at-or-before the boundary can't consume the latch; the later answer does")


# ── Q5. a question rings while quiet with the latch CLEAR (Phase B is independent)
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"))   # quiet ON
assert not (bot.live.get("s") or {}).get("awaiting_tg_reply"), "the latch starts clear"
ma.peek = lambda session, lines=40: "Do you want to proceed?\n❯ 1. Yes\n  2. No\n"
bot.waiting._since["s"] = 1000.0
_grace = tg.PERM_GRACE_SECS
tg.PERM_GRACE_SECS = 10.0     # pin: the shipped default is Jan's preference, not a constant
saved = _patch_time(1011.0)   # past the pinned grace window
bot._check_permission_prompts([{"name": "s", "status": "waiting"}])
qrings = [x for x in mt.sent if x[3] is False]
assert len(qrings) == 1 and "🔐 s" in qrings[0][1], f"a permission question rings while quiet + latch clear: {mt.sent}"
# ...and it rings with the FLEET /quiet flag on too: `question` is excluded from
# /quiet by design (Principle 5 — never lose a prompt to be quiet).
bot.topics.set_quiet(True)
bot.prompts.clear("s"); bot.waiting._since["s"] = 1000.0
ma.peek = lambda session, lines=40: "Different question?\n❯ 1. Yes\n  2. No\n"
bot._check_permission_prompts([{"name": "s", "status": "waiting"}])
assert len([x for x in mt.sent if x[3] is False]) == 2, \
    f"/quiet must NOT silence a permission question: {mt.sent}"
bot.topics.set_quiet(False)
tg.PERM_GRACE_SECS = _grace
tg.time.time = saved
print("question-quiet ok — a permission prompt rings with the latch clear AND with fleet /quiet on (never gated)")


# ── Q6. limit episode: rings once via the shared limit_rung key; the usage-limit
#        system row in the same episode is deduped; leave+re-enter rings again;
#        a MUTED session's limit does NOT ring ──────────────────────────────────
def limit_row(name):
    """A RATE limit — self-resolving. amux answers that menu itself (ethos D2),
    so as of 2026-08-18 (plan chat-improvement.md C2c) it lands SILENTLY."""
    return {"name": name, "rate_limit_banner": "Claude usage limit reached"}


def credit_row(name):
    """A CREDIT limit — terminal. Nothing amux does resolves it; a human must
    act, so it still RINGS."""
    return {"name": name, "credit_limited": True}


def idle_row(name):
    return {"name": name, "status": "idle"}


bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"))
# (1) enter a RATE limit -> SILENT (C2c), but still claims the episode key so the
#     next poll doesn't re-decide it. The old assertion here was "rings once"; it is
#     changed rather than deleted, because a rate limit resolves itself and buzzing
#     the phone for it was noise. (3) below proves the system row stays deduped.
bot._check_limit_rings([limit_row("s")])
assert [x for x in mt.sent if x[3] is False] == [], \
    f"C2c: a self-resolving RATE limit must not ring: {mt.sent}"
assert bot.live.get("s")["limit_rung"] is True, "a silent limit still claims the episode"
# (1b) a CREDIT limit is TERMINAL — nothing amux does fixes it, so it still rings.
botc, mtc, _ = make_bot(topics={"c": 100}, outbound_state=seeded("c"))
botc._check_limit_rings([credit_row("c")])
crings = [x for x in mtc.sent if x[3] is False]
assert len(crings) == 1, f"a CREDIT limit is terminal and must still ring: {mtc.sent}"
assert botc.live.get("c")["limit_rung"] is True
botc._check_limit_rings([credit_row("c")])
assert len([x for x in mtc.sent if x[3] is False]) == 1, "staying credit-limited must not re-ring"
# (1c) /quiet on silences even the terminal credit limit — the ONE cell it changes.
botq, mtq, _ = make_bot(topics={"q": 100}, outbound_state=seeded("q"))
botq.topics.set_quiet(True)
botq._check_limit_rings([credit_row("q")])
assert [x for x in mtq.sent if x[3] is False] == [], \
    f"/quiet on routes a terminal failure to the live box: {mtq.sent}"
# (2) still in limit next poll -> nothing new.
bot._check_limit_rings([limit_row("s")])
assert [x for x in mt.sent if x[3] is False] == [], "staying in limit must not ring"
# (3) the usage-limit SYSTEM row in the same episode is deduped -> silent (shared key).
saved = _patch_time(1000.0)
ma.threads["s"] = {"cursor": 0, "thread": [
    owner_item("o1", "telegram", 10),
    {"id": "sys-1", "role": "system", "text": "usage limit", "ts": 20, "seq": None}]}
bot.forward_session("s", status_label="limit")
sysdup = [x for x in mt.sent if "usage limit" in x[1] and "reached" not in x[1]]
assert len(sysdup) == 1 and sysdup[0][3] is True, f"the usage-limit system row is deduped to silent: {sysdup}"
tg.time.time = saved
# (4) leave limit -> clears the key; re-enter a CREDIT limit -> rings again.
bot._check_limit_rings([idle_row("s")])
assert bot.live.get("s")["limit_rung"] is False, "leaving limit clears the shared key"
botc._check_limit_rings([idle_row("c")])
assert botc.live.get("c")["limit_rung"] is False
botc._check_limit_rings([credit_row("c")])
assert len([x for x in mtc.sent if x[3] is False]) == 2, "re-entering a credit limit rings again"
print("limit-dedup ok — rate limits silent (C2c), credit limits ring once per episode, /quiet silences even those")

# a MUTED session's limit must NOT ring (explicit is_muted guard, outside the loop break).
bot2, mt2, ma2 = make_bot(topics={"m": 100}, outbound_state=seeded("m"))
bot2.topics.mute("m")
bot2._check_limit_rings([credit_row("m")])
assert mt2.sent == [], "a muted session's limit must NOT ring"
assert not (bot2.live.get("m") or {}).get("limit_rung"), "a muted limit does not consume the shared key"
print("limit-mute ok — a muted session's limit transition is silent (mute is absolute, fails included)")


# ── Q7. TG_QUIET_DEFAULT=0 -> legacy: a telegram-origin autonomous final rings
#        (latch forced off, window forced open) — the kill switch ───────────────
bot, mt, ma = make_bot(topics={"s": 100}, outbound_state=seeded("s"), presence=False,
                       quiet_default=0)
bot.outbound.observe_owner("s", "telegram")
saved = _patch_time(1000.0)
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10),
                                           reply_item("R:1", "legacy answer", 20, 1)]}
bot.forward_session("s", status_label="active")
tg.time.time = lambda: 1001.0
ma.threads["s"] = {"cursor": 1, "thread": [owner_item("o1", "telegram", 10)]}
bot.forward_session("s", status_label="idle")
tg.time.time = lambda: 1006.0
bot.forward_session("s", status_label="idle")
rings = [x for x in mt.sent if x[3] is False]
assert len(rings) == 1 and rings[0][1] == "legacy answer", f"quiet off -> legacy telegram final rings (no latch): {rings}"
tg.time.time = saved
print("quiet-off-legacy ok — TG_QUIET_DEFAULT=0 restores the exact legacy ring-on-telegram-final behavior")


print("\nALL TELEGRAM-SILENT CHECKS PASSED")


def test_telegram_silent():
    """The scenarios above execute at import time and raise on any regression;
    reaching here means route_reply's truth table, should_type, elapsed_bucket,
    FinalityTracker, LiveStore, live_trim/_live_render trimming, the promotion
    tail (Hazard 1), the live-box edit wrapper (Hazard 2), creation-on-inject
    (E2), the presence toggles, the autonomous-loop guard, and the out-of-scope
    system-row path all hold."""
    assert True
