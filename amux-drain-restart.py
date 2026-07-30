#!/usr/bin/env python3
"""amux-drain-restart — lossless fleet/session restarts for amux.

A standalone, stdlib-only sidecar (see .claude/rules/extend-via-sidecar.md).
Upstream (mixpeek/amux) has no such file, so it is conflict-immune and makes
ZERO changes to amux-server.py — it talks to the running server purely over
its localhost HTTPS API (self-signed cert -> no TLS verify) plus read-only
system introspection (`ps`, `tmux list-panes`, `pgrep`).

Why this exists: a hard-kill-based restart lost 7 sessions' conversations in
one incident (`resumed: false` — Claude Code never got the chance to persist
its rename/close dance). A GRACEFUL stop of an IDLE session is already
lossless in amux (`stop_session` does a rename dance that guarantees resume,
then sends `/exit`) — the missing piece was ensuring we only ever stop a
session once it has actually finished its in-flight turn, and that we never
call `/start` before the previous Claude process has genuinely exited.

Sequence per session: drain (wait for a STABLE idle) -> capture any unsent
composer text -> POST /stop -> wait for the real `claude` process to exit
(verified via system introspection, not the API's `status` field, which
cannot distinguish "Claude idle" from "shell after /exit") -> POST /start ->
record whether Claude resumed the prior conversation.

Sessions that never drain within --timeout-mins, or whose process survives
the graceful stop past the wait bound, are NEVER touched further (no kill) —
they are reported as stragglers for a human to look at. See
docs/drain-restart.md for full semantics.

The pure logic (drain-stability tracking, composer-snapshot extraction,
process matching, the rolling drain/restart state machine) is importable and
unit-tested (tests/test_drain_restart.py) with the HTTP and process layers
injected, so it runs with no live server and no real `ps`/`tmux`/`pgrep`.
"""
import argparse
import json
import os
import re
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field

HOME = os.path.expanduser("~")
AMUX_DIR = os.path.join(HOME, ".amux")
WRITE_TOKEN_PATH = os.path.join(AMUX_DIR, "write_token")
DEFAULT_AMUX_URL = "https://localhost:8822"

DEFAULT_TIMEOUT_MINS = 15
POLL_INTERVAL_S = 3.0          # drain-loop polling cadence
STABLE_IDLE_POLLS = 2          # consecutive idle polls required to call a session "drained"
STOP_WAIT_TIMEOUT_S = 30.0     # bound on waiting for the claude process to exit after /stop
STOP_WAIT_POLL_S = 1.0

_VALID_SESSION_NAME_RE = re.compile(r'^[a-zA-Z0-9_.\-]+$')

_ANSI_RE = re.compile(
    r'\x1b\[[0-9;?]*[a-zA-Z]|\x1b\]8;[^\x1b]*\x1b\\|\x1b\][^\x07]*\x07'
    r'|\x1b\][^\x1b]*\x1b\\|\x1b[()][A-Z0-9]|\x1b[\x20-\x2f]*[\x40-\x7e]'
)
# A composer line: '❯' followed by whitespace then non-empty un-submitted text.
_COMPOSER_RE = re.compile(r'^❯[ \t\xa0]*(\S.*)$')


class AmuxError(Exception):
    """Raised for any non-2xx amux API response or transport failure."""


# ── HTTP layer (injectable) ──────────────────────────────────────────────────
class AmuxClient:
    """Talks to the running amux-server over its localhost HTTPS API. Mirrors
    amux-telegram.py's AmuxClient: self-signed cert -> no verify, write token
    from ~/.amux/write_token sent as X-Amux-Write-Token on non-GET requests
    (GET is loose/localhost-bypassed server-side, per amux-server.py's
    _write_auth_ok / _check_auth)."""

    def __init__(self, base=DEFAULT_AMUX_URL, write_token="", opener=None):
        self.base = base.rstrip("/")
        self.write_token = write_token
        if opener is not None:
            self._opener = opener
        else:
            ctx = ssl.create_default_context()
            ctx.check_hostname = False
            ctx.verify_mode = ssl.CERT_NONE
            self._opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=ctx))

    def _call(self, method, path, params=None, body=None, timeout=20):
        url = self.base + path
        if params:
            url += "?" + urllib.parse.urlencode(params)
        headers = {}
        data = None
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if method not in ("GET", "HEAD"):
            headers["X-Amux-Write-Token"] = self.write_token
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with self._opener.open(req, timeout=timeout) as resp:
                raw = resp.read().decode("utf-8")
                return resp.status, (json.loads(raw) if raw else {})
        except urllib.error.HTTPError as e:
            try:
                payload = json.loads(e.read().decode("utf-8"))
            except Exception:
                payload = {}
            return e.code, payload
        except (urllib.error.URLError, OSError, ValueError) as e:
            raise AmuxError(f"{method} {path}: {e}")

    def list_sessions(self):
        code, body = self._call("GET", "/api/sessions", timeout=30)
        if code != 200:
            raise AmuxError(f"GET /api/sessions -> {code}")
        return body

    def peek(self, name, lines=20, live=True):
        params = {"lines": lines}
        if live:
            params["live"] = 1
        code, body = self._call(
            "GET", f"/api/sessions/{urllib.parse.quote(name)}/peek", params=params)
        if code != 200:
            raise AmuxError(f"peek {name} -> {code}")
        return body.get("live") or body.get("output") or ""

    def stop(self, name):
        code, body = self._call("POST", f"/api/sessions/{urllib.parse.quote(name)}/stop")
        if code not in (200, 202):
            raise AmuxError(f"stop {name} -> {code}: {body}")
        return body

    def start(self, name):
        code, body = self._call("POST", f"/api/sessions/{urllib.parse.quote(name)}/start")
        if code != 200:
            raise AmuxError(f"start {name} -> {code}: {body}")
        return body


# ── Process layer (injectable) ───────────────────────────────────────────────
class ProcessLayer:
    """Real, READ-ONLY system-process introspection: `ps`, `tmux list-panes`,
    `pgrep`. Never sends a signal to any process."""

    def ps_snapshot(self):
        """Return [(pid, command), ...] from `ps -axo pid,command`."""
        try:
            r = subprocess.run(["ps", "-axo", "pid,command"],
                                capture_output=True, text=True, timeout=10)
        except Exception:
            return []
        out = []
        for line in r.stdout.splitlines()[1:]:  # skip header row
            line = line.strip()
            if not line:
                continue
            parts = line.split(None, 1)
            if len(parts) != 2:
                continue
            try:
                pid = int(parts[0])
            except ValueError:
                continue
            out.append((pid, parts[1]))
        return out

    def tmux_pane_pid(self, tmux_session):
        """Return the shell PID of a tmux session's first pane, or 0."""
        try:
            r = subprocess.run(
                ["tmux", "list-panes", "-t", tmux_session, "-F", "#{pane_pid}"],
                capture_output=True, text=True, timeout=5)
            if r.returncode != 0 or not r.stdout.strip():
                return 0
            return int(r.stdout.strip().splitlines()[0])
        except Exception:
            return 0

    def pgrep_children(self, parent_pid):
        """Return direct child PIDs of `parent_pid`."""
        try:
            r = subprocess.run(["pgrep", "-P", str(parent_pid)],
                                capture_output=True, text=True, timeout=5)
            return [int(x) for x in r.stdout.split()]
        except Exception:
            return []


def _cmd_is_claude_binary(command: str) -> bool:
    """True iff `command`'s first whitespace-delimited token's basename is
    literally "claude". This is what keeps process matching from ever
    catching THIS script's own `ps` line (which starts with a python
    interpreter, never "claude"), regardless of what arguments follow."""
    command = command.strip()
    if not command:
        return False
    first = command.split(None, 1)[0]
    return os.path.basename(first) == "claude"


def _cmd_mentions_session(command: str, name: str) -> bool:
    """True iff `command` contains `--name <name>` as a whole flag value
    (word-boundary — a longer session name must not match, e.g. 'foo' must
    not match '--name foobar')."""
    return re.search(rf'--name\s+{re.escape(name)}(\s|$)', command) is not None


def find_claude_pid(process_layer, name: str) -> int:
    """Resolve the live `claude` PID for session `name`, or 0 if none.

    Primary path: the tmux pane's process tree (mirrors amux-server.py's own
    _find_claude_pid) — this is flag-agnostic, so it correctly finds a
    session's Claude process whether it was launched with `--name <name>`
    (first-ever start) or `--resume <uuid>` (every subsequent start after a
    graceful stop, which is the common case this tool creates).

    Fallback: scan the full `ps` snapshot for a line whose command starts
    with the claude binary and contains `--name <name>` — covers providers
    without a tmux pane (e.g. iTerm2) or an unresolvable pane PID.
    """
    tmux_session = f"amux-{name}"
    pane_pid = process_layer.tmux_pane_pid(tmux_session)
    if pane_pid > 1:
        snapshot = dict(process_layer.ps_snapshot())
        for cpid in process_layer.pgrep_children(pane_pid):
            if _cmd_is_claude_binary(snapshot.get(cpid, "")):
                return cpid
    for pid, cmd in process_layer.ps_snapshot():
        if _cmd_is_claude_binary(cmd) and _cmd_mentions_session(cmd, name):
            return pid
    return 0


def wait_for_claude_exit(process_layer, name: str, *, timeout_s=STOP_WAIT_TIMEOUT_S,
                          poll_interval_s=STOP_WAIT_POLL_S, sleep_fn=time.sleep,
                          clock=time.monotonic) -> bool:
    """Poll until no claude process remains for `name`, bounded by timeout_s.
    Returns True on confirmed exit, False if the process survived the bound —
    callers must NOT kill on False, only report a "stop-failed" straggler."""
    deadline = clock() + timeout_s
    while True:
        if find_claude_pid(process_layer, name) == 0:
            return True
        if clock() >= deadline:
            return False
        sleep_fn(poll_interval_s)


def extract_composer_snapshot(peek_text: str) -> str:
    """Return the last un-submitted composer line in `peek_text` — a line
    starting with '❯' + whitespace followed by non-empty text — after
    stripping ANSI escapes. '' if no such line (composer empty / not shown)."""
    if not peek_text:
        return ""
    clean = _ANSI_RE.sub("", peek_text)
    snapshot = ""
    for line in clean.splitlines():
        m = _COMPOSER_RE.match(line.strip())
        if m:
            snapshot = m.group(1).strip()
    return snapshot


# ── Drain-stability tracking ──────────────────────────────────────────────────
@dataclass
class DrainTracker:
    name: str
    stable_needed: int = STABLE_IDLE_POLLS
    consecutive_idle: int = 0
    ready: bool = False
    ready_reason: str = ""     # "idle-stable" | "dead"
    ready_at: float = None
    last_status: str = ""
    polls: int = 0

    def observe(self, status: str, now: float) -> None:
        """Feed one poll's status ('active'/'waiting'/'idle'/''). Flapping
        (active/idle alternating) never accumulates — only a STABLE run of
        `stable_needed` consecutive 'idle' polls marks the session drained.
        An empty status means claude isn't running at all (crashed or never
        started) — that's the "dead" straggler class, ready immediately since
        there is no in-flight turn to lose."""
        self.polls += 1
        self.last_status = status
        if self.ready:
            return
        if status == "":
            self.ready = True
            self.ready_reason = "dead"
            self.ready_at = now
            return
        if status == "idle":
            self.consecutive_idle += 1
        else:
            self.consecutive_idle = 0
        if self.consecutive_idle >= self.stable_needed:
            self.ready = True
            self.ready_reason = "idle-stable"
            self.ready_at = now


@dataclass
class SessionResult:
    name: str
    drained_in_s: float = None
    restarted: bool = False
    resumed: bool = None
    composer_snapshot: str = ""
    straggler: str = ""        # "" | "timeout" | "stop-failed" | "stop-request-failed: ..." | "start-request-failed: ..."
    dead: bool = False
    last_status: str = ""


def resolve_targets(sessions: list, requested: list) -> tuple:
    """Resolve CLI target args against the live session list.
    'all' (alone) = every currently-RUNNING session. Otherwise each arg must
    name an existing session (running or not — the drain loop below no-ops
    safely on an already-stopped one). Returns (targets, unknown_names)."""
    by_name = {s.get("name"): s for s in sessions}
    if requested == ["all"]:
        return sorted(n for n, s in by_name.items() if s.get("running")), []
    targets, unknown = [], []
    for n in requested:
        if n in by_name:
            targets.append(n)
        else:
            unknown.append(n)
    return targets, unknown


def build_plan(sessions: list, targets: list) -> list:
    by_name = {s.get("name"): s for s in sessions}
    lines = []
    for n in targets:
        s = by_name.get(n)
        status = (s.get("status") or "") if s else ""
        lines.append(f"  {n:<24} status={status or '(dead/stopped)'}")
    return lines


def dry_run_report(sessions: list, targets: list) -> list:
    """One-cycle, read-only classification — no polling loop, no POSTs."""
    by_name = {s.get("name"): s for s in sessions}
    lines = []
    for n in targets:
        s = by_name.get(n)
        status = (s.get("status") or "") if s else ""
        if status == "":
            verdict = "DEAD (claude not running) -> would revive immediately via stop+start"
        elif status == "idle":
            verdict = f"IDLE now -> would need {STABLE_IDLE_POLLS} stable idle polls (~{POLL_INTERVAL_S:.0f}s apart) before draining"
        else:
            verdict = "would keep waiting to drain (bounded by --timeout-mins)"
        lines.append(f"  {n:<24} status={status or '(empty)':<8} {verdict}")
    return lines


def _drain_one(name, tracker, http, process_layer, stop_wait_s, sleep_fn, clock, start_time) -> SessionResult:
    drained_in = (tracker.ready_at - start_time) if tracker.ready_at is not None else None
    dead = tracker.ready_reason == "dead"
    snapshot = ""
    try:
        snapshot = extract_composer_snapshot(http.peek(name))
    except AmuxError:
        snapshot = ""  # best-effort — a peek failure must not block the restart
    try:
        http.stop(name)
    except AmuxError as e:
        return SessionResult(name=name, drained_in_s=drained_in, composer_snapshot=snapshot,
                              dead=dead, straggler=f"stop-request-failed: {e}")
    exited = wait_for_claude_exit(process_layer, name, timeout_s=stop_wait_s,
                                   sleep_fn=sleep_fn, clock=clock)
    if not exited:
        return SessionResult(name=name, drained_in_s=drained_in, composer_snapshot=snapshot,
                              dead=dead, straggler="stop-failed")
    try:
        start_resp = http.start(name)
    except AmuxError as e:
        return SessionResult(name=name, drained_in_s=drained_in, composer_snapshot=snapshot,
                              dead=dead, straggler=f"start-request-failed: {e}")
    return SessionResult(name=name, drained_in_s=drained_in, restarted=True,
                          resumed=bool(start_resp.get("resumed")),
                          composer_snapshot=snapshot, dead=dead)


def run_drain_restart(names: list, http, process_layer, *, timeout_mins=DEFAULT_TIMEOUT_MINS,
                       poll_interval_s=POLL_INTERVAL_S, stable_polls=STABLE_IDLE_POLLS,
                       stop_wait_s=STOP_WAIT_TIMEOUT_S, sleep_fn=time.sleep,
                       clock=time.monotonic, on_result=None) -> dict:
    """Drive the rolling drain -> graceful-stop -> start cycle for `names`.

    Rolling: each target is restarted as SOON as it individually drains —
    other targets keep polling/waiting independently in the same loop, so one
    slow session never blocks a fast one. `on_result(name, result)` fires the
    instant a session's cycle completes (restart, straggler, or timeout),
    letting callers stream progress. Returns {name: SessionResult}."""
    start = clock()
    deadline = start + timeout_mins * 60
    trackers = {n: DrainTracker(n, stable_needed=stable_polls) for n in names}
    results = {}
    pending = set(names)
    while pending:
        try:
            sessions = http.list_sessions()
        except AmuxError:
            sessions = []
        by_name = {s.get("name"): s for s in sessions}
        now = clock()
        ready_now = []
        for n in list(pending):
            s = by_name.get(n)
            status = (s.get("status") or "") if s else ""
            trackers[n].observe(status, now)
            if trackers[n].ready:
                ready_now.append(n)
        for n in ready_now:
            pending.discard(n)
            result = _drain_one(n, trackers[n], http, process_layer, stop_wait_s,
                                 sleep_fn, clock, start)
            results[n] = result
            if on_result:
                on_result(n, result)
        if not pending:
            break
        if clock() >= deadline:
            for n in pending:
                s = by_name.get(n)
                results[n] = SessionResult(
                    name=n, straggler="timeout",
                    last_status=(s.get("status", "") if s else ""))
                if on_result:
                    on_result(n, results[n])
            pending.clear()
            break
        sleep_fn(poll_interval_s)
    return results


def compute_exit_code(targets: list, results: dict) -> int:
    """0 iff every target restarted with resumed=True; 2 if any straggler,
    missing result, un-restarted target, or resumed=False."""
    for n in targets:
        r = results.get(n)
        if not r or r.straggler or not r.restarted or r.resumed is False:
            return 2
    return 0


def _read_write_token(path=WRITE_TOKEN_PATH) -> str:
    try:
        return open(path, encoding="utf-8").read().strip()
    except OSError:
        return ""


def _format_result(name: str, r) -> str:
    if r is None:
        return f"  {name}: no result (internal error)"
    snap = f" | unsent composer: {r.composer_snapshot!r}" if r.composer_snapshot else ""
    if r.straggler == "timeout":
        return f"  {name}: STRAGGLER (timeout) — last_status={r.last_status or '(empty)'!r}, never touched"
    if r.straggler == "stop-failed":
        return f"  {name}: STRAGGLER (stop-failed) — process survived graceful stop, NOT killed{snap}"
    if r.straggler:
        return f"  {name}: STRAGGLER ({r.straggler}){snap}"
    drained = f"{r.drained_in_s:.1f}s" if r.drained_in_s is not None else "n/a"
    dead_tag = " [revived from dead]" if r.dead else ""
    return (f"  {name}: drained in {drained}{dead_tag} -> restarted={r.restarted} "
            f"resumed={r.resumed}{snap}")


def build_arg_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="amux-drain-restart.py",
        description="Lossless fleet/session restart: wait for idle, then graceful stop+start.")
    p.add_argument("targets", nargs="+", metavar="session",
                    help="session name(s), or 'all' for every running session")
    p.add_argument("--timeout-mins", type=float, default=DEFAULT_TIMEOUT_MINS,
                    help=f"per-run drain timeout in minutes (default {DEFAULT_TIMEOUT_MINS})")
    p.add_argument("--dry-run", action="store_true",
                    help="print the plan + one status poll; make zero POST requests")
    p.add_argument("--yes", action="store_true",
                    help="skip the interactive confirmation prompt")
    p.add_argument("--url", default=os.environ.get("AMUX_URL", DEFAULT_AMUX_URL),
                    help=f"amux server base URL (default {DEFAULT_AMUX_URL}, env AMUX_URL)")
    return p


def main(argv=None) -> int:
    args = build_arg_parser().parse_args(argv)
    http = AmuxClient(args.url, _read_write_token())

    try:
        sessions = http.list_sessions()
    except AmuxError as e:
        print(f"error: could not reach amux server at {args.url}: {e}", file=sys.stderr)
        return 2

    targets, unknown = resolve_targets(sessions, args.targets)
    if unknown:
        print(f"error: unknown session(s): {', '.join(unknown)}", file=sys.stderr)
        return 2
    if not targets:
        print("no matching running sessions; nothing to do")
        return 0

    print(f"Plan — {len(targets)} session(s):")
    for line in build_plan(sessions, targets):
        print(line)

    if args.dry_run:
        print("\n--dry-run: one status poll, no POSTs will be made.\n")
        for line in dry_run_report(sessions, targets):
            print(line)
        return 0

    if not args.yes:
        resp = input(f"\nProceed with drain-restart of {len(targets)} session(s)? [y/N] ")
        if resp.strip().lower() not in ("y", "yes"):
            print("aborted")
            return 1

    process_layer = ProcessLayer()
    print("\nDraining (rolling — each session restarts as soon as it drains)...")
    results = run_drain_restart(
        targets, http, process_layer, timeout_mins=args.timeout_mins,
        on_result=lambda n, r: print(_format_result(n, r)))

    print("\nFinal report:")
    for n in targets:
        print(_format_result(n, results.get(n)))
    return compute_exit_code(targets, results)


if __name__ == "__main__":
    sys.exit(main())
