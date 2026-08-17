#!/usr/bin/env python3
# UTILITY, not the old Python server: audits frustrations.md CARD: pointers against the live board over HTTP (server-agnostic).
"""Audit frustrations.md's CARD: pointers against the live board (AF-28).

The protocol rests on this field. `.claude/rules/frustrations.md` requires every entry to
link a card ("a frustration without a CARD: is a complaint, with one it is work somebody
can pick up"), and the deletion protocol keys an author's confirmation to the entry->card
pair. Nothing validated it, so on 2026-08-09 five of thirty-four entries queued for
deletion pointed at cards about something else entirely — one of them another session's
OPEN card, which was seconds from receiving "validated, deleting" text.

Why the field rots by construction, rather than by carelessness:
  - ids are hand-typed into markdown, with no write path that could check them
  - boards are per-instance, so an id valid on one board silently names a different card
    on another (AC-*, AMUX-*, AH-*, MS-* are not one namespace)
  - supersede entries get filed under the ORIGINAL entry's card id, so one id legitimately
    covers several entries and "delete AC-300" is ambiguous
From the file alone, a stale id and a colliding id are indistinguishable.

Exit codes: 0 clean, 1 problems found, 2 could not reach the board (NOT a pass — an
unreachable board means unchecked, and this says so rather than exiting 0).

    python3 scripts/frustrations_audit.py [--quiet]
"""
import json
import os
import re
import ssl
import sys
import urllib.request
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
FRUST = REPO / "frustrations.md"
REQUIRED = ["AREA", "SEVERITY", "STATUS", "DATE", "SESSION", "CARD", "SYMPTOM", "COST", "FIX"]


def parse(text):
    """Entries are '## ' at column 0, AFTER the header's `---` rule.

    Two separate false positives live here and both make the audit useless in the same
    way — by crying wolf on every run, which is how a check gets ignored:

      - the field TEMPLATE inside the header is indented two spaces precisely so it does
        not match (the file says so itself: "an instrument that measures itself is the
        bug this file exists to record"), and
      - the header's own SECTION HEADING, `## Format — fixed fields so this greps`, IS at
        column 0 and is not an entry. The first cut of this audit reported it as an entry
        missing all nine required fields, on every run, forever.

    Anchoring on the `---` that closes the header fixes both structurally rather than by
    string-matching a heading that someone will eventually reword."""
    body = text.split("\n---\n", 1)
    text = body[1] if len(body) > 1 else text
    out = []
    for blk in re.findall(r'(?ms)^## .*?(?=\n## |\Z)', text):
        e = {"title": blk.split("\n", 1)[0][3:].strip(), "_raw": blk}
        for f in REQUIRED:
            m = re.search(r'(?m)^%s:\s*(.*)$' % f, blk)
            e[f] = m.group(1).strip() if m else None
        out.append(e)
    return out


def structure_check(text, entries):
    """Cross-check the entry count against an INDEPENDENT signal, and fail loud.

    Added 2026-08-14 at amux-cloud's suggestion, after their catch. A session
    (me) audited this file with an ad-hoc parser that split entries on `DATE:`.
    Field ORDER varies here — plenty of entries put STATUS: above DATE: — so
    every such entry's STATUS bound to the PREVIOUS entry and it inherited the
    NEXT one's. The error ran in the only direction that costs something: OPEN
    entries reading as `fixed`, i.e. proposed for DELETION, which is the single
    irreversible step in the validate-and-delete loop. An open entry recording a
    live, thrice-regressed incident was on that list.

    The discriminator existed the whole time and nobody was routed to it: this
    script said 122 entries, the ad-hoc parse said 127. Both numbers were read in
    the same session and never compared. So the fix is not "write better
    parsers" — it is to make the disagreement ANNOUNCE ITSELF from the canonical
    tool, because the next person will also write an ad-hoc parse and will also
    have no reason to suspect it.

    One DATE: and one STATUS: per entry is the file's own contract. If either
    tally drifts from the '## ' heading count, something is malformed OR someone
    is about to be misled, and both are worth stopping for.
    """
    body = text.split("\n---\n", 1)
    body = body[1] if len(body) > 1 else text
    problems = []
    for field in ("DATE", "STATUS"):
        n = len(re.findall(r'(?m)^%s:' % field, body))
        if n != len(entries):
            problems.append(
                "  %s: %d occurrence(s) vs %d entries (delta %+d)"
                % (field, n, len(entries), n - len(entries))
            )
    if problems:
        print("STRUCTURE DRIFT — the entry count disagrees with its own fields:")
        print("\n".join(problems))
        print("  Entries are '## ' headings. Do NOT split on DATE: — field order")
        print("  varies, and a DATE-split silently shifts STATUS by one entry")
        print("  (open -> fixed), which proposes live entries for deletion.")
        print("  Canonical count from this script: %d" % len(entries))
        return False
    return True


def fetch_board():
    # The DEFAULT here is already correct; the hazard is the ENV VAR overriding it
    # with a dead address. 8822 was the Python compatibility bind, removed
    # 2026-08-11, and every session spawned before that still carries the old
    # AMUX_URL in its process env — which a live process cannot re-read. So the
    # override has to be ignored when it names the retired port.
    #
    # This fails SILENTLY otherwise: fetch_board() raises, main() prints "Structural
    # checks only" and exits 2, which reads like a deliberate offline mode rather
    # than a broken probe. It ran that way for a full sweep before anyone noticed.
    base = os.environ.get("AMUX_URL", "") or "https://localhost:8824"
    if base.rstrip("/").endswith(":8822"):
        base = "https://localhost:8824"
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    ids = {}
    # done_limit matters: the default GET caps done items, so a plain fetch reports live
    # cards as missing. That cap is what made a first pass at this audit claim 48 absent
    # cards when the real number was 1.
    for q in ("?done_limit=100000", "?done_limit=100000&archived=1"):
        req = urllib.request.Request(base + "/api/board" + q)
        for i in json.load(urllib.request.urlopen(req, context=ctx, timeout=30)):
            ids[i["id"]] = i
    return ids


def overlap(a, b):
    """Loose word overlap. Deliberately crude and deliberately only ADVISORY: card titles
    get rewritten as understanding improves, so a low score is 'a human should look',
    never 'this is wrong'. Reporting it as an error would train people to ignore it."""
    w1 = {w.lower().strip('`",.:;()') for w in a.split() if len(w) > 4}
    w2 = {w.lower().strip('`",.:;()') for w in b.split() if len(w) > 4}
    if not w1 or not w2:
        return 1.0
    return len(w1 & w2) / min(len(w1), len(w2))


def main():
    quiet = "--quiet" in sys.argv
    raw = FRUST.read_text()
    entries = parse(raw)
    # Before any per-entry finding: does the file's own shape agree with itself?
    # A structural drift makes every downstream verdict suspect, so it is
    # reported FIRST rather than buried under 122 lines of per-entry output.
    structure_ok = structure_check(raw, entries)
    problems, advisories = [], []

    for e in entries:
        miss = [f for f in REQUIRED if not e.get(f)]
        if miss:
            problems.append("%-52s missing field(s): %s" % (e["title"][:52], ", ".join(miss)))

    # Duplicate ids are NOT automatically wrong — a supersede entry under the original's
    # id is legitimate and amux-cloud does it deliberately. But it means the id cannot be
    # used as a delete key, which is the trap that nearly destroyed the wrong two of four
    # AC-300 entries. Report it so anyone scripting against this file knows.
    dupes = defaultdict(list)
    for e in entries:
        if e.get("CARD") and e["CARD"].lower() != "none":
            dupes[e["CARD"]].append(e["title"])
    shared_ids = {k: v for k, v in dupes.items() if len(v) > 1}

    try:
        board = fetch_board()
    except Exception as ex:
        print("CANNOT REACH BOARD: %s" % ex)
        print("Structural checks only. %d entries, %d structural problem(s)."
              % (len(entries), len(problems)))
        for p in problems:
            print("  PROBLEM  " + p)
        return 2

    for e in entries:
        c = e.get("CARD")
        if not c or c.lower() == "none":
            continue
        card = board.get(c)
        if not card:
            # Cross-instance ids are expected (AC-* live on amux-cloud's board). Flag as
            # advisory rather than error, but SAY it, so "not on this board" is a known
            # state rather than a silent hole in the protocol.
            advisories.append("%-10s not on this board (other instance, or deleted) :: %s"
                              % (c, e["title"][:46]))
            continue
        ov = overlap(e["title"], card.get("title", ""))
        if ov <= 0.3:
            advisories.append("%-10s TITLE MISMATCH (%.2f)\n              entry: %s\n              card:  %s"
                              % (c, ov, e["title"][:70], card.get("title", "")[:70]))

    if not quiet or problems or advisories:
        print("frustrations.md audit — %d entries" % len(entries))
        for p in problems:
            print("  PROBLEM   " + p)
        for a in advisories:
            print("  CHECK     " + a)
        if shared_ids:
            print("  NOTE      %d card id(s) cover multiple entries — id is NOT a delete key:"
                  % len(shared_ids))
            for k, v in sorted(shared_ids.items()):
                print("              %s x%d" % (k, len(v)))
        if not problems and not advisories:
            print("  clean — every CARD: resolves and plausibly matches its entry")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
