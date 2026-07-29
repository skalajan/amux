#!/usr/bin/env python3
"""
amux cloud E2E trial-provisioning test.

Exercises the admin provision flow end to end: provision a trial workspace by
email (budget + expiry), email-bound invite acceptance, god-mode workspace
access, budget enforcement (402 + session stop), pro upgrade lifting the gate,
then cleans everything up.

Env vars required:
  CLERK_SECRET_KEY   — Clerk backend key
  COOKIE_SECRET      — gateway HMAC secret (doubles as X-E2E-Secret)

Usage:
  python3 cloud/tests/e2e_trial.py [--gateway https://cloud.amux.io]
"""
import argparse, hashlib, hmac, json, os, sys, time, urllib.request, urllib.error, urllib.parse

GATEWAY = os.environ.get("E2E_GATEWAY", "https://cloud.amux.io")
CLERK_SECRET = os.environ.get("CLERK_SECRET_KEY", "")
COOKIE_SECRET = os.environ.get("COOKIE_SECRET", "")
TRIAL_EMAIL = "e2e-trial@test.amux.io"
TRIAL_PASSWORD = "E2eTrial!Test2026x"

PASS = 0
FAIL = 0
WARNINGS = []


def log(msg):
    print(f"  {msg}", flush=True)


def step(msg):
    print(f"\n→ {msg}", flush=True)


def ok(msg):
    global PASS
    PASS += 1
    print(f"  ✓ {msg}", flush=True)


def fail(msg):
    global FAIL
    FAIL += 1
    print(f"  ✗ {msg}", flush=True)


def warn(msg):
    WARNINGS.append(msg)
    print(f"  ⚠ {msg}", flush=True)


# ── Clerk helpers ─────────────────────────────────────────────────────────────

def clerk_api(method, path, body=None):
    url = f"https://api.clerk.com/v1{path}"
    data = json.dumps(body).encode() if body else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", f"Bearer {CLERK_SECRET}")
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", "amux-e2e-trial/1.0")
    try:
        resp = urllib.request.urlopen(req, timeout=15)
        return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"Clerk API {method} {path} → {e.code}: {e.read().decode()}")


def _user_emails(u):
    return {a.get("email_address", "").strip().lower()
            for a in (u.get("email_addresses") or [])}


def clerk_find_user(email):
    """Look up a user by email. Must use ?email_address= — Clerk silently
    ignores ?email_address[]= and returns every user, which previously caused
    real accounts to be deleted. Results are re-verified before being returned."""
    data = clerk_api("GET", f"/users?email_address={urllib.parse.quote(email)}&limit=5")
    if not isinstance(data, list):
        return None
    for u in data:
        if email.strip().lower() in _user_emails(u):
            return u
    return None


def clerk_delete_user_checked(user_id, expect_email):
    """Delete only after confirming the account carries expect_email."""
    u = clerk_api("GET", f"/users/{user_id}")
    if expect_email.strip().lower() not in _user_emails(u):
        raise RuntimeError(f"REFUSING to delete {user_id}: not {expect_email}")
    return clerk_api("DELETE", f"/users/{user_id}")


def clerk_create_user(email, password):
    for attempt in range(3):
        try:
            return clerk_api("POST", "/users", {
                "email_address": [email],
                "password": password,
                "skip_password_checks": True,
                "first_name": "E2E",
                "last_name": "Trial",
            })
        except RuntimeError as e:
            if "form_identifier_exists" in str(e) and attempt < 2:
                stale = clerk_find_user(email)
                if stale:
                    clerk_delete_user_checked(stale["id"], email)
                time.sleep(2)
            else:
                raise


# ── Cookie / HTTP helpers ─────────────────────────────────────────────────────

def make_cookie(user_id):
    ts = int(time.time())
    payload = f"{user_id}|{ts}"
    sig = hmac.new(COOKIE_SECRET.encode(), payload.encode(), hashlib.sha256).hexdigest()
    return f"{payload}|{sig}"


_ssl_ctx = __import__("ssl").create_default_context()
_ssl_ctx.check_hostname = False
_ssl_ctx.verify_mode = __import__("ssl").CERT_NONE


class _NoRedirect(urllib.request.HTTPSHandler):
    def __init__(self):
        super().__init__(context=_ssl_ctx)

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None

    def http_error_302(self, req, fp, code, msg, headers):
        return fp

    http_error_301 = http_error_303 = http_error_307 = http_error_302


def gw(method, path, body=None, cookies=None, e2e=False, accept="application/json", timeout=30):
    """cookies: dict of cookie name → value. e2e=True adds X-E2E-Secret."""
    url = f"{GATEWAY}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    if cookies:
        req.add_header("Cookie", "; ".join(f"{k}={v}" for k, v in cookies.items()))
    if e2e:
        req.add_header("X-E2E-Secret", COOKIE_SECRET)
    if accept:
        req.add_header("Accept", accept)
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", "amux-e2e-trial/1.0")
    try:
        opener = urllib.request.build_opener(_NoRedirect)
        resp = opener.open(req, timeout=timeout)
        return resp.status, resp.read().decode(), dict(resp.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(), dict(e.headers)
    except urllib.error.URLError as e:
        return 0, str(e), {}


def wait_healthy(cookies, max_wait=240):
    start = time.time()
    while time.time() - start < max_wait:
        code, _, _ = gw("GET", "/api/sessions", cookies=cookies)
        if code == 200:
            log(f"Container ready after {int(time.time() - start)}s")
            return True
        time.sleep(5)
    return False


def main():
    if not CLERK_SECRET or not COOKIE_SECRET:
        print("FATAL: CLERK_SECRET_KEY / COOKIE_SECRET not set")
        sys.exit(1)

    print("═══ amux cloud E2E trial-provisioning test ═══")
    print(f"    gateway: {GATEWAY}")
    print(f"    time:    {time.strftime('%Y-%m-%d %H:%M:%S UTC', time.gmtime())}")

    admin_uid = "user_e2e_trial_admin"
    admin_cookies = {"amux_session": make_cookie(admin_uid)}
    org_id = None
    trial_user = None
    imposter_uid = "user_e2e_trial_imposter"
    cleanup_ids = [admin_uid, imposter_uid]

    try:
        # ── 1. Provision ──
        step("Provision a trial workspace by email (X-E2E-Secret admin)")
        code, body, _ = gw("POST", "/api/gateway/admin/provision",
                           body={"email": TRIAL_EMAIL, "trial_days": 7,
                                 "budget_usd": 5.0, "notify": False},
                           cookies=admin_cookies, e2e=True)
        if code != 201:
            fail(f"provision returned {code}: {body[:300]}")
            return
        prov = json.loads(body)
        org_id = prov["org_id"]
        cleanup_ids.append(org_id)
        ok(f"Provisioned {org_id} (port {prov['port']})")
        if prov["budget_usd"] == 5.0 and prov["trial_ends_at"] > time.time():
            ok(f"Budget ${prov['budget_usd']} + trial expiry set")
        else:
            fail(f"Bad budget/trial fields: {prov}")
        if prov.get("api_key_provisioned"):
            ok("Our API key pre-provisioned into the org")
        else:
            warn("No API key provisioned (gateway has no ANTHROPIC_API_KEY?)")
        invite_token = prov["invite_token"]

        # ── 2. Admin orgs list shows it ──
        step("Admin org list includes the new org with budget state")
        code, body, _ = gw("GET", "/api/gateway/admin/orgs", cookies=admin_cookies, e2e=True)
        orgs = {o["id"]: o for o in json.loads(body).get("orgs", [])} if code == 200 else {}
        if org_id in orgs and orgs[org_id]["budget_usd"] == 5.0:
            ok("Org listed with budget_usd=5.0")
        else:
            fail(f"admin/orgs missing org or budget (code {code})")

        # ── 3. Email binding: wrong account cannot accept ──
        step("Invite is email-bound — wrong account rejected")
        imposter_cookies = {"amux_session": make_cookie(imposter_uid)}
        code, body, _ = gw("POST", f"/api/gateway/invite/{invite_token}/accept",
                           cookies=imposter_cookies)
        if code == 403:
            ok("Imposter (different email) got 403")
        else:
            fail(f"Imposter accept returned {code}, want 403: {body[:200]}")

        # ── 4. Invited user accepts ──
        step("Invited user signs up and accepts")
        trial_user = clerk_create_user(TRIAL_EMAIL, TRIAL_PASSWORD)
        cleanup_ids.append(trial_user["id"])
        user_cookies = {"amux_session": make_cookie(trial_user["id"])}
        # Prime the user row (gateway fetches the email from Clerk on first request)
        gw("GET", "/api/stripe/status", cookies=user_cookies)
        code, body, hdrs = gw("POST", f"/api/gateway/invite/{invite_token}/accept",
                              cookies=user_cookies)
        # urllib follows the 302 (same as the smoke test's logout step), so assert
        # the outcome instead: the user must now be a member of the org.
        if code not in (200, 302):
            fail(f"Accept returned {code}: {body[:200]}")
        else:
            code, body, _ = gw("GET", "/api/gateway/admin/orgs", cookies=admin_cookies, e2e=True)
            orgs = {o["id"]: o for o in json.loads(body).get("orgs", [])} if code == 200 else {}
            members = [m["user_id"] for m in orgs.get(org_id, {}).get("members", [])]
            if trial_user["id"] in members:
                ok("Accept succeeded — invited user is a member of the provisioned org")
            else:
                fail(f"Invited user not in org members: {members}")
        user_cookies["amux_org"] = org_id

        # ── 5. Container starts; user lands in the org workspace ──
        step("Org container boots for the invited user")
        if wait_healthy(user_cookies):
            ok("Org container healthy — user is in the provisioned workspace")
        else:
            fail("Org container did not become healthy")
            return

        # ── 6. God mode: admin (non-member) enters the same workspace ──
        step("God mode — admin enters the workspace without membership")
        god_cookies = {"amux_session": make_cookie(imposter_uid), "amux_org": org_id}
        # First hit may 503 while the org container wakes; poll like a browser would
        deadline = time.time() + 120
        code = 0
        while time.time() < deadline:
            code, body, _ = gw("GET", "/api/sessions", cookies=god_cookies, e2e=True)
            if code == 200:
                break
            time.sleep(5)
        if code == 200:
            ok("Admin (e2e tier) reached the org container via amux_org cookie")
        else:
            fail(f"God-mode access returned {code}")

        # ── 7. Budget enforcement ──
        step("Budget enforcement — cap at $0 and enforce")
        code, body, _ = gw("PATCH", f"/api/gateway/admin/orgs/{org_id}",
                           body={"budget_usd": 0}, cookies=admin_cookies, e2e=True)
        if code == 200:
            ok("Budget lowered to $0")
        else:
            fail(f"Budget PATCH returned {code}: {body[:200]}")
        code, body, _ = gw("POST", f"/api/gateway/admin/orgs/{org_id}/refresh-spend",
                           cookies=admin_cookies, e2e=True, timeout=60)
        d = json.loads(body) if code == 200 else {}
        if code == 200 and d.get("enforced"):
            ok(f"Spend refreshed (${d.get('spend_usd', 0):.2f}) and enforcement ran")
        else:
            fail(f"refresh-spend: code {code}, body {body[:200]}")
        code, body, _ = gw("GET", "/api/sessions", cookies=user_cookies)
        d = json.loads(body) if body.startswith("{") else {}
        if code == 402 and d.get("error") == "budget_exceeded":
            ok("API gated with 402 budget_exceeded for the trial user")
        else:
            fail(f"Expected 402 budget_exceeded, got {code}: {body[:200]}")
        code, body, _ = gw("GET", "/", cookies=user_cookies, accept="text/html")
        # 'agent usage this trial' only appears on the gateway's budget page —
        # the dashboard itself contains modal copy, so match a page-unique string
        if code == 200 and "agent usage this trial" in body:
            ok("HTML request served the budget upgrade page")
        else:
            fail(f"Budget upgrade page not served (code {code})")

        # ── 8. Admin bypasses the budget gate ──
        step("God mode bypasses the budget gate")
        code, _, _ = gw("GET", "/api/sessions", cookies=god_cookies, e2e=True)
        if code == 200:
            ok("Admin still has full access to the gated workspace")
        else:
            fail(f"Admin gated too: {code}")

        # ── 9. Pro upgrade lifts the gate ──
        step("Plan=pro lifts the budget gate")
        gw("PATCH", f"/api/gateway/admin/orgs/{org_id}",
           body={"plan": "pro"}, cookies=admin_cookies, e2e=True)
        code, _, _ = gw("GET", "/api/sessions", cookies=user_cookies)
        if code == 200:
            ok("Pro plan restored access")
        else:
            fail(f"Pro plan did not lift gate: {code}")

    finally:
        # ── Cleanup ──
        step("Cleanup — org container, DB rows, Clerk user")
        for uid in cleanup_ids:
            code, body, _ = gw("DELETE", f"/api/gateway/admin/cleanup/{uid}",
                               cookies=admin_cookies, e2e=True, timeout=90)
            if code == 200:
                log(f"cleaned {uid}")
            else:
                warn(f"cleanup {uid} returned {code}")
        if trial_user:
            try:
                clerk_delete_user_checked(trial_user["id"], TRIAL_EMAIL)
                ok(f"Clerk user {trial_user['id']} deleted")
            except Exception as e:
                warn(f"Clerk user delete failed: {e}")

    print("\n" + "═" * 50)
    print(f"  PASS: {PASS}  FAIL: {FAIL}  WARN: {len(WARNINGS)}")
    for w in WARNINGS:
        print(f"  ⚠ {w}")
    print(f"  RESULT: {'PASSED' if FAIL == 0 else 'FAILED'}")
    sys.exit(0 if FAIL == 0 else 1)


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--gateway", default=GATEWAY)
    args = p.parse_args()
    GATEWAY = args.gateway
    main()
