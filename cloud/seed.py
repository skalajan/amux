#!/usr/bin/env python3
"""
amux cloud workspace seeder + verifier.

Provisions a trial workspace and populates it from a declarative PLAN: context
docs, sessions, amux schedules, board items. Optionally runs each session once
and verifies its output, so a prospect environment can be built and proven
before anyone is invited into it.

Two intended modes, both driven by the same plan format:

  automatic      an agent researches a company domain, emits a plan JSON, and
                 this applies it. Research is agentic; applying is deterministic.
                 See --emit-template for the shape to fill in.

  semi-automatic a human describes the sessions and supplies files; the plan is
                 hand-authored (or edited from a template) and applied here.

Usage:
  python3 cloud/seed.py --plan plans/capital-express.json            # provision + seed
  python3 cloud/seed.py --plan p.json --org org_abc123               # seed existing org
  python3 cloud/seed.py --plan p.json --run --verify                 # + run once + check
  python3 cloud/seed.py --emit-template > plans/new.json
  python3 cloud/seed.py --plan p.json --teardown                     # remove the org
  python3 cloud/seed.py --plan p.json --org o --prune-duplicate-schedules [--apply]

Applying a plan is idempotent for sessions and schedules: re-running reconciles
what is already there instead of adding another copy. Board items and columns do
NOT converge yet (AC-141) — until they do, use --prune-duplicate-schedules for
schedule maintenance rather than re-running a full seed to clean up.

Env:
  CLERK_SECRET_KEY   (only needed for --teardown of Clerk users)
  COOKIE_SECRET      gateway HMAC secret; doubles as the X-E2E-Secret admin tier
  E2E_GATEWAY        default https://cloud.amux.io
  ADMIN_USER_ID      Clerk user id to act as (defaults to the amux owner)
"""
import argparse, hashlib, hmac, json, os, ssl, sys, time, urllib.error, urllib.parse, urllib.request, uuid

GATEWAY = os.environ.get("E2E_GATEWAY", "https://cloud.amux.io")
COOKIE_SECRET = os.environ.get("COOKIE_SECRET", "")
ADMIN_USER_ID = os.environ.get("ADMIN_USER_ID", "user_3AP4n5hreSZdTsJbhIt22Xv6LDh")

_ctx = ssl.create_default_context()
_ctx.check_hostname = False
_ctx.verify_mode = ssl.CERT_NONE

PASS = FAIL = 0
WARN = []


def ok(m):
    global PASS
    PASS += 1
    print(f"  \033[32m✓\033[0m {m}", flush=True)


def bad(m):
    global FAIL
    FAIL += 1
    print(f"  \033[31m✗\033[0m {m}", flush=True)


def warn(m):
    WARN.append(m)
    print(f"  \033[33m⚠\033[0m {m}", flush=True)


def log(m):
    print(f"  {m}", flush=True)


def step(m):
    print(f"\n\033[1m→ {m}\033[0m", flush=True)


# ── gateway calls (god mode via X-E2E-Secret) ─────────────────────────────────

def _cookie(org=None):
    p = f"{ADMIN_USER_ID}|{int(time.time())}"
    sig = hmac.new(COOKIE_SECRET.encode(), p.encode(), hashlib.sha256).hexdigest()
    c = f"amux_session={p}|{sig}"
    return c + (f"; amux_org={org}" if org else "")


def gw(method, path, body=None, org=None, raw=None, ctype="application/json", timeout=90):
    url = f"{GATEWAY}{path}"
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    headers = {"Cookie": _cookie(org), "X-E2E-Secret": COOKIE_SECRET,
               "Accept": "application/json", "Content-Type": ctype,
               "User-Agent": "amux-seed/1.0"}
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        r = urllib.request.urlopen(req, timeout=timeout, context=_ctx)
        return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()
    except urllib.error.URLError as e:
        return 0, str(e)


def gw_json(method, path, **kw):
    code, body = gw(method, path, **kw)
    try:
        return code, json.loads(body)
    except Exception:
        return code, body


def wait_ready(org, max_wait=300):
    """Block until the org's container answers. Cold boot is ~30s."""
    start = time.time()
    while time.time() - start < max_wait:
        code, _ = gw("GET", "/api/sessions", org=org)
        if code == 200:
            return int(time.time() - start)
        if code == 402:
            return -1
        time.sleep(6)
    return None


# ── plan steps ────────────────────────────────────────────────────────────────

def provision(plan):
    o = plan.get("org", {})
    body = {"email": o["email"], "trial_days": o.get("trial_days", 7),
            "budget_usd": o.get("budget_usd", 5),
            "name": o.get("name") or f"{o['email']} (trial)",
            "notify": bool(o.get("notify", False))}
    code, d = gw_json("POST", "/api/gateway/admin/provision", body=body)
    if code != 201:
        bad(f"provision failed: {code} {str(d)[:200]}")
        return None
    ok(f"provisioned {d['org_id']} for {d['email']} "
       f"(trial {o.get('trial_days',7)}d, ${d['budget_usd']}, auth={d.get('claude_auth')})")
    if d.get("claude_auth") == "none":
        warn("workspace has NO Claude auth — set ANTHROPIC_API_KEY on the gateway "
             "or supply api_key in the plan, or sessions will not produce output")
    if not d.get("clerk_invitation_sent") and body["notify"]:
        warn(f"invite email not sent: {d.get('clerk_detail','')[:120]}")
    return d


def upload_docs(org, docs):
    """Write context files into the workspace before anyone signs in.

    Overwrites: a plan declares that a path should hold these bytes. The upload
    endpoint's default is never-clobber (right for a human dragging a file in,
    wrong here) and it suffixes instead, so before this a re-seed left
    compliance.md, compliance_1.md, compliance_2.md and compliance_3.md side by
    side in a workspace a prospect was about to be shown.
    """
    for doc in docs:
        path = doc["path"]
        directory, _, fname = path.rpartition("/")
        directory = directory or "/root"
        # content_file is read as BYTES so real artifacts (a customer's .xlsx)
        # survive the upload intact; inline content stays text.
        if "content_file" in doc:
            blob = open(doc["content_file"], "rb").read()
            ctype_part = "application/octet-stream"
        else:
            blob = doc.get("content", "").encode()
            ctype_part = "text/plain"
        gw("POST", "/api/fs/mkdir", body={"path": directory}, org=org)
        b = "----amux" + uuid.uuid4().hex
        payload = b"".join([
            f"--{b}\r\nContent-Disposition: form-data; name=\"dir\"\r\n\r\n{directory}\r\n".encode(),
            f"--{b}\r\nContent-Disposition: form-data; name=\"overwrite\"\r\n\r\n1\r\n".encode(),
            (f"--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{fname}\"\r\n"
             f"Content-Type: {ctype_part}\r\n\r\n").encode() + blob + b"\r\n",
            f"--{b}--\r\n".encode(),
        ])
        content = blob
        code, resp = gw("POST", "/api/fs/upload", org=org, raw=payload,
                        ctype=f"multipart/form-data; boundary={b}")
        if code == 200:
            ok(f"doc {path} ({len(content)} bytes)")
        else:
            bad(f"doc {path} failed: {code} {resp[:140]}")


def session_is_live(org, name):
    """Is an AGENT actually running in this session? Returns (live, why).

    The one check that matters and the one that was missing. POST /api/sessions
    returns 200 for the DB record, which is not the same fact as "a session is
    running": a container recreated by a cloud image deploy keeps the record
    (the DB lives on a volume) and loses every tmux session with it. Three times
    a workspace was reported as seeded while its owner looked at an empty
    dashboard, because the seeder believed its own 200.

    Uses the server's `running` flag, which is tmux-backed: it checks the tmux
    session exists, that the pane is not sitting at a shell prompt, and
    cross-checks that the shell actually has a child process. Do NOT substitute
    "peek returned some lines" for this — a STOPPED session keeps its tmux and
    its scrollback, so peek happily returns 17 lines of a dead session. That
    check passes exactly when it should fail, which is the failure mode this
    function exists to prevent.
    """
    code, body = gw("GET", "/api/sessions", org=org)
    if code != 200:
        return False, f"session list unreadable (HTTP {code})"
    try:
        rows = json.loads(body)
    except Exception:
        return False, "session list unparsable"
    for s in rows if isinstance(rows, list) else []:
        if s.get("name") == name:
            if s.get("running"):
                return True, f"running, status={s.get('status') or 'starting'}"
            return False, f"not running (preview: {str(s.get('preview') or '')[:40]!r})"
    return False, "no such session in this workspace"


def ensure_sessions_live(org, sessions, settle=15):
    """Start any session that has no live pane, then PROVE it came up.

    Called after create_sessions because creation and aliveness are different
    facts, and only the second one is what a prospect sees.
    """
    started = []
    for s in sessions:
        name = s["name"]
        live, why = session_is_live(org, name)
        if live:
            ok(f"session '{name}' is live ({why})")
            continue
        log(f"session '{name}' is NOT running — {why}; starting it")
        code, _ = gw("POST", f"/api/sessions/{name}/start", org=org, timeout=120)
        if code not in (200, 201, 202):
            bad(f"session '{name}' would not start: HTTP {code}")
            continue
        started.append(name)
    if started:
        log(f"started {len(started)} session(s); letting them come up ({settle}s)…")
        time.sleep(settle)
        for name in started:
            live, why = session_is_live(org, name)
            (ok if live else bad)(
                f"session '{name}' {'started and running' if live else 'START DID NOT TAKE'} ({why})")
    return started


def create_sessions(org, sessions):
    made = []
    for s in sessions:
        body = {"name": s["name"], "desc": s.get("desc", ""),
                "provider": s.get("provider", "claude")}
        if s.get("dir"):
            body["dir"] = s["dir"]
        # Tags become the session-list filter chips — the "tabs" across categories
        # of work, so a fleet of sessions stays navigable as it grows.
        if s.get("tags"):
            body["tags"] = s["tags"]
        code, resp = gw("POST", "/api/sessions", body=body, org=org, timeout=120)
        if code in (200, 201):
            ok(f"session '{s['name']}' ({body['provider']})")
            made.append(s)
        elif code == 409:
            warn(f"session '{s['name']}' already existed")
            made.append(s)
        else:
            bad(f"session '{s['name']}' failed: {code} {resp[:140]}")
    return made


DEFAULT_SCAFFOLD = ("hello-world", "amux-helper")


def remove_scaffold(org, seeded):
    """Drop the first-run scaffold sessions once real ones exist.

    Every cloud container gets a 'hello-world' session at /root/dev on boot —
    sensible for a self-serve signup, wrong for a curated prospect workspace
    where it is the first thing they see and belongs to nothing in the plan.
    Only ever removes the known scaffold names, and only when the plan actually
    seeded sessions of its own.
    """
    if not seeded:
        return
    code, body = gw("GET", "/api/sessions", org=org)
    if code != 200:
        return
    try:
        existing = {s["name"] for s in json.loads(body)}
    except Exception:
        return
    for name in DEFAULT_SCAFFOLD:
        if name in existing:
            # ARCHIVE, not delete: deleting a session is guarded to human
            # dashboard actions on purpose, and archiving is reversible while
            # still keeping the workspace clean for a prospect.
            c, _ = gw("POST", f"/api/sessions/{name}/archive", org=org, timeout=120)
            if c in (200, 202, 204):
                ok(f"archived scaffold session '{name}'")
            elif c == 403:
                # Archiving/deleting a session is deliberately restricted to a
                # human acting in the dashboard (guard: _session_destructive_allowed).
                # Do NOT weaken that for cosmetics — surface it instead.
                warn(f"'{name}' still visible — archiving needs a human click in the "
                     f"dashboard (agents are blocked from removing sessions by design)")
            else:
                warn(f"could not archive '{name}' ({c})")


def existing_schedules(org):
    """Index the org's live schedules by (session, title).

    Returns None if the workspace could not be read — the caller must treat that
    as "unknown", not as "empty", or it will re-create everything.
    """
    code, d = gw_json("GET", "/api/schedules", org=org)
    if code != 200 or not isinstance(d, list):
        return None
    idx = {}
    for s in d:
        idx.setdefault((s.get("session") or "", s.get("title") or ""), []).append(s)
    return idx


def create_schedules(org, sessions):
    """Attach amux schedules, idempotently.

    A plan DECLARES what should exist, so applying it twice must converge, not
    accumulate. This used to POST unconditionally: the Wexus prospect workspace
    ended up running 14 schedules for a 7-schedule plan — exactly 2x, one extra
    copy per re-seed — and because four of them recur every 15m, the duplicates
    doubled the workspace's burn rate until the $25 trial budget was gone
    (AC-137). Matching is on (session, title), the pair a plan author controls.

    Schedules stay disabled unless the plan says otherwise, so a seeded demo
    cannot start spending on its own. Enforcing that took a server fix too: POST
    /api/schedules ignored `enabled` and created everything running (AC-139).
    """
    existing = existing_schedules(org)
    if existing is None:
        # Fail closed. Creating blind is precisely what produced the duplicates.
        bad("could not list existing schedules — refusing to create any "
            "(a blind create is how the 2x duplication happened)")
        return []
    out = []
    for s in sessions:
        for sch in s.get("schedules", []):
            want = {"command": sch["command"],
                    "schedule_expr": sch.get("expr", "daily at 9am"),
                    "enabled": 1 if sch.get("enabled") else 0}
            live = existing.get((s["name"], sch["title"]), [])
            if live:
                cur = live[0]
                if len(live) > 1:
                    # Report, do not auto-delete: extra copies are workspace data
                    # and removing them is the operator's call, not the seeder's.
                    warn(f"'{sch['title']}' → {s['name']} has {len(live)} copies "
                         f"({', '.join(x['id'] for x in live)}); reconciling {cur['id']}, "
                         f"remove the rest with --prune-duplicate-schedules")
                drift = {k: v for k, v in want.items() if cur.get(k) != v}
                if drift:
                    code, d = gw_json("PATCH", f"/api/schedules/{cur['id']}",
                                      body={**drift, "by": "seed.py"}, org=org)
                    if code == 200:
                        ok(f"schedule {cur['id']} '{sch['title']}' reconciled to plan "
                           f"({', '.join(f'{k}={v!r}' for k, v in drift.items())})")
                    else:
                        bad(f"schedule {cur['id']} '{sch['title']}' PATCH failed: "
                            f"{code} {str(d)[:140]}")
                else:
                    ok(f"schedule {cur['id']} '{sch['title']}' → {s['name']} "
                       f"already matches the plan (skipped)")
                out.append((cur["id"], s["name"], sch["title"]))
                continue
            body = {"title": sch["title"], "session": s["name"], **want}
            code, d = gw_json("POST", "/api/schedules", body=body, org=org)
            if code in (200, 201) and isinstance(d, dict) and d.get("id"):
                out.append((d["id"], s["name"], sch["title"]))
                existing.setdefault((s["name"], sch["title"]), []).append(d)
                # The server used to force enabled=1 regardless of the body. If
                # this workspace still runs an old build, say so here rather than
                # letting a "disabled" demo quietly spend.
                if not body["enabled"] and d.get("enabled"):
                    warn(f"{d['id']} '{sch['title']}' was created ENABLED though the "
                         f"plan disables it — this workspace predates the POST "
                         f"enabled fix (AC-139); update its image or disable by hand")
                ok(f"schedule {d['id']} '{sch['title']}' → {s['name']} "
                   f"[{body['schedule_expr']}]{'' if body['enabled'] else ' (disabled)'}")
            else:
                bad(f"schedule '{sch['title']}' failed: {code} {str(d)[:140]}")
    return out


def prune_duplicate_schedules(org, sessions, apply=False):
    """Report (and optionally remove) extra copies of a plan's schedules.

    Keeps the OLDEST id per (session, title) — it carries the run history — and
    only ever touches pairs the plan itself declares, so nothing a human added
    by hand is in scope. Deletion is the amux soft-delete, and it is audited.
    """
    live = existing_schedules(org)
    if live is None:
        bad("could not list schedules — nothing pruned")
        return []
    planned = {(s["name"], sch["title"])
               for s in sessions for sch in s.get("schedules", [])}
    removed = []
    for key in sorted(planned):
        copies = live.get(key, [])
        if len(copies) < 2:
            continue
        copies = sorted(copies, key=lambda x: (x.get("created") or 0, x["id"]))
        keep, extra = copies[0], copies[1:]
        for x in extra:
            if not apply:
                warn(f"duplicate {x['id']} '{key[1]}' → {key[0]} "
                     f"(would keep {keep['id']}, runs={keep.get('run_count')})")
                continue
            code, _ = gw("DELETE", f"/api/schedules/{x['id']}?by=seed.py", org=org)
            if code == 200:
                removed.append(x["id"])
                ok(f"removed duplicate {x['id']} '{key[1]}' → {key[0]} "
                   f"(kept {keep['id']}, runs={keep.get('run_count')})")
            else:
                bad(f"could not remove {x['id']}: {code}")
    if not removed and apply:
        ok("no duplicate schedules to remove")
    return removed


def prune_duplicate_board(org, plan, apply=False):
    """Report (and optionally remove) extra copies of the plan's board items and
    columns. Only titles/labels the plan itself declares are in scope, so a card
    or column a person added by hand is never touched.

    Cards in a duplicate column are MOVED to the kept column before it goes:
    deleting a column server-side reparents its cards to 'todo', which would
    quietly undo work the prospect had already sorted.
    """
    live_items, live_cols = existing_board(org), existing_columns(org)
    if live_items is None or live_cols is None:
        bad("could not list board — nothing pruned")
        return
    for title in sorted({i["title"] for i in plan.get("board", [])}):
        copies = sorted(live_items.get(title.strip(), []),
                        key=lambda x: (x.get("created") or 0, x.get("id") or ""))
        for x in copies[1:]:
            if not apply:
                warn(f"duplicate board item {x.get('id')} '{title[:44]}' "
                     f"(would keep {copies[0].get('id')})")
                continue
            code, _ = gw("DELETE", f"/api/board/{x['id']}", org=org)
            (ok if code in (200, 204) else bad)(
                f"removed duplicate board item {x['id']} '{title[:44]}' "
                f"(kept {copies[0]['id']})")
    # Re-read: the item pass above just deleted cards, and most of them were the
    # very cards sitting in the duplicate columns. Reusing the pre-delete
    # snapshot here would "move" rows that no longer exist and report a card
    # count nobody could reconcile with the board.
    if apply:
        live_items = existing_board(org) or live_items
    for label in plan.get("board_columns", []):
        cols = live_cols.get(label.strip().lower(), [])
        if len(cols) < 2:
            continue
        cols = sorted(cols, key=lambda c: (c.get("position") or 0, c.get("id") or ""))
        keep, extra = cols[0], cols[1:]
        for c in extra:
            stranded = [i for group in live_items.values() for i in group
                        if i.get("status") == c["id"]]
            if not apply:
                warn(f"duplicate column '{label}' {c['id']} "
                     f"(would move {len(stranded)} card(s) to {keep['id']}, then delete)")
                continue
            moved = 0
            for i in stranded:
                mc, _ = gw("PATCH", f"/api/board/{i['id']}",
                           body={"status": keep["id"]}, org=org)
                moved += 1 if mc == 200 else 0
            code, _ = gw("DELETE", f"/api/board/statuses/{c['id']}", org=org)
            (ok if code in (200, 204) else bad)(
                f"removed duplicate column '{label}' {c['id']} "
                f"(moved {moved}/{len(stranded)} card(s) to {keep['id']})")


def existing_columns(org):
    """Live board columns grouped by lowercased label. None if unreadable."""
    code, d = gw_json("GET", "/api/board/statuses", org=org)
    rows = d if isinstance(d, list) else (d or {}).get("statuses")
    if code != 200 or not isinstance(rows, list):
        return None
    idx = {}
    for s in rows:
        idx.setdefault((s.get("label") or "").strip().lower(), []).append(s)
    return idx


def existing_board(org):
    """Live board items grouped by title. None if unreadable."""
    code, d = gw_json("GET", "/api/board", org=org)
    rows = d if isinstance(d, list) else (d or {}).get("items")
    if code != 200 or not isinstance(rows, list):
        return None
    idx = {}
    for i in rows:
        idx.setdefault((i.get("title") or "").strip(), []).append(i)
    return idx


def create_board_columns(org, labels):
    """Add a board column per category of work, so the kanban mirrors how the
    prospect actually thinks about their pipeline rather than generic todo/doing.
    Returns {label: status_id} for placing items into them.

    Reuses a column that already carries the label. POST does not dedupe — it
    mints a fresh id with a numeric suffix, so re-seeding used to leave
    Underwriting, Underwriting-2, Underwriting-3 side by side in the prospect's
    kanban (Jacob's workspace, seeded four times, AC-141).
    """
    live = existing_columns(org)
    if live is None:
        bad("could not list board columns — refusing to create (would duplicate)")
        return {}
    mapping = {}
    for label in labels:
        have = live.get(label.strip().lower(), [])
        if have:
            keep = have[0]
            mapping[label] = keep["id"]
            extra = f" ({len(have)} columns share this label)" if len(have) > 1 else ""
            ok(f"board column '{label}' → {keep['id']} (exists, reused){extra}")
            continue
        code, d = gw_json("POST", "/api/board/statuses", body={"label": label}, org=org)
        if code in (200, 201) and isinstance(d, dict) and d.get("id"):
            mapping[label] = d["id"]
            live.setdefault(label.strip().lower(), []).append(d)
            ok(f"board column '{label}' → {d['id']}")
        else:
            bad(f"board column '{label}' failed: {code} {str(d)[:120]}")
    return mapping


def create_board(org, items, columns=None):
    """Seed the plan's board items, once.

    An item already present under the same title is left ALONE, including its
    status: a board card is something a person moves, and re-applying the plan
    must not drag a card the prospect advanced back to its seeded column.
    """
    columns = columns or {}
    live = existing_board(org)
    if live is None:
        bad("could not list board items — refusing to create (would duplicate)")
        return
    for it in items:
        # A plan may target a column by its human label; fall back to a raw status.
        status = columns.get(it.get("column", ""), it.get("status", "todo"))
        have = live.get(it["title"].strip(), [])
        if have:
            extra = f" ({len(have)} copies exist)" if len(have) > 1 else ""
            ok(f"board item '{it['title'][:44]}' already present, left as-is "
               f"(status={have[0].get('status')}){extra}")
            continue
        body = {"title": it["title"], "desc": it.get("desc", ""), "status": status}
        if it.get("tags"):
            body["tags"] = it["tags"]
        code, d = gw_json("POST", "/api/board", body=body, org=org)
        if code in (200, 201):
            live.setdefault(it["title"].strip(), []).append(
                d if isinstance(d, dict) else {"title": it["title"], "status": status})
            ok(f"board item '{it['title'][:44]}' → {status}")
        else:
            bad(f"board item '{it['title'][:44]}' failed: {code}")


def run_once(org, sessions, schedules):
    """Kick each session once: prefer its schedule (proves the schedule path),
    else send the seed prompt directly."""
    fired = []
    sched_by_session = {}
    for sid, sname, title in schedules:
        sched_by_session.setdefault(sname, (sid, title))
    for s in sessions:
        name = s["name"]
        if name in sched_by_session:
            sid, title = sched_by_session[name]
            code, _ = gw("POST", f"/api/schedules/{sid}/run", org=org, timeout=120)
            (ok if code in (200, 202) else bad)(f"ran schedule '{title}' for {name} ({code})")
            fired.append(name)
        elif s.get("prompt"):
            code, _ = gw("POST", f"/api/sessions/{name}/send",
                         body={"text": s["prompt"], "record_history": True},
                         org=org, timeout=120)
            (ok if code in (200, 202) else bad)(f"sent seed prompt to {name} ({code})")
            fired.append(name)
        else:
            warn(f"{name}: nothing to run (no schedule, no prompt)")
    return fired


def verify(org, sessions, settle=90):
    """Check each session actually produced output, and that any declared
    expectations appear in it. Reads `history`, not `output` — a full-screen
    prompt clears the viewport and would read as 'nothing happened'."""
    log(f"letting agents work for {settle}s…")
    time.sleep(settle)
    for s in sessions:
        name = s["name"]
        code, d = gw_json("GET", f"/api/sessions/{name}/peek?lines=400", org=org, timeout=120)
        if code != 200 or not isinstance(d, dict):
            bad(f"{name}: peek failed ({code})")
            continue
        text = d.get("history") or d.get("output") or ""
        # The terminal echoes the prompt, so any expectation drawn from the
        # prompt's own vocabulary matched itself — 'tier' and 'factor' both
        # "passed" while the agent was actually erroring. Strip the prompt and
        # the schedule commands before asserting, so a match means the AGENT
        # said it.
        haystack = text
        for echoed in [s.get("prompt", "")] + [sc.get("command", "") for sc in s.get("schedules", [])]:
            if echoed:
                haystack = haystack.replace(echoed, " ")
        if "API Error" in text or "api error" in text.lower():
            bad(f"{name}: session shows an API error — agent could not reach Claude")
            continue
        if len(haystack.strip()) < 40:
            bad(f"{name}: no agent output beyond the echoed prompt "
                f"({len(haystack)} chars) — check Claude auth in this workspace")
            continue
        ok(f"{name}: produced {len(haystack)} chars of agent output")
        for needle in s.get("expect", []):
            if needle.lower() in haystack.lower():
                ok(f"{name}: found expected '{needle}'")
            else:
                bad(f"{name}: expected '{needle}' not in agent output")
        # Prefer file evidence when the plan declares it — a written artifact is
        # far stronger than terminal text.
        for f in s.get("expect_files", []):
            code2, d2 = gw_json("GET", f"/api/fs/read?path={urllib.parse.quote(f)}", org=org)
            body2 = d2.get("content", "") if isinstance(d2, dict) else str(d2)
            (ok if code2 == 200 and len(body2) > 20 else bad)(
                f"{name}: artifact {f} ({len(body2)} bytes)" if code2 == 200
                else f"{name}: artifact {f} missing ({code2})")


def spend_report(org):
    code, d = gw_json("GET", "/api/gateway/admin/orgs")
    if code != 200 or not isinstance(d, dict):
        return
    for o in d.get("orgs", []):
        if o["id"] == org:
            log(f"spend ${o.get('spend_usd') or 0:.4f} of ${o.get('budget_usd')} budget; "
                f"trial ends {time.strftime('%Y-%m-%d', time.gmtime(o['trial_ends_at']))}")
            return


TEMPLATE = {
    "org": {"email": "prospect@example.com", "name": "Example Co (trial)",
            "trial_days": 7, "budget_usd": 5, "notify": False},
    "docs": [
        {"path": "/root/demo/context.md",
         "content": "# Context\n\nDomain facts the agents should rely on.\n"}
    ],
    "board": [{"title": "Demo: review agent output", "status": "todo"}],
    "sessions": [
        {"name": "use-case-1", "provider": "claude", "dir": "/root/demo",
         "desc": "One-line description of the use case",
         "prompt": "Read /root/demo/context.md and produce the first deliverable.",
         "expect": ["a string that must appear in the output"],
         "schedules": [{"title": "Daily run", "expr": "daily at 9am",
                        "command": "Re-run the analysis and post a summary.",
                        "enabled": False}]}
    ],
}


def seed_via_envspec(org, plan):
    """AMUX-2779 convergence: seed the DECLARATIVE env through the ONE applier
    (`POST /api/env/apply`) that export_env.py emits for and save->redeploy
    consumes, instead of the per-primitive create loops in main().

    Ownership split, deliberate:
      * /api/env/apply owns everything declarative and idempotent — worker
        CONFIG (model/tags/dir/desc, written as the SAME `<name>.env` a normal
        create writes — verified field-for-field against create_session_legacy),
        groups, files (docs), schedules, and cards, plus the first-run prompt
        (steered once, to newly-created workers only).
      * seed.py keeps ONLY the two imperative side-effects apply refuses to do:
        provisioning the org (the caller already did that) and BOOTING the tmux
        panes. apply writes a worker's .env but never starts its pane — starting
        a pane is not idempotent, so it stays here. The panes boot from the .env
        apply just wrote, and the prompt apply queued lands as each pane comes up
        (steer_enqueue delivers at the pane's first turn boundary; the applier is
        built for exactly this "provisioned separately" order).

    Returns the worker list (name-bearing dicts) so the caller can boot + verify.
    """
    import export_env  # lazy: export_env imports seed, so avoid an import cycle
    envspec, retained = export_env.plan_to_envspec(plan)
    workers = envspec.get("workers", [])

    step("Apply declarative env (/api/env/apply — groups/workers/files/schedules/cards)")
    code, resp = gw_json("POST", "/api/env/apply", body=envspec, org=org, timeout=180)
    if code != 200 or not isinstance(resp, dict):
        bad(f"env apply failed: HTTP {code} {str(resp)[:240]}")
        return []

    # Summarize the report by (kind, action) so a --via-apply run reads like the
    # create loops it replaces ("worker create x3, group create x7, file create
    # x12…"), and surface every error/write-failure loudly rather than as a 200.
    from collections import Counter
    report = resp.get("report", [])
    errors = resp.get("errors", [])
    tally = Counter((r.get("kind"), r.get("action")) for r in report)
    for (kind, action), n in sorted(tally.items(), key=lambda kv: (kv[0][0] or "", kv[0][1] or "")):
        if action == "error":
            for r in report:
                if r.get("kind") == kind and r.get("action") == "error":
                    where = r.get("name") or r.get("path") or r.get("title") or ""
                    warn(f"{kind} error: {r.get('detail', '')} [{where}]")
        elif action == "not-yet-applied":
            log(f"{kind} x{n} — phase-2 (parsed + reported, not written)")
        else:
            ok(f"{kind} {action} x{n}")
    for e in errors:
        bad(f"apply write error: {e}")

    n_prompts = sum(1 for r in report if r.get("kind") == "worker" and r.get("prompt"))
    if n_prompts:
        log(f"{n_prompts} first-run prompt(s) queued — land as each pane boots")
    return workers


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--plan")
    ap.add_argument("--org", help="seed an existing org instead of provisioning")
    ap.add_argument("--run", action="store_true", help="run each session/schedule once")
    ap.add_argument("--verify", action="store_true", help="check outputs after running")
    ap.add_argument("--settle", type=int, default=90, help="seconds to let agents work")
    ap.add_argument("--teardown", action="store_true", help="delete the org afterwards")
    ap.add_argument("--prune-duplicates", action="store_true",
                    help="maintenance mode: report extra copies of everything the "
                         "plan declares — schedules, board items, columns — and "
                         "exit (add --apply to remove them)")
    ap.add_argument("--prune-duplicate-schedules", action="store_true",
                    help="as --prune-duplicates but schedules only")
    ap.add_argument("--reconcile-schedules", action="store_true",
                    help="maintenance mode: bring schedules in line with the plan "
                         "(enabled/command/expr) and exit, touching nothing else")
    ap.add_argument("--apply", action="store_true",
                    help="with a --prune-* mode, actually delete")
    ap.add_argument("--via-apply", action="store_true",
                    help="AMUX-2779 converged path: seed the declarative env "
                         "(groups/workers/files/schedules/cards/prompts) through the "
                         "single /api/env/apply applier — the SAME path export_env.py "
                         "emits for and save->redeploy consumes — instead of the "
                         "per-primitive create loops. Org provisioning and pane-booting "
                         "stay seed-side. OPT-IN while it is proven against a live "
                         "provision; the default flow is untouched.")
    ap.add_argument("--emit-template", action="store_true")
    a = ap.parse_args()

    if a.emit_template:
        print(json.dumps(TEMPLATE, indent=2))
        return 0
    if not a.plan:
        ap.error("--plan is required (or --emit-template)")
    if not COOKIE_SECRET:
        print("FATAL: COOKIE_SECRET not set")
        return 1

    # Accept YAML or JSON. The convergence (AMUX-2779) is toward ONE format —
    # the EnvSpec YAML that /api/env/apply reads and export_env.py emits — so a
    # plan authored or exported as .yaml loads here without a separate converter.
    # JSON still loads unchanged (superset), so existing plans/*.json keep working.
    if a.plan.endswith((".yaml", ".yml")):
        import yaml
        plan = yaml.safe_load(open(a.plan))
    else:
        plan = json.load(open(a.plan))
    print(f"═══ amux workspace seed: {os.path.basename(a.plan)} ═══")
    print(f"    gateway: {GATEWAY}")

    org = a.org
    if not org:
        step("Provision workspace")
        d = provision(plan)
        if not d:
            return 1
        org = d["org_id"]
        print(f"\n    org_id:     {org}")
        print(f"    invite_url: {d['invite_url']}")

    step("Boot container")
    el = wait_ready(org)
    if el is None:
        bad("container never became ready")
        return 1
    if el == -1:
        bad("workspace is gated (budget or trial expired)")
        return 1
    ok(f"container ready in {el}s")

    if a.reconcile_schedules:
        # Reconcile is schedules-only: it is the one part of a plan that is a
        # pure declaration. Board cards are things a person moves, so re-applying
        # a plan must never drag them back.
        step("Reconcile schedules to plan")
        create_schedules(org, plan.get("sessions", []))
        a.prune_duplicate_schedules, a.apply = True, False  # then show the result

    if a.prune_duplicates or a.prune_duplicate_schedules:
        step(f"Prune duplicate schedules ({'APPLY' if a.apply else 'report only'})")
        prune_duplicate_schedules(org, plan.get("sessions", []), apply=a.apply)
        if a.prune_duplicates:
            step(f"Prune duplicate board items + columns "
                 f"({'APPLY' if a.apply else 'report only'})")
            prune_duplicate_board(org, plan, apply=a.apply)
        step("Resulting schedules")
        live = existing_schedules(org) or {}
        for (sess_name, title), copies in sorted(live.items()):
            for c in copies:
                log(f"{c['id']:9s} enabled={c.get('enabled')} runs={c.get('run_count')} "
                    f"| {sess_name} :: {title} [{c.get('schedule_expr')}]")
        total = sum(len(v) for v in live.values())
        planned = sum(len(s.get("schedules", [])) for s in plan.get("sessions", []))
        log(f"{total} live schedule(s); plan declares {planned}")
        if a.prune_duplicates:
            items, cols = existing_board(org) or {}, existing_columns(org) or {}
            n_items = sum(len(v) for v in items.values())
            planned_cols = [l.strip().lower() for l in plan.get("board_columns", [])]
            dup_cols = sum(len(cols.get(l, [])) - 1
                           for l in planned_cols if len(cols.get(l, [])) > 1)
            log(f"{n_items} live board item(s); plan declares "
                f"{len(plan.get('board', []))}. "
                f"{dup_cols} duplicate plan column(s) remain")
        print("\n" + "═" * 52)
        print(f"  PASS: {PASS}  FAIL: {FAIL}  WARN: {len(WARN)}")
        for w in WARN:
            print(f"  ⚠ {w}")
        print(f"  ORG: {org}")
        return 0 if FAIL == 0 else 1

    if a.via_apply:
        # AMUX-2779 converged path (opt-in): one applier for all declarative
        # content, then the two imperative side-effects apply won't do.
        if a.run:
            warn("--run with --via-apply: apply already steered each worker's "
                 "first-run prompt; --run will re-send it (double-dispatch)")
        workers = seed_via_envspec(org, plan)
        # Boot list: prefer the plan's sessions (they carry prompt/expect that
        # --verify reads for echo-stripping); fall back to the envspec worker
        # names when the input was already an EnvSpec with no `sessions`.
        sessions = plan.get("sessions") or [{"name": w["name"]} for w in workers]
        # apply wrote each .env but never starts a pane — boot them here.
        step("Ensure sessions are actually running")
        ensure_sessions_live(org, sessions)
        step("Remove first-run scaffold")
        remove_scaffold(org, sessions)
        schedules = []  # apply created them; run_once only fires plan schedules
    else:
        if plan.get("docs"):
            step("Upload context docs")
            upload_docs(org, plan["docs"])
        columns = {}
        if plan.get("board_columns"):
            step("Create work-category columns")
            columns = create_board_columns(org, plan["board_columns"])
        if plan.get("board"):
            step("Seed board")
            create_board(org, plan["board"], columns)

        step("Create sessions")
        sessions = create_sessions(org, plan.get("sessions", []))

        # Creating a session and having one RUNNING are different facts. Re-seeding
        # after a container was recreated finds every record intact and every tmux
        # session gone, and without this the seeder reports a fully seeded workspace
        # that shows nothing but the scaffold.
        step("Ensure sessions are actually running")
        ensure_sessions_live(org, sessions)

        step("Create schedules")
        schedules = create_schedules(org, sessions)

        step("Remove first-run scaffold")
        remove_scaffold(org, sessions)

    if a.run:
        step("Run once")
        run_once(org, sessions, schedules)
    if a.verify:
        step("Verify outputs")
        verify(org, sessions, settle=a.settle)

    step("Spend / trial state")
    spend_report(org)

    if a.teardown:
        step("Teardown")
        code, d = gw_json("DELETE", f"/api/gateway/admin/cleanup/{org}", timeout=120)
        (ok if code == 200 else bad)(f"cleanup {org} ({code})")

    print("\n" + "═" * 52)
    print(f"  PASS: {PASS}  FAIL: {FAIL}  WARN: {len(WARN)}")
    for w in WARN:
        print(f"  ⚠ {w}")
    print(f"  ORG: {org}")
    print(f"  RESULT: {'PASSED' if FAIL == 0 else 'FAILED'}")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
