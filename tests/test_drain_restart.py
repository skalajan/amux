"""Deterministic tests for the drain-restart sidecar (amux-drain-restart.py).

Imports the sidecar's PURE logic via importlib (hyphenated filename, like
tests/test_telegram_sidecar.py) and drives it with mocked HTTP + process
layers — no real server, no real `ps`/`tmux`/`pgrep`, no real sleeping (a fake
clock/sleep_fn stands in for time.monotonic/time.sleep).

Follows this repo's "run directly" test convention (see test_write_auth.py /
test_chat_core.py / test_steering_delivery.py / test_telegram_sidecar.py):
assertions execute at import time and raise on any regression; a trailing
test_drain_restart() stub exists only for pytest collection if pytest is ever
available. Run with: python3 tests/test_drain_restart.py

Covers:
  * status-stability detection (flapping active/idle does not count as
    drained; 2 stable idle polls do)
  * rolling behavior (one session drains + restarts while another is still
    active)
  * straggler timeout (never drained -> never touched, reported)
  * stop-failed path (process survives graceful stop -> NOT killed, straggler)
  * resumed:false surfaced in the result + drives exit code 2
  * composer snapshot extraction (line starting with '❯ ' + non-empty text;
    ANSI stripped; last match wins)
  * dead-claude session (empty status) classed "dead" and revived via
    stop+start
  * self-match safety: a ps line for this script itself is never matched,
    even if it happens to contain "--name <session>"
"""
import importlib.util
import os

HERE = os.path.dirname(os.path.abspath(__file__))
SIDECAR = os.path.join(os.path.dirname(HERE), "amux-drain-restart.py")

_spec = importlib.util.spec_from_file_location("amux_drain_restart", SIDECAR)
dr = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(dr)


# ── fakes ─────────────────────────────────────────────────────────────────────
class FakeClock:
    """A manually-advanced clock: sleep_fn(secs) advances it, now() reads it.
    Lets the drain loop's timing logic run instantly and deterministically."""
    def __init__(self):
        self.t = 0.0
        self.sleeps = []

    def now(self):
        return self.t

    def sleep(self, secs):
        self.sleeps.append(secs)
        self.t += secs


class MockHttp:
    """Mock amux HTTP surface. `status_sequence[name]` is the list of `status`
    values GET /api/sessions returns for that session, one per call (the last
    value repeats once exhausted). stop()/start() record calls and can be
    made to fail or return specific `resumed` values per session."""
    def __init__(self):
        self.status_sequence = {}
        self._idx = {}
        self.peek_text = {}
        self.stop_calls = []
        self.start_calls = []
        self.stop_should_fail = set()
        self.start_should_fail = set()
        self.resumed_for = {}

    def list_sessions(self):
        out = []
        for name, seq in self.status_sequence.items():
            i = self._idx.get(name, 0)
            status = seq[i] if i < len(seq) else seq[-1]
            self._idx[name] = i + 1
            out.append({"name": name, "status": status, "running": True})
        return out

    def peek(self, name, lines=20, live=True):
        return self.peek_text.get(name, "")

    def stop(self, name):
        self.stop_calls.append(name)
        if name in self.stop_should_fail:
            raise dr.AmuxError("simulated stop failure")
        return {"ok": True, "message": "stopping"}

    def start(self, name):
        self.start_calls.append(name)
        if name in self.start_should_fail:
            raise dr.AmuxError("simulated start failure")
        return {"ok": True, "resumed": self.resumed_for.get(name, True)}


class MockProcessLayer:
    """Mock system-process introspection. Sessions marked `alive` via
    set_alive() resolve through the tmux-pane-tree path; sessions marked via
    set_alive_by_name_only() are only discoverable through the ps-snapshot
    `--name` fallback (simulating a launch that predates rename tracking)."""
    def __init__(self):
        self._pane_pid = {}       # tmux_session -> pane_pid
        self._children = {}       # pane_pid -> [pid]
        self._ps = {}             # pid -> command
        self._extra_ps_lines = []  # additional (pid, command) — self-match / fallback tests

    def set_alive(self, name, pane_pid=100, claude_pid=101, command="/opt/homebrew/bin/claude --resume abc-uuid"):
        self._pane_pid[f"amux-{name}"] = pane_pid
        self._children[pane_pid] = [claude_pid]
        self._ps[claude_pid] = command

    def clear_alive(self, name):
        pane_pid = self._pane_pid.pop(f"amux-{name}", None)
        if pane_pid is not None:
            for pid in self._children.pop(pane_pid, []):
                self._ps.pop(pid, None)

    def add_ps_line(self, pid, command):
        self._extra_ps_lines.append((pid, command))

    def tmux_pane_pid(self, tmux_session):
        return self._pane_pid.get(tmux_session, 0)

    def pgrep_children(self, parent_pid):
        return list(self._children.get(parent_pid, []))

    def ps_snapshot(self):
        return list(self._ps.items()) + list(self._extra_ps_lines)


def _no_sleep(_secs):
    """A sleep_fn that does nothing — for tests where the process is never
    alive, so wait_for_claude_exit returns on its first check and never sleeps."""
    pass


# ── status-stability detection ────────────────────────────────────────────────
t = dr.DrainTracker("flap")
now = 0.0
for status in ["active", "idle", "active", "idle"]:
    now += 1.0
    t.observe(status, now)
assert not t.ready, "flapping active/idle must never accumulate to drained"
now += 1.0
t.observe("idle", now)  # 2nd CONSECUTIVE idle now
assert t.ready and t.ready_reason == "idle-stable", "2 stable idle polls must mark drained"
assert t.ready_at == now
print("DrainTracker ok — flapping active/idle never drains; 2 consecutive idle polls do")

t2 = dr.DrainTracker("dead-immediate")
t2.observe("", 5.0)
assert t2.ready and t2.ready_reason == "dead" and t2.ready_at == 5.0, \
    "empty status (claude not running) must be ready immediately, classed 'dead'"
print("DrainTracker ok — empty status is immediately ready, classed 'dead'")


# ── composer snapshot extraction ──────────────────────────────────────────────
assert dr.extract_composer_snapshot("") == ""
assert dr.extract_composer_snapshot("no composer marker here\njust text") == ""
assert dr.extract_composer_snapshot("❯ \n") == "", "bare ❯ with no text is not a snapshot"
assert dr.extract_composer_snapshot("some line\n❯ finish the refactor\nmore junk") == "finish the refactor"
# ANSI-styled composer line (dim-gray ❯ + colored text, as amux-server.py emits it)
ansi_line = "\x1b[38;5;239m\x1b[48;5;237m❯ \x1b[38;5;231mtype this after stripping ansi\x1b[0m"
assert dr.extract_composer_snapshot(ansi_line) == "type this after stripping ansi"
# Last matching ❯ line wins (an earlier echoed message must not shadow the real composer).
multi = "❯ [09:32 AM] an earlier echoed message\nsome tool output\n❯ the actual unsent draft"
assert dr.extract_composer_snapshot(multi) == "the actual unsent draft"
print("extract_composer_snapshot ok — ANSI stripped, empty composer -> '', last ❯ line wins")


# ── self-match safety ─────────────────────────────────────────────────────────
assert dr._cmd_is_claude_binary("/opt/homebrew/bin/claude --resume abc --model x") is True
assert dr._cmd_is_claude_binary("/usr/bin/python3 amux-drain-restart.py session1") is False
assert dr._cmd_is_claude_binary("") is False
# Word-boundary safety: a longer session name must not match a shorter target.
assert dr._cmd_mentions_session("/usr/local/bin/claude --name foobar", "foo") is False
assert dr._cmd_mentions_session("/usr/local/bin/claude --name foo", "foo") is True

proc = MockProcessLayer()
# A decoy ps line for THIS SCRIPT'S OWN process — deliberately crafted to contain
# the literal substring "--name victim" to prove that command-prefix (not just
# flag-substring) matching is what protects us: the executable is python3, not
# claude, so it must never be treated as victim's claude process.
proc.add_ps_line(9999, "/usr/bin/python3 /path/amux-drain-restart.py --name victim")
assert dr.find_claude_pid(proc, "victim") == 0, \
    "a ps line containing this script's own text must never be matched, even if it contains '--name <session>'"
print("self-match safety ok — this script's own ps line is never matched as a claude process")

# find_claude_pid also resolves a REAL claude process via the tmux-pane tree
# (covers --resume-launched sessions, which don't contain '--name <session>' at all).
proc.set_alive("realsession", pane_pid=200, claude_pid=201, command="/opt/homebrew/bin/claude --resume some-uuid")
assert dr.find_claude_pid(proc, "realsession") == 201
proc.clear_alive("realsession")
assert dr.find_claude_pid(proc, "realsession") == 0
# ... and via the ps '--name' fallback for sessions with no resolvable tmux pane.
proc.add_ps_line(301, "/opt/homebrew/bin/claude --name freshstart")
assert dr.find_claude_pid(proc, "freshstart") == 301
print("find_claude_pid ok — resolves via tmux-pane tree (--resume) and ps '--name' fallback")


# ── rolling behavior: one session drains+restarts while another is still active ─
http = MockHttp()
http.status_sequence = {
    "s1": ["active", "idle", "idle"],                        # drains at poll 3
    "s2": ["active", "active", "active", "idle", "idle"],    # drains at poll 5
}
proc = MockProcessLayer()  # neither session's claude process is "alive" -> exits instantly on stop
order = []
clk = FakeClock()
results = dr.run_drain_restart(
    ["s1", "s2"], http, proc,
    timeout_mins=60, poll_interval_s=3.0, stop_wait_s=5.0,
    sleep_fn=clk.sleep, clock=clk.now,
    on_result=lambda n, r: order.append(n),
)
assert order == ["s1", "s2"], f"s1 must complete its restart before s2 even drains (rolling): {order}"
assert results["s1"].restarted and results["s1"].resumed is True
assert results["s2"].restarted and results["s2"].resumed is True
assert http.stop_calls == ["s1", "s2"] and http.start_calls == ["s1", "s2"]
assert results["s1"].drained_in_s < results["s2"].drained_in_s
print("run_drain_restart ok — rolling: s1 drains+restarts while s2 is still active")


# ── straggler: timeout (never drained -> never touched) ──────────────────────
http = MockHttp()
http.status_sequence = {"s3": ["active", "active", "active", "active", "active"]}
proc = MockProcessLayer()
clk = FakeClock()
results = dr.run_drain_restart(
    ["s3"], http, proc,
    timeout_mins=0.05, poll_interval_s=3.0,  # deadline = 3s, one poll interval
    sleep_fn=clk.sleep, clock=clk.now,
)
r3 = results["s3"]
assert r3.straggler == "timeout", r3
assert r3.last_status == "active"
assert not r3.restarted and r3.drained_in_s is None
assert "s3" not in http.stop_calls and "s3" not in http.start_calls, \
    "a straggler that never drains must NEVER be stopped or started"
print("run_drain_restart ok — straggler timeout: never-idle session is reported, never touched")


# ── straggler: stop-failed (process survives graceful stop -> no kill) ────────
http = MockHttp()
http.status_sequence = {"s4": ["idle", "idle"]}  # drains immediately (2 stable polls)
http.peek_text["s4"] = "❯ unsent draft on s4"
proc = MockProcessLayer()
proc.set_alive("s4", pane_pid=400, claude_pid=401, command="/opt/homebrew/bin/claude --resume s4-uuid")
# s4's claude process is ALWAYS alive — it never exits, simulating a hung graceful stop.
clk = FakeClock()
results = dr.run_drain_restart(
    ["s4"], http, proc,
    timeout_mins=60, poll_interval_s=3.0, stop_wait_s=5.0,
    sleep_fn=clk.sleep, clock=clk.now,
)
r4 = results["s4"]
assert r4.straggler == "stop-failed", r4
assert r4.composer_snapshot == "unsent draft on s4"
assert "s4" in http.stop_calls, "a graceful stop MUST have been attempted"
assert "s4" not in http.start_calls, "must never /start a session whose process survived the stop"
print("run_drain_restart ok — stop-failed: surviving process is reported as a straggler, never killed, never started")


# ── resumed:false surfaced in report + drives exit code 2 ─────────────────────
http = MockHttp()
http.status_sequence = {"s5": ["idle", "idle"]}
http.resumed_for["s5"] = False
proc = MockProcessLayer()  # not alive -> exits instantly
clk = FakeClock()
results = dr.run_drain_restart(["s5"], http, proc, timeout_mins=60,
                                sleep_fn=clk.sleep, clock=clk.now)
r5 = results["s5"]
assert r5.restarted is True and r5.resumed is False
assert dr.compute_exit_code(["s5"], results) == 2, "resumed=False must drive exit code 2"
# A fully-successful run must exit 0.
ok_results = {"s5": dr.SessionResult(name="s5", restarted=True, resumed=True)}
assert dr.compute_exit_code(["s5"], ok_results) == 0
print("run_drain_restart ok — resumed:false is surfaced and drives exit code 2; full success drives exit code 0")


# ── dead-claude session: classed 'dead', revived via stop+start ───────────────
http = MockHttp()
http.status_sequence = {"s6": [""]}  # empty status from the very first poll — claude is not running
proc = MockProcessLayer()  # no tmux pane / no claude pid at all for s6
clk = FakeClock()
results = dr.run_drain_restart(["s6"], http, proc, timeout_mins=60,
                                sleep_fn=clk.sleep, clock=clk.now)
r6 = results["s6"]
assert r6.dead is True, r6
assert r6.restarted is True
assert "s6" in http.stop_calls and "s6" in http.start_calls, \
    "a dead session must be revived via stop+start, not left as an untouched straggler"
assert r6.straggler == ""
print("run_drain_restart ok — dead-claude session (empty status) is classed 'dead' and revived via stop+start")


# ── resolve_targets / dry-run classification (cheap extra coverage) ──────────
sessions = [
    {"name": "a", "status": "idle", "running": True},
    {"name": "b", "status": "active", "running": True},
    {"name": "c", "status": "", "running": False},  # fully stopped -> excluded from 'all'
]
targets, unknown = dr.resolve_targets(sessions, ["all"])
assert targets == ["a", "b"] and unknown == []
targets, unknown = dr.resolve_targets(sessions, ["a", "nope"])
assert targets == ["a"] and unknown == ["nope"]
report = dr.dry_run_report(sessions, ["a", "b", "c"])
assert any("DEAD" in line for line in report if line.strip().startswith("c"))
print("resolve_targets/dry_run_report ok — 'all' resolves running sessions only; unknown names surfaced")


print("\nALL DRAIN-RESTART CHECKS PASSED")


def test_drain_restart():
    """The scenarios above execute at import time and raise on any regression;
    reaching here means drain-stability tracking, the rolling restart state
    machine, straggler classification (timeout + stop-failed), the dead-claude
    revival path, composer-snapshot extraction, and self-match-safe process
    matching all hold."""
    assert True
