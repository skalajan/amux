#!/usr/bin/env python3
"""C1 coverage gate — every notification decision passes through ONE funnel.

This is the check whose absence let `_send_prompt_notify` hold a literal
`disable_notification=False` for months: it sat outside the router, no control
could reach it, nothing recorded that it fired, and its own docstring advertised
that it overrode /ring off. Nothing could have caught that, because nothing
enumerated the send sites.

Two properties, both able to fail:
  1. Every `self.tg.send_message(...)` call site either consults `_decide` or is
     on the explicit exemption list below (with a reason).
  2. A decision increments the counters AND emits exactly one `tg-decision` log
     line — so the counters and the log can be diffed, and a send that skips the
     funnel shows up as a mismatch rather than as silence.
"""
import importlib.util
import logging
import re
import sys
import os

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(os.path.dirname(HERE), "amux-telegram.py")

spec = importlib.util.spec_from_file_location("tg", SRC)
tg = importlib.util.module_from_spec(spec)
try:
    spec.loader.exec_module(tg)
except SystemExit:
    pass

# ── 1. send-site coverage ──────────────────────────────────────────────────────
# Sends that answer something Jan just typed. He is looking at the screen; the
# reply IS what he is waiting for. Putting these under a quiet flag would make
# the bot look dead when he talks to it.
EXEMPT = {
    "_reply":     "command acks — Jan typed a second ago and is watching",
    "_cmd_last":  "answers an explicit /last request",
}
# Sites that EXECUTE a class already chosen by _decide, rather than choosing one.
EXECUTORS = {
    "_ring_final":          "sends the 'ring' _promote_final decided",
    "_live_create":         "sends the 'live' box the router decided",
    "_ring_failure":        "sends the 'ring' _check_limit_rings decided",
}

src = open(SRC).read().splitlines()


def enclosing(idx):
    for j in range(idx, -1, -1):
        m = re.match(r"^    def (\w+)", src[j])
        if m:
            return m.group(1)
    return "?"


sites = {}
for i, line in enumerate(src):
    if re.search(r"self\.tg\.send_message\(", line):
        sites.setdefault(enclosing(i), []).append(i + 1)

assert sites, "found no send sites at all — the gate itself is broken"

bodies = {}
for fn in sites:
    start = next(j for j, l in enumerate(src) if re.match(rf"^    def {fn}\b", l))
    end = next((j for j in range(start + 1, len(src))
                if re.match(r"^    def |^class ", src[j])), len(src))
    bodies[fn] = "\n".join(src[start:end])

uncovered = []
for fn, lines in sites.items():
    if fn in EXEMPT or fn in EXECUTORS:
        continue
    if "self._decide(" not in bodies[fn]:
        uncovered.append((fn, lines))

assert not uncovered, (
    "send site(s) choose a notification class without consulting the router "
    f"and are not documented as exempt: {uncovered}. Either route it through "
    "self._decide(...) or add it to EXEMPT/EXECUTORS with a reason.")
print(f"coverage ok — {len(sites)} send sites: "
      f"{len(sites) - len(EXEMPT) - len(EXECUTORS)} routed, "
      f"{len(EXEMPT)} exempt, {len(EXECUTORS)} executors")

# The gate must be able to fail: prove it detects a bypass.
_probe = dict(bodies)
_probe["_send_prompt_notify"] = "    def _send_prompt_notify(self):\n        pass"
_would_fail = [fn for fn in sites
               if fn not in EXEMPT and fn not in EXECUTORS
               and "self._decide(" not in _probe[fn]]
assert _would_fail, "the coverage gate cannot fail — it would pass a bypassed send site"
print(f"gate-can-fail ok — a bypassed _send_prompt_notify is detected ({_would_fail})")

# ── 2. counters and the tg-decision log agree ──────────────────────────────────
class _Rec(logging.Handler):
    def __init__(self):
        super().__init__()
        self.lines = []

    def emit(self, record):
        self.lines.append(record.getMessage())


rec = _Rec()
tg.log.addHandler(rec)
tg.log.setLevel(logging.INFO)


class _Topics:
    def is_quiet(self):
        return False

    def is_ring_off(self, s):
        return False


bot = tg.Bot.__new__(tg.Bot)
bot.topics = _Topics()
bot.counters = tg.CounterStore("/dev/null", {})

before = bot.counters.total()
assert bot._decide("s", "question", rule="t") == "ring"
assert bot._decide("s", "failure", terminal=False, rule="t") == "live"
assert bot._decide("s", "failure", terminal=True, rule="t") == "ring"
assert bot._decide("s", "reply", is_final=False, rule="t") == "suppress"
after = bot.counters.total()

decisions = [l for l in rec.lines if l.startswith("tg-decision ")]
assert after - before == 4, f"every decision must bump a counter: {after - before}"
assert len(decisions) == 4, f"every decision must log exactly one line: {len(decisions)}"
assert bot.counters.total("ring") == 2 and bot.counters.total("live") == 1 \
    and bot.counters.total("suppress") == 1, bot.counters.to_dict()

# The log line must carry the router's INPUTS, not just its verdict — a replay
# harness needs inputs; a log of conclusions cannot be replayed.
for field in ("session=", "kind=", "is_final=", "ring_off=", "latch_armed=",
              "window_open=", "origin_tg=", "quiet=", "terminal=", "class=", "rule="):
    assert all(field in l for l in decisions), f"tg-decision must carry {field}"
print(f"counters ok — {len(decisions)} decisions, counters match the log, full input tuple recorded")

# ── 3. /quiet changes exactly ONE cell of the table ────────────────────────────
def grid(quiet):
    out = {}
    for kind in ("question", "failure", "reply"):
        for fin in (True, False):
            for ro in (True, False):
                for lat in (True, False):
                    for win in (True, False):
                        for otg in (True, False):
                            for term in (True, False):
                                k = (kind, fin, ro, lat, win, otg, term)
                                out[k] = tg.notify_class(kind, fin, ro, lat, win, otg,
                                                         quiet, term)
    return out


off, on = grid(False), grid(True)
changed = {k for k in off if off[k] != on[k]}
assert changed, "/quiet changes nothing — the flag is wired to no branch"
kinds = {k[0] for k in changed}
assert kinds == {"failure"}, f"/quiet must only affect failures, got {kinds}"
assert all(k[6] is True for k in changed), "/quiet must only affect TERMINAL failures"
assert all(off[k] == "ring" and on[k] == "live" for k in changed), \
    "the one cell /quiet changes is terminal-failure ring -> live"
# Principle 5: a question is never gated, by any control, in any combination.
assert all(v == "ring" for k, v in on.items() if k[0] == "question"), \
    "a permission question must ring regardless of /quiet — never lose a prompt to be quiet"
assert all(v == "ring" for k, v in off.items() if k[0] == "question")
# The answer-latch survives /quiet: silencing it is the defect the latch prevents.
latched = [k for k in on if k[0] == "reply" and k[1] and not k[2] and k[3]]
assert latched and all(on[k] == "ring" for k in latched), \
    "the answer-latch must still ring under /quiet — it is the answer to Jan's own question"
print(f"quiet-scope ok — /quiet changes exactly {len(changed)} cells, all terminal failures; "
      "questions and the answer-latch are never gated")

print("\nALL TELEGRAM-INSTRUMENTATION CHECKS PASSED")
