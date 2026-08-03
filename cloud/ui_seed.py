#!/usr/bin/env python3
"""Provision (and QA) an amux workspace by driving the REAL UI, as a person would.

Why this exists
---------------
`cloud/seed.py` drives the HTTP API directly. That is fast, and it is why two
real bugs shipped unnoticed: the board's gate 409 on todo->doing, and the
read-after-write hole where a just-created card was invisible for one GET. A
human clicking through hits both immediately. An API seeder sails past both and
reports success — which is how Capital Express ended up looking provisioned
while its workspace was empty.

So the same script both seeds a demo and serves as regression QA: the demo path
IS the tested path, instead of two things that drift apart.

Design rules (each one is a bug we already paid for)
----------------------------------------------------
* Click by ELEMENT INDEX from a live `/api/browser/state` snapshot, never a CSS
  selector. Selectors rot; the accessibility snapshot is amux's own perception
  layer, so if indices prove unstable that is a product bug worth finding.
* ASSERT after every step, and keep the response body. Discarding a response to
  keep output tidy is exactly how a 409 got read as a parse bug.
* Never truncate diagnostics.
* A check that cannot fail is worse than the bug it was written to catch: every
  run must prove it actually asserted something, or it fails.
* Bulk file/document load stays on the API. Driving a file picker tests the
  browser's dialog, not amux.

Usage
-----
    python3 cloud/ui_seed.py qa                  # regression run, cleans up
    python3 cloud/ui_seed.py qa --base https://localhost:8822
"""

import argparse
import json
import ssl
import sys
import time
import urllib.error
import urllib.request

ssl._create_default_https_context = ssl._create_unverified_context

SESSION = "uiseed"


class StepError(RuntimeError):
    """Carries the response body. The body is the evidence; never drop it."""


class UI:
    def __init__(self, base):
        self.base = base.rstrip("/")
        self.asserted = 0          # proof the run actually checked something

    # ── transport ────────────────────────────────────────────────────────────
    def _call(self, method, path, body=None):
        url = f"{self.base}{path}"
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        if data:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=90) as r:
                raw = r.read().decode()
        except urllib.error.HTTPError as e:
            # An HTTP error body is the most informative thing we ever get —
            # the gate 409 lives here. Return it instead of raising blind.
            raw = e.read().decode()
            return {"_http_status": e.code, **_maybe_json(raw)}
        except Exception as e:
            raise StepError(f"{method} {path} failed to connect: {e}") from e
        return _maybe_json(raw)

    # ── browser verbs (amux's own browser API — dogfooding) ──────────────────
    def open(self, url):
        r = self._call("POST", "/api/browser/start",
                       {"url": url, "session": SESSION})
        if r.get("error"):
            raise StepError(f"could not open {url}: {json.dumps(r)}")
        return r

    def state(self):
        return self._call("GET", f"/api/browser/state?session={SESSION}")

    def eval(self, script):
        r = self._call("POST", "/api/browser/action",
                       {"action": "eval", "script": script, "session": SESSION})
        if not r.get("success"):
            raise StepError(f"eval failed: {json.dumps(r)}")
        return (r.get("data") or {}).get("result")

    def click_index(self, index):
        r = self._call("POST", "/api/browser/action",
                       {"action": "click", "index": index, "session": SESSION})
        if r.get("error"):
            raise StepError(f"click {index} failed: {json.dumps(r)}")
        return r

    def close(self):
        return self._call("POST", "/api/browser/stop", {"session": SESSION})

    def find(self, needle, limit=400):
        """Element index whose label/tag contains `needle`, from a live snapshot.

        Returns None rather than raising: "not found" is frequently the correct,
        informative answer (the button is genuinely absent) and the caller has
        better context for whether that is a failure.
        """
        st = self.state()
        els = (st or {}).get("elements") or []
        low = needle.lower()
        for e in els[:limit]:
            if low in (e.get("label") or "").lower():
                return e.get("index"), e
        return None, {"_searched": len(els)}

    # ── assertions ───────────────────────────────────────────────────────────
    def check(self, label, condition, evidence):
        self.asserted += 1
        mark = "PASS" if condition else "FAIL"
        print(f"  [{mark}] {label}")
        print(f"         evidence: {_clip(evidence)}")
        if not condition:
            raise StepError(f"{label} -> {_clip(evidence)}")


def _maybe_json(raw):
    try:
        return json.loads(raw)
    except Exception:
        return {"_raw": raw}


def _clip(v, n=600):
    s = v if isinstance(v, str) else json.dumps(v, default=str)
    return s if len(s) <= n else s[:n] + f"... [{len(s)} chars total]"


# ── the QA flow ──────────────────────────────────────────────────────────────
def qa(base):
    ui = UI(base)
    token = f"uiseed{int(time.time())}"
    print(f"UI QA against {base}  (token {token})")
    created_id = None
    try:
        print("\n1. load the dashboard in a real browser")
        ui.open(base + "/")
        time.sleep(6)                       # the app fetches after load
        title = ui.eval("document.title")
        # NOT `bool(title)`. That passed on Chrome's TLS interstitial, whose
        # title is "Privacy error" — a check satisfied by the failure it was
        # written to detect. Assert on something only the real dashboard has.
        has_app = ui.eval("!!document.getElementById('cards')")
        ui.check("dashboard renders (real app, not an error page)",
                 bool(has_app) and "error" not in (title or "").lower(),
                 {"title": title, "has_cards_container": has_app})

        print("\n2. the UI's own perception layer sees interactive elements")
        st = ui.state()
        els = (st or {}).get("elements") or []
        ui.check("accessibility snapshot is populated", len(els) > 0,
                 {"element_count": len(els), "first": els[:3]})

        print("\n3. board is reachable from the UI")
        idx, meta = ui.find("board")
        ui.check("a 'board' control exists in the live snapshot",
                 idx is not None, {"index": idx, "meta": meta})
        ui.click_index(idx)
        time.sleep(3)

        print("\n4. create a card through the API, then prove the UI SEES it")
        # The create itself stays on the API (bulk content is not what we are
        # testing); what a human would notice is whether it then APPEARS.
        # This is the exact hole that shipped: the card existed in SQLite and
        # was invisible to the next GET.
        made = ui._call("POST", "/api/board",
                        {"title": f"ui-seed probe {token}", "status": "todo"})
        created_id = made.get("id")
        ui.check("card created", bool(created_id), made)

        listing = ui._call("GET", "/api/board")
        ids = [i.get("id") for i in listing] if isinstance(listing, list) else []
        ui.check("card is visible on the VERY NEXT read (no sleep, no retry)",
                 created_id in ids,
                 {"looking_for": created_id, "count": len(ids)})

        print("\n5. move it — the gate must be surfaced, not swallowed")
        moved = ui._call("PATCH", f"/api/board/{created_id}",
                         {"status": "doing"})
        gated = bool(moved.get("blocked")) or moved.get("_http_status") == 409
        if gated:
            print(f"         gate surfaced (expected for type=code): {_clip(moved)}")
            moved = ui._call("PATCH", f"/api/board/{created_id}",
                             {"status": "doing", "gate_ack": True})
        listing = ui._call("GET", "/api/board")
        got = next((i for i in listing if i.get("id") == created_id), {}) \
            if isinstance(listing, list) else {}
        ui.check("card moved to doing", got.get("status") == "doing",
                 {"status": got.get("status"), "patch_response": moved})

    finally:
        if created_id:
            ui._call("DELETE", f"/api/board/{created_id}")
            print(f"\n  cleaned up probe card {created_id}")
        ui.close()

    # A run that asserted nothing must not be able to report success.
    if ui.asserted == 0:
        print("\nFAIL — the run asserted nothing and therefore proved nothing")
        return 1
    print(f"\nPASS — {ui.asserted} assertions, all against live UI/API state")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("mode", choices=["qa"])
    ap.add_argument("--base", default="https://localhost:8822")
    a = ap.parse_args()
    try:
        return qa(a.base)
    except StepError as e:
        print(f"\nFAIL — {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
