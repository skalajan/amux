"""Owner-alert storm guard — replay of the real incident, and its negative control.

MF-427. On 2026-08-02/03 the owner-alert channel delivered 38 identical pages,
message '--help', one every ~302s for 3h06m, each a real push AND a real
iMessage, `deduped=false` on all 38. Nothing noticed for three hours.

The pre-existing protection was a 60s dedupe, which is the wrong SHAPE for that
failure rather than merely too short: a storm on a 302s cadence steps cleanly
between 60s windows, so every page counts as distinct and legitimate. A guard
whose window is narrower than the interval it must suppress suppresses nothing.

Why it is worth a guard at all: the owner alert is the fire alarm. Its whole
value is that a page means act now, and 38 empty pages in one night train the
owner to ignore it — destroying the signal exactly as thoroughly as a dead alert
path would.

Both directions are tested here, and the negative one is the load-bearing half.
Collapsing genuine distinct incidents would be a remediation worse than the
defect, so 38 DISTINCT alerts must still deliver 38 times.

Timings below are the real gaps measured from the ledger: 301/302/303s.
"""

import importlib.util
import sys
import types
from pathlib import Path

import pytest


def _load_decision():
    """Import just the pure decision function out of amux-server.py.

    The module is a single large server file with import-time side effects, so
    the function's source is extracted and exec'd standalone. That keeps this
    test honest about WHICH code it exercises: it reads the real file, and fails
    if the function is renamed or removed.
    """
    src = (Path(__file__).resolve().parents[1] / "amux-server.py").read_text()
    start = src.index("URGENT_STORM_THRESHOLD = ")
    end = src.index("# ─────", start)
    ns: dict = {}
    exec(src[start:end], ns)  # noqa: S102 — deliberately executing repo source
    assert "urgent_alert_decision" in ns, "the guard function is gone or renamed"
    return ns


@pytest.fixture
def g():
    return _load_decision()


def _replay(g, timestamps, keys=None):
    """Run attempts through the guard, returning the actions taken.

    Mirrors the server's own state handling: per-key history and mute expiry.
    """
    hist: dict = {}
    mute: dict = {}
    last: dict = {}
    actions = []
    for i, ts in enumerate(timestamps):
        key = keys[i] if keys else "same-key"
        action, h, m = g["urgent_alert_decision"](
            key, ts, hist.get(key), mute.get(key, 0.0), dedupe_last=last.get(key, 0)
        )
        hist[key] = h
        mute[key] = m
        if action in ("send", "storm_notice"):
            last[key] = ts
        actions.append(action)
    return actions


# The real incident: 38 attempts, ~302s apart, one identical message.
_STORM_TS = [i * 302.0 for i in range(38)]


class TestTheRealIncident:
    def test_at_most_two_pages_leave_the_server(self, g):
        actions = _replay(g, _STORM_TS)
        delivered = [a for a in actions if a in ("send", "storm_notice")]
        assert len(delivered) <= 2, (
            f"38 identical pages must collapse to at most 2; got {len(delivered)}: "
            f"{actions[:6]}"
        )

    def test_the_owner_still_learns_it_is_storming(self, g):
        """Silence would be its own failure. One page must say what happened."""
        actions = _replay(g, _STORM_TS)
        assert "storm_notice" in actions, (
            "collapsing to zero pages replaces a noisy alarm with a dead one"
        )
        assert actions[0] == "send", "the first genuine alert must always deliver"

    def test_the_mute_slides_so_a_continuing_storm_never_resumes(self, g):
        """A fixed mute would let 3h06m of 302s attempts page again each window."""
        actions = _replay(g, _STORM_TS)
        after_notice = actions[actions.index("storm_notice") + 1 :]
        assert set(after_notice) <= {"muted"}, (
            f"a still-firing storm resumed paging: {sorted(set(after_notice))}"
        )

    def test_the_old_60s_dedupe_would_have_stopped_none_of_them(self, g):
        """Proves the guard is doing the work, not the pre-existing dedupe."""
        gaps = [302.0] * 37
        assert all(gap >= 60 for gap in gaps)
        actions = _replay(g, _STORM_TS)
        assert "dedupe" not in actions, (
            "at a 302s cadence the 60s window never fires — which is the bug"
        )


class TestTheNegativeControl:
    """The half that matters most: real incidents must not be collapsed."""

    def test_38_distinct_alerts_all_deliver(self, g):
        keys = [f"distinct-{i}" for i in range(38)]
        actions = _replay(g, _STORM_TS, keys=keys)
        assert actions == ["send"] * 38, (
            "distinct genuine alerts share no key and must never be suppressed"
        )

    def test_the_same_message_from_different_sessions_is_not_a_storm(self, g):
        """Two sessions independently hitting the same condition is real news."""
        keys = ["sess-a", "sess-b", "sess-c"]
        actions = _replay(g, [0.0, 5.0, 10.0], keys=keys)
        assert actions == ["send", "send", "send"]

    def test_a_storm_ends_by_stopping(self, g):
        """After a quiet gap longer than the mute, the alert pages again."""
        ts = _STORM_TS[:10] + [_STORM_TS[9] + 3600.0]
        actions = _replay(g, ts)
        assert actions[-1] != "muted", (
            "a storm that has stopped must be able to page again, or a real "
            "recurrence hours later is silently swallowed"
        )

    def test_a_single_alert_is_never_touched(self, g):
        assert _replay(g, [0.0]) == ["send"]

    def test_a_slow_genuine_repeat_outside_the_window_still_pages(self, g):
        """Two of the same alert an hour apart is not a storm."""
        actions = _replay(g, [0.0, 3600.0])
        assert actions == ["send", "send"]


class TestTheExistingDedupeStillWorks:
    def test_a_burst_inside_60s_still_dedupes(self, g):
        actions = _replay(g, [0.0, 10.0, 20.0])
        assert actions[0] == "send"
        assert "dedupe" in actions, "the original 60s repeat guard must survive"
