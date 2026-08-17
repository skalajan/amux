#!/usr/bin/env python3
"""God-mode walkthrough: sign in as hello@amux.io and evidence every customer env.

WHY THIS EXISTS. Ethan's gate for `verified` on the cloud environment cards is not
"the API says the workers are there" — it is a PEER signing into god mode, moving
between every customer environment, and confirming the workers exist with LOGS and
FILES that demonstrate the workflow. This script does the tedious half (auth, org
switching, evidence collection) so the reviewer spends their judgement on whether
the evidence is convincing, not on rebuilding a Clerk MFA flow.

IT IS NOT A PASS/FAIL ORACLE. It prints what it found and does not conclude. A
reviewer reading "3 workers, 11 tool calls, 4 demo files" decides whether that
demonstrates a workflow; this script must not decide it for them, because the
whole point of the gate is a second pair of eyes.

AUTH CHAIN (no mailbox, no OAuth — see sign_in() for why it changed):
  1. Clerk BACKEND sign-in token      api.clerk.com + E2E_CLERK_SECRET_KEY + AMUX_QA_USER_ID
  2. ticket -> complete sign-in       no second factor, no inbox poll
  3. Clerk session JWT
  4. POST /api/cloud-auth             exchanges the JWT for an amux_session cookie
  5. amux_org cookie                  switches which workspace the gateway proxies to

Credentials come from ~/.amux/server.env (chmod 600, outside every git repo). This
file contains NO secrets — see docs/credentials.md.

USAGE
  python3 cloud/tests/godmode_walkthrough.py            # all three customers
  python3 cloud/tests/godmode_walkthrough.py org_xxx    # one org
"""
import json
import os
import re
import ssl
import subprocess
import sys
import time
import urllib.parse
import urllib.request

CLERK = "https://clerk.amux.io"
CLOUD = "https://cloud.amux.io"
AMUX = os.environ.get("AMUX_URL", "https://localhost:8822")
Q = "__clerk_api_version=2021-02-05&_clerk_js_version=5.0.0"
UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36"

# The real customer environments. Deliberately a literal list rather than "every
# org": 60 orgs exist and 56 are personal workspaces belonging to individual
# signups. Walking those would be reading strangers' workspaces to test our own
# demo, which is not ours to do (ethos rule 8).
CUSTOMERS = {
    "org_18f676d91310d02f": "Wexus Group — AR Reconciliation",
    "org_37aa24eb89c1d97a": "Capital Express (jacob@)",
    "org_02699c9c2866c5cf": "Capital Express — Terry (terry@)",
}

_CTX = ssl.create_default_context()
_CTX.check_hostname = False
_CTX.verify_mode = ssl.CERT_NONE


def env(key):
    path = os.path.expanduser("~/.amux/server.env")
    for line in open(path):
        if line.startswith(key + "="):
            return line.split("=", 1)[1].strip().strip('"')
    raise SystemExit(f"{key} not in {path} — see docs/credentials.md")


def sign_in():
    """Full god-mode sign-in. Returns the amux_session cookie value.

    NO MAILBOX, NO OAUTH, NO SECOND FACTOR (AC-282). This used to be: password
    first factor -> Clerk emails a 6-digit code -> read it from the hello@amux.io
    inbox via the Gmail API -> submit it. That made SIGNING IN depend on READING AN
    INBOX, and therefore on a Gmail OAuth grant that can be revoked silently. On
    2026-08-07 it was: hello@amux.io's token returned invalid_grant, the rig died
    at "no verification code arrived", and the failure read as Clerk not sending
    the code. Wrong owner, three probes wasted.

    Now: mint a sign-in token with the Clerk BACKEND api (E2E_CLERK_SECRET_KEY +
    AMUX_QA_USER_ID, both already in server.env), exchange it for a session via the
    `ticket` strategy, take the JWT. A backend-minted ticket needs no factors at
    all, so the whole fragile leg is deleted rather than re-plumbed.

    Measured before being chosen, not after: qa-godmode@amux.io hits the SAME
    email_code requirement as hello@, so swapping identities does not avoid the
    coupling (and qa-godmode has no Gmail token at all). Meanwhile the backend api
    reports that user as two_factor_enabled: FALSE — so the second factor is
    INSTANCE-level config, not per-user MFA. Plain username/password is reachable
    by a Clerk config change; this path does not need it either way.
    """
    # hello@amux.io is the god-mode identity Ethan named, so mint the ticket for
    # THAT user rather than the pre-existing qa-godmode account. Both are in
    # ADMIN_EMAILS and either would work; using the named one keeps the evidence
    # this rig produces attributable to the account a reviewer expects to see.
    key, uid = env("E2E_CLERK_SECRET_KEY"), env("AMUX_GODMODE_USER_ID")

    # api.clerk.com is behind Cloudflare and answers 403 "error code: 1010" to a
    # default urllib user-agent. That is a browser-signature block, NOT an auth
    # failure — it reads exactly like a bad secret key and cost a probe to tell
    # apart. Send a normal UA.
    print("  [1/3] minting a Clerk sign-in token (backend api, no factors)")
    out = subprocess.run(
        ["curl", "-s", "-w", "\n%{http_code}", "-X", "POST",
         "-H", f"Authorization: Bearer {key}", "-H", "Content-Type: application/json",
         "-A", UA, "https://api.clerk.com/v1/sign_in_tokens",
         "-d", json.dumps({"user_id": uid, "expires_in_seconds": 300})],
        capture_output=True, text=True, timeout=60).stdout
    body, _, code = out.rpartition("\n")
    if code.strip() != "200":
        raise SystemExit(f"  sign-in token refused (HTTP {code.strip()}): {body[:200]}\n"
                         f"  403 with 'error code: 1010' is Cloudflare blocking the "
                         f"user-agent, not a bad key.")
    ticket = json.loads(body).get("token", "")

    import http.cookiejar
    jar = http.cookiejar.CookieJar()
    op = urllib.request.build_opener(
        urllib.request.HTTPCookieProcessor(jar),
        urllib.request.HTTPSHandler(context=_CTX))

    def fapi(path, data=None):
        req = urllib.request.Request(
            f"{CLERK}{path}?{Q}", data=urllib.parse.urlencode(data or {}).encode(),
            headers={"Content-Type": "application/x-www-form-urlencoded",
                     "Origin": "https://accounts.amux.io", "User-Agent": UA})
        try:
            with op.open(req, timeout=40) as r:
                return json.loads(r.read().decode())
        except urllib.error.HTTPError as e:
            return json.loads(e.read().decode() or "{}")

    print("  [2/3] exchanging the ticket for a session")
    d = fapi("/v1/client/sign_ins", {"strategy": "ticket", "ticket": ticket})
    r = d.get("response") or d
    if r.get("status") != "complete":
        raise SystemExit("  ticket exchange did not complete: " +
                         json.dumps(d.get("errors") or r.get("status"))[:200])
    jwt = (fapi(f"/v1/client/sessions/{r['created_session_id']}/tokens", {}) or {}).get("jwt", "")

    print("  [3/3] exchanging the JWT for an amux_session cookie")
    req = urllib.request.Request(
        f"{CLOUD}/api/cloud-auth", method="POST",
        data=json.dumps({"token": jwt, "email": env("AMUX_GODMODE_EMAIL")}).encode(),
        headers={"Content-Type": "application/json", "User-Agent": UA})
    with op.open(req, timeout=60) as resp:
        for c in resp.headers.get_all("Set-Cookie") or []:
            if c.startswith("amux_session="):
                return c.split("=", 1)[1].split(";")[0]
    raise SystemExit("  gateway did not mint an amux_session cookie")


def get(cookie, path, org=None, timeout=90):
    ck = f"amux_session={cookie}" + (f"; amux_org={org}" if org else "")
    req = urllib.request.Request(
        f"{CLOUD}{path}",
        headers={"Cookie": ck, "Accept": "application/json", "User-Agent": "Mozilla/5.0"})
    try:
        with urllib.request.urlopen(req, context=_CTX, timeout=timeout) as r:
            body = r.read().decode()
            try:
                return r.status, json.loads(body)
            except Exception:
                return r.status, body
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()[:200]
    except Exception as e:
        return 0, str(e)[:120]


def wake(cookie, org, tries=20):
    """Wait for the org container to be healthy. Returns True if it came up.

    Containers are STOPPED by default since AC-231 set every restart policy to
    `no` to stop the boot-time thundering herd — so the gateway starts one on
    demand and answers 503 {"error":"starting"} meanwhile. A walkthrough that
    treats that first 503 as a verdict reports a healthy environment as broken,
    which is a false negative in the direction that wastes a reviewer's time and
    makes them distrust the rig.
    """
    for i in range(tries):
        st, _ = get(cookie, "/api/sessions", org, timeout=60)
        if st == 200:
            if i:
                print(f"  container woke after ~{i * 6}s")
            return True
        if st != 503:
            return False
        if i == 0:
            print("  container asleep (expected — AC-231 leaves them stopped); waking…")
        time.sleep(6)
    return False


def walk(cookie, org, label):
    print(f"\n{'=' * 72}\n  {label}\n  {org}\n{'=' * 72}")
    if not wake(cookie, org):
        print("  CONTAINER DID NOT COME UP — cannot evidence this env right now.")
        return

    st, sessions = get(cookie, "/api/sessions", org)
    if not isinstance(sessions, list):
        print(f"  SESSIONS UNAVAILABLE (HTTP {st}): {str(sessions)[:120]}")
        return
    workers = [s for s in sessions if s.get("name") != "hello-world"]
    print(f"  WORKERS: {len(sessions)} sessions, {len(workers)} beyond hello-world")

    st, scheds = get(cookie, "/api/schedules", org)
    if isinstance(scheds, list):
        on = [s for s in scheds if s.get("enabled")]
        print(f"  SCHEDULES: {len(scheds)} total, {len(on)} ENABLED "
              f"({'manual-trigger only ✓' if not on else 'SOME AUTO-FIRE — check this'})")
        for s in scheds:
            print(f"    [{'ON ' if s.get('enabled') else 'off'}] {(s.get('title') or '')[:42]:44s}"
                  f" -> {s.get('session','?')}")

    # BOARD ISSUES. Ethan named this dimension explicitly — "board issues all have
    # intuitive named" — and the rig did not report it at all, so the one instrument
    # a reviewer uses to satisfy that gate criterion could not express it. A gate whose
    # evidence cannot be produced is the ethos rule-4 shape: the reviewer would have had
    # to take it on faith or go hunting by hand.
    #
    # Prints the TITLES, not a count. "12 issues" cannot be judged for intuitiveness;
    # the whole question is whether a human reading them understands the workflow. The
    # capture-shell heuristic is a HINT, not a verdict — a title that starts like a
    # pasted prompt is the specific failure mode seen on the local board (AMUX-2474),
    # and flagging it tells the reviewer where to look without deciding for them.
    # NO archived filter. The first version asked for archived=0 and reported
    # "0 open issues" for Capital Express (jacob@) — which has SEVEN, all archived
    # and all still in `todo`, including QA scaffolding ("Reply with exactly:
    # QUOTA-OK"). That is the ethos rule-1 trap in its purest form: a view that
    # does not share the predicate of the thing it claims to describe, reporting
    # an empty board as evidence of a clean one. Archived cards are exactly what a
    # demo reviewer needs to see, because a prospect who opens the board finds
    # either nothing or leftover test cards.
    st, issues = get(cookie, "/api/board?slim=1", org)
    if isinstance(issues, list):
        shells = [i for i in issues
                  if re.match(r"(?i)^(capture|captured|prompt)\b", (i.get("title") or "")
                              ) or len(i.get("title") or "") > 90]
        _arch = [i for i in issues if i.get("archived")]
        print(f"\n  BOARD: {len(issues)} issue(s), {len(_arch)} ARCHIVED"
              + (f"  — {len(shells)} look like undecomposed captures, REVIEW THESE"
                 if shells else "  — none look like raw captures"))
        for i in issues[:12]:
            flag = "!" if i in shells else " "
            _a = "arch " if i.get("archived") else "     "
            print(f"   {flag}{_a}[{(i.get('status') or '?')[:9]:9s}] {(i.get('title') or '')[:68]}")
        if len(issues) > 12:
            print(f"     … +{len(issues) - 12} more (printed 12; not a sample — the rest are on the board)")
    else:
        print(f"\n  BOARD UNAVAILABLE (HTTP {st}): {str(issues)[:100]}")

    for w in workers:
        name = w.get("name")
        print(f"\n  --- {name} ({w.get('status')}) ---")
        print(f"      {(w.get('desc') or '')[:100]}")
        st, d = get(cookie, f"/api/sessions/{name}/peek?lines=400", org)
        hist = (d.get("history") or d.get("output") or "") if isinstance(d, dict) else ""
        clean = re.sub(r"\x1b\[[0-9;]*m", "", hist)
        tools = len(re.findall(r"⏺\s+\*?\*?(Read|Edit|Write|Bash|Grep|Glob)", clean))
        lines = [l for l in clean.split("\n") if l.strip()]
        print(f"      LOGS: history_lines={d.get('history_lines') if isinstance(d,dict) else '?'}"
              f"  tool_calls={tools}")
        for l in lines[-4:]:
            print(f"        | {l[:104]}")

    # /api/fs/list, not /api/files — the latter does not exist and 404s, which
    # reads as "no files" rather than "wrong endpoint". A reviewer would have
    # taken that as evidence the workflow produced nothing.
    # Try each env's own layout. Wexus keeps its workbooks in /root/wexus while
    # the Capital Express envs use /root/demo — assuming one path reports a
    # populated environment as empty, which is the worst direction for a demo
    # review: it reads as "the workflow produced nothing".
    st, files = 0, None
    for _cand in ("/root/demo", "/root/wexus", "/root"):
        st, files = get(cookie, f"/api/fs/list?path={_cand}", org)
        if isinstance(files, dict) and any(isinstance(files.get(k), list)
                                           for k in ("entries", "files", "items", "list")):
            demo_dir = _cand
            break
    else:
        demo_dir = "/root/demo"
    ents = files.get("entries") if isinstance(files, dict) else None
    if ents is None and isinstance(files, dict):
        for k in ("files", "items", "list"):
            if isinstance(files.get(k), list):
                ents = files[k]
                break
    if ents is not None:
        print(f"\n  FILES in {demo_dir}: {len(ents)} — the workflow's inputs and outputs")
        for f in ents[:14]:
            nm = f.get("name") if isinstance(f, dict) else str(f)
            sz = f.get("size", "?") if isinstance(f, dict) else "?"
            print(f"    {str(nm)[:44]:46s} {sz} bytes")
    else:
        print(f"\n  FILES: no listable demo dir (HTTP {st}): {str(files)[:110]}")


def main():
    targets = {a: CUSTOMERS.get(a, a) for a in sys.argv[1:]} or CUSTOMERS
    print("GOD-MODE WALKTHROUGH — hello@amux.io")
    print("This prints EVIDENCE. It does not decide whether the evidence is sufficient.\n")
    cookie = sign_in()
    print("  signed in ✓")
    st, d = get(cookie, "/api/gateway/admin/orgs")
    orgs = d.get("orgs", d) if isinstance(d, dict) else []
    print(f"  god mode sees {len(orgs)} orgs")
    for org, label in targets.items():
        walk(cookie, org, label)
    print(f"\n{'=' * 72}\n  Reviewer: does the above demonstrate a working workflow per env?\n"
          f"{'=' * 72}")


if __name__ == "__main__":
    main()
