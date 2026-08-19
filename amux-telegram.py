#!/usr/bin/env python3
"""amux-telegram — Telegram <-> amux session-chat sidecar (Scope B3, decision B3-alpha).

A standalone, stdlib-only sidecar. Upstream (mixpeek/amux) has no such file, so it is
conflict-immune and makes ZERO changes to amux-server.py — it talks to the running
server purely over its localhost HTTP API (the B1 chat core):

  inbound  : Telegram getUpdates long-poll  ->  POST /api/chat  (origin "telegram")
  outbound : GET /api/chat?session=&since=<cursor>  ->  Telegram sendMessage (per topic)

Design invariants (plan .omc/plans/chat-layer-auth.md sec 6 / sec 10):
  * Owner-only: messages from a non-TG_OWNER_ID user are ignored + logged.
  * Inbound crash-safety: the long-poll offset advances ONLY AFTER amux returns a
    durable 200. The chat id is derived from the Telegram update_id, so a re-delivery
    after a crash is an idempotent no-op server-side.
  * Outbound exactly-once + transcript order: forwards are deduped by the STABLE reply
    id (conversation_id:turn_index); rowid_seq is a fetch-optimization cursor only, so a
    server-side cache rebuild (which may renumber seqs) can neither stall nor re-flood.
  * Resilience: amux self-restarts on file save (connection drops are NORMAL) and
    Telegram API errors are retried with backoff — the sidecar reconnects, never dies.

The pure logic (id derivation, offset handling, outbound cursor/dedup, topic mapping) is
importable and unit-tested (tests/test_telegram_sidecar.py); the network layer is
injected so it can be mocked. The hyphenated filename intentionally blocks a reverse
`import` into amux-server.py.

Setup: see docs/telegram-chat.md.  Config: ~/.amux/telegram.env (0600).
"""
import hashlib
import json
import logging
import os
import re
import socket
import ssl
import stat
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

log = logging.getLogger("amux-telegram")

HOME = os.path.expanduser("~")
AMUX_DIR = os.path.join(HOME, ".amux")
CONFIG_PATH = os.path.join(AMUX_DIR, "telegram.env")
WRITE_TOKEN_PATH = os.path.join(AMUX_DIR, "write_token")
TOPICS_PATH = os.path.join(AMUX_DIR, "telegram-topics.json")
OFFSET_PATH = os.path.join(AMUX_DIR, "telegram-offset")
OUTBOUND_PATH = os.path.join(AMUX_DIR, "telegram-outbound.json")
PROMPTS_PATH = os.path.join(AMUX_DIR, "telegram-prompts.json")
LIVE_PATH = os.path.join(AMUX_DIR, "telegram-live.json")
COUNTERS_PATH = os.path.join(AMUX_DIR, "telegram-counters.json")

# Permission-prompt notify (plan .omc/plans/telegram-permissions.md Phase B).
# Only ping once a session has been CONTINUOUSLY "waiting" this long — a debounce
# longer than the server's yolo auto-responder cooldown, so recognized/auto-
# answerable prompts self-clear and never reach Jan (plan B.2).
# How long a session must sit continuously `waiting` before its prompt pings the
# phone. Raised 10 -> 90 on 2026-08-18 (Jan's decision, plan chat-improvement.md
# C2b'). At 10s a prompt Jan answered himself at the keyboard in 20s still buzzed
# his phone, and the permission prompt is structurally the loudest class in the
# fleet — it is the only one that rings per waiting session, and it cannot be
# silenced by /ring off or /quiet (only /mute) precisely because losing one is
# worse than hearing one. So the honest lever is WHEN it fires, not whether.
# Nothing is lost by waiting: the continuous-waiting timer keeps running, so a
# genuinely blocked session still pings — 90s later instead of 10s later.
PERM_GRACE_SECS = float(os.environ.get("TG_PERM_GRACE_SECS", "90") or 90)
# Suppress the "input needed / permission prompt" Telegram ping for a session
# while a human tmux client is attached to it — Jan is sitting at the CLI and
# sees the prompt live, so pinging his phone is pure spam. He detaches ⇒ the next
# poll pings normally (he walked away from a still-open prompt). tmux failure ⇒
# empty attached-set ⇒ nothing suppressed (fail toward notifying — never hide a
# prompt he might need to answer remotely). TG_SUPPRESS_ATTACHED=0 disables.
SUPPRESS_ATTACHED = os.environ.get("TG_SUPPRESS_ATTACHED", "1") != "0"
TMUX_PREFIX = os.environ.get("AMUX_TMUX_PREFIX", "amux-")
# Timeout for the per-session GET /api/chat in the outbound sweep. Short by
# design: the sweep is serial, so this bounds how long one slow session can block
# every other session's updates.
CHAT_TIMEOUT_SECS = float(os.environ.get("TG_CHAT_TIMEOUT_SECS", "8") or 8)
PERM_PEEK_LINES = 40
PERM_PROMPT_MAX_CHARS = 1500

DEFAULT_TG_API_BASE = "https://api.telegram.org"
# 8822 was retired at the Rust cutover (2026-08-17) and nothing listens there.
# The server writes its live address to ~/.amux/endpoint.json on every boot, so
# resolve from that and fall back to the current canonical port — a hardcoded dead
# port is how a config-less start silently fails every request.
def _default_amux_base():
    try:
        with open(os.path.join(AMUX_DIR, "endpoint.json"), encoding="utf-8") as f:
            url = (json.load(f) or {}).get("canonical_url")
            if url:
                return str(url).rstrip("/")
    except (OSError, ValueError, AttributeError):
        pass
    return "https://localhost:8824"


DEFAULT_AMUX_BASE = _default_amux_base()

# Per-topic display mode for outbound session replies. "smart" is the DEFAULT
# for every topic (new and pre-existing) unless overridden by /mode or
# TG_DEFAULT_MODE.
VALID_MODES = ("smart", "brief", "full")


def parse_attached_sessions(tmux_output, prefix=TMUX_PREFIX):
    """Parse `tmux list-sessions -F '#{session_name} #{session_attached}'` output
    into the set of amux session names (prefix stripped) that have >=1 client
    attached. Malformed lines are skipped; a session_attached of "" or "0" counts
    as detached."""
    out = set()
    for line in (tmux_output or "").splitlines():
        line = line.strip()
        if not line:
            continue
        parts = line.rsplit(" ", 1)
        if len(parts) != 2:
            continue
        tmux_name, attached = parts[0].strip(), parts[1].strip()
        if attached in ("", "0"):
            continue
        out.add(tmux_name[len(prefix):] if tmux_name.startswith(prefix) else tmux_name)
    return out


# ── config ────────────────────────────────────────────────────────────────────
def parse_env_file(text):
    """Parse a KEY=VALUE .env file (comments with #, blank lines ignored)."""
    out = {}
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            continue
        k, v = line.split("=", 1)
        k = k.strip()
        v = v.strip().strip('"').strip("'")
        if k:
            out[k] = v
    return out


class ConfigError(Exception):
    pass


def load_config(config_path=CONFIG_PATH, write_token_path=WRITE_TOKEN_PATH, environ=None):
    """Load + validate config. Raises ConfigError with a clear message on missing
    file, insecure perms, or missing required keys. Process env overrides the file."""
    environ = os.environ if environ is None else environ
    if not os.path.exists(config_path):
        raise ConfigError(
            f"config not found: {config_path}\n"
            "Create it (0600) with:\n"
            "  TG_BOT_TOKEN=<from @BotFather>\n"
            "  TG_OWNER_ID=<your numeric Telegram user id>\n"
            "  TG_CHAT_ID=<the forum supergroup id, e.g. -1001234567890>\n"
            "See docs/telegram-chat.md.")
    st = os.stat(config_path)
    if st.st_mode & 0o077:
        raise ConfigError(
            f"insecure perms on {config_path} (mode {oct(stat.S_IMODE(st.st_mode))}); "
            f"run: chmod 600 {config_path}")
    filecfg = parse_env_file(open(config_path, encoding="utf-8").read())

    def get(key, default=None):
        return environ.get(key, filecfg.get(key, default))

    token = (get("TG_BOT_TOKEN") or "").strip()
    owner = (get("TG_OWNER_ID") or "").strip()
    if not token:
        raise ConfigError("TG_BOT_TOKEN missing in telegram.env")
    if not owner:
        raise ConfigError("TG_OWNER_ID missing in telegram.env")
    try:
        owner_id = int(owner)
    except ValueError:
        raise ConfigError(f"TG_OWNER_ID must be a numeric user id, got {owner!r}")

    write_token = ""
    try:
        write_token = open(write_token_path, encoding="utf-8").read().strip()
    except OSError:
        log.warning("write token not readable at %s — amux writes may 401", write_token_path)

    # Shared auth token (~/.amux/auth_token) — the file BOTH servers read. Needed
    # on every request, reads included, once AMUX_RS_NO_LOOPBACK_BYPASS=1 is set.
    auth_token = ""
    try:
        auth_token = open(os.path.join(AMUX_DIR, "auth_token"), encoding="utf-8").read().strip()
    except OSError:
        log.warning("auth token not readable — reads will 401 if the loopback bypass is off")

    chat_id = (get("TG_CHAT_ID") or "").strip()

    default_mode = (get("TG_DEFAULT_MODE") or "smart").strip().lower()
    if default_mode not in VALID_MODES:
        log.warning("TG_DEFAULT_MODE=%r invalid (want smart/brief/full) — using 'smart'",
                    default_mode)
        default_mode = "smart"

    return {
        "bot_token": token,
        "owner_id": owner_id,
        "chat_id": chat_id,
        "tg_api_base": (get("TG_API_BASE") or DEFAULT_TG_API_BASE).rstrip("/"),
        "amux_base": (get("AMUX_BASE") or DEFAULT_AMUX_BASE).rstrip("/"),
        # Where /api/chat lives. Empty = same host as amux_base (pre-cutover, when
        # the python server still served it). Post-cutover this points at the
        # amux-chat.py sidecar, because upstream's rust server has no such route.
        "chat_base": (get("AMUX_CHAT_BASE") or "").rstrip("/") or None,
        # Read on EVERY request once the rust loopback bypass is off.
        "auth_token": (get("AMUX_AUTH_TOKEN") or auth_token),
        "write_token": write_token,
        "poll_secs": float(get("TG_POLL_SECS", "2.0") or 2.0),
        "long_poll_secs": int(get("TG_LONG_POLL_SECS", "25") or 25),
        "default_mode": default_mode,
        "summary_model": (get("TG_SUMMARY_MODEL") or "haiku").strip(),
        "summary_timeout": float(get("TG_SUMMARY_TIMEOUT", "90") or 90),
        "summary_config_dir": (get("TG_SUMMARY_CONFIG_DIR") or "").strip() or None,
        "machine_label": (get("TG_MACHINE_LABEL") or "").strip() or None,
        # Silent/invisible updates + presence layer (plan
        # .omc/plans/telegram-silent-updates.md, M1).
        "final_settle_secs": float(get("TG_FINAL_SETTLE_SECS", "4") or 4),
        "presence": (get("TG_PRESENCE", "1") or "1").strip().lower()
                    not in ("0", "false", "no", "off"),
        "presence_react": (get("TG_PRESENCE_REACT", "0") or "0").strip().lower()
                          in ("1", "true", "yes", "on"),
        # Quiet mode (plan .omc/plans/telegram-quiet-mode.md, design A): default ON
        # (1) — while quiet, only questions, failures, and the latch-armed answer to
        # a Telegram turn ring; autonomous finals land silently in the live box.
        # TG_QUIET_DEFAULT=0 forces the legacy route_reply behavior (kill switch).
        "quiet_default": (get("TG_QUIET_DEFAULT", "1") or "1").strip().lower()
                         not in ("0", "false", "no", "off"),
    }


# ── pure logic: idempotent inbound id ──────────────────────────────────────────
def derive_inbound_id(update_id):
    """Stable chat id from a Telegram update_id. Same update_id -> same id, so a
    re-delivered update POSTs the same id and the server dedups (INSERT OR IGNORE)."""
    return "tg-" + str(update_id)


# ── pure logic: update inspection ──────────────────────────────────────────────
def update_message(update):
    return update.get("message") or update.get("edited_message") or {}


def is_owner(update, owner_id):
    frm = update_message(update).get("from") or {}
    try:
        return int(frm.get("id")) == int(owner_id)
    except (TypeError, ValueError):
        return False


def message_text(update):
    return (update_message(update).get("text") or "").strip()


def callback_query(update):
    """The callback_query payload of an inline-button tap, or {} for a
    non-callback update."""
    return update.get("callback_query") or {}


def is_callback_owner(update, owner_id):
    """Owner gate for a callback_query — the tapping user is callback_query.from,
    NOT message.from (which is the bot that posted the buttons)."""
    frm = callback_query(update).get("from") or {}
    try:
        return int(frm.get("id")) == int(owner_id)
    except (TypeError, ValueError):
        return False


def message_topic_id(update):
    """The forum topic id (message_thread_id) for a topic message, else None.
    The General topic has no message_thread_id."""
    tid = update_message(update).get("message_thread_id")
    try:
        return int(tid) if tid is not None else None
    except (TypeError, ValueError):
        return None


def parse_command(text):
    """('/peek', ['3']) from '/peek 3'. Strips a @botname suffix. Non-command -> (None, [])."""
    if not text.startswith("/"):
        return None, []
    parts = text.split()
    cmd = parts[0].split("@", 1)[0].lower()
    return cmd, parts[1:]


def command_raw_arg(text):
    """Everything after the command word, exactly as typed — internal spacing,
    punctuation and case preserved. parse_command()'s text.split() collapses
    runs of whitespace and loses this, which matters for /type (the argument
    may be an OAuth code or similar where mangling it defeats the point).
    str.split(None, 1) only consumes the single whitespace run separating the
    command from its argument; everything after that is returned untouched."""
    parts = text.split(None, 1)
    return parts[1] if len(parts) > 1 else ""


# ── pure logic: topic <-> session mapping (persisted) ──────────────────────────
class TopicStore:
    """session <-> forum-topic-id map + per-session mute set. Pure logic; JSON persist."""

    def __init__(self, path=TOPICS_PATH, state=None):
        self.path = path
        state = state or {}
        # session -> topic_id
        self._topics = {str(k): int(v) for k, v in (state.get("topics") or {}).items()}
        self._muted = set(str(s) for s in (state.get("muted") or []))
        # session -> display mode override ("smart"/"brief"/"full"); absent ->
        # falls back to the global TG_DEFAULT_MODE. Unknown values from a
        # hand-edited file are dropped rather than raising.
        self._modes = {str(k): v for k, v in (state.get("modes") or {}).items()
                       if v in VALID_MODES}
        # session -> /ring off (force disable_notification on EVERY forward for
        # this topic, regardless of governing origin). This is a full
        # mute-of-sound, distinct from /mute's content suppression. Absent ->
        # "on" (the origin rule in Bot.forward_session governs, the default).
        self._ring_off = set(str(s) for s in (state.get("ring_off") or []))
        # Fleet-scope quiet flag (chat scope). Top-level, not per-session.
        self._quiet = bool(state.get("quiet") or False)

    @classmethod
    def load(cls, path=TOPICS_PATH):
        try:
            with open(path, encoding="utf-8") as f:
                return cls(path, json.load(f))
        except (OSError, ValueError):
            return cls(path, {})

    def to_dict(self):
        return {"topics": dict(self._topics), "muted": sorted(self._muted),
                "modes": dict(self._modes), "ring_off": sorted(self._ring_off),
                "quiet": self._quiet}

    def save(self):
        _atomic_write_0600(self.path, json.dumps(self.to_dict(), indent=2))

    def topic_for_session(self, session):
        return self._topics.get(str(session))

    def session_for_topic(self, topic_id):
        if topic_id is None:
            return None
        for s, t in self._topics.items():
            if t == int(topic_id):
                return s
        return None

    def set(self, session, topic_id):
        self._topics[str(session)] = int(topic_id)

    def is_muted(self, session):
        return str(session) in self._muted

    def mute(self, session):
        self._muted.add(str(session))

    def unmute(self, session):
        self._muted.discard(str(session))

    def mode_for_session(self, session):
        """This session's mode override, or None (caller falls back to the
        global default)."""
        return self._modes.get(str(session))

    def set_mode(self, session, mode):
        if mode not in VALID_MODES:
            raise ValueError(f"invalid mode: {mode!r} (want one of {VALID_MODES})")
        self._modes[str(session)] = mode

    def is_ring_off(self, session):
        return str(session) in self._ring_off

    def set_ring_off(self, session, off):
        session = str(session)
        if off:
            self._ring_off.add(session)
        else:
            self._ring_off.discard(session)

    # ── fleet-scope quiet (chat scope, NOT session scope) ──────────────────────
    # Deliberately a TOP-LEVEL key, not a session key like everything else in
    # this store: /quiet governs the whole chat. to_dict() emits it
    # unconditionally — the apparent absence of `ring_off` from an older state
    # file is exactly what produced a false "no command has ever run" finding
    # during planning, so an always-present key is worth the two bytes.
    def is_quiet(self):
        return self._quiet

    def set_quiet(self, on):
        self._quiet = bool(on)


class CounterStore:
    """Cumulative per-(kind, class) notification counters — the DENOMINATOR.

    Before this existed the sidecar logged no successful send at any level: of
    573 log lines containing "forward", every one was a failure or a traceback
    frame and zero were INFO. So "how many messages arrived, and how many rang?"
    — the single question Jan ranked first — was unanswerable from retained data,
    and every ranking claim made during planning had to be retracted for exactly
    that reason.

    Kept deliberately small and monotonic: a flat {"kind:class": n} map plus a
    `since` stamp. It is persisted so a sidecar restart doesn't reset the
    denominator mid-measurement, and surfaced by /quiet status."""

    def __init__(self, path=COUNTERS_PATH, state=None):
        self.path = path
        state = state or {}
        self._n = {str(k): int(v) for k, v in (state.get("counts") or {}).items()}
        self._since = float(state.get("since") or 0) or time.time()

    @classmethod
    def load(cls, path=COUNTERS_PATH):
        try:
            with open(path, encoding="utf-8") as f:
                return cls(path, json.load(f))
        except (OSError, ValueError):
            return cls(path, {})

    def to_dict(self):
        return {"counts": dict(self._n), "since": self._since}

    def save(self):
        _atomic_write_0600(self.path, json.dumps(self.to_dict(), indent=2))

    def bump(self, kind, klass):
        key = f"{kind}:{klass}"
        self._n[key] = self._n.get(key, 0) + 1

    def total(self, klass=None):
        if klass is None:
            return sum(self._n.values())
        return sum(v for k, v in self._n.items() if k.endswith(":" + klass))

    def render(self):
        """Human-readable tally for /quiet status."""
        if not self._n:
            return "žádná rozhodnutí zatím nezaznamenána"
        hours = max(1.0, (time.time() - self._since) / 3600.0)
        lines = [f"za {hours:.1f} h — celkem {self.total()} "
                 f"(ring {self.total('ring')} · live {self.total('live')} · "
                 f"suppress {self.total('suppress')})"]
        for key in sorted(self._n):
            lines.append(f"  {key} = {self._n[key]}  ({self._n[key] / hours:.1f}/h)")
        return "\n".join(lines)


# ── pure logic: pending permission prompt per session (persisted) ──────────────
class PromptStore:
    """Per-session pending permission-prompt state (plan B.5): one dict per
    session — {fp, message_id, ts, kind, body, answered}. fp is the dedup key:
    while the live prompt's fp matches the stored fp we never re-notify. The
    `answered` flag records that a callback resolved this prompt in place (the
    Telegram message already shows "✅ Allowed …"), so the outbound loop's
    leave-waiting cleanup clears state WITHOUT overwriting that edit. Persisted
    (atomic 0600) so a sidecar restart doesn't re-notify an already-seen prompt."""

    def __init__(self, path=PROMPTS_PATH, state=None):
        self.path = path
        self._pending = {str(k): dict(v) for k, v in (state or {}).items()}

    @classmethod
    def load(cls, path=PROMPTS_PATH):
        try:
            with open(path, encoding="utf-8") as f:
                return cls(path, json.load(f))
        except (OSError, ValueError):
            return cls(path, {})

    def to_dict(self):
        return self._pending

    def save(self):
        _atomic_write_0600(self.path, json.dumps(self._pending))

    def get(self, session):
        return self._pending.get(str(session))

    def pending_sessions(self):
        return list(self._pending.keys())

    def set(self, session, fp, message_id, ts, kind, body=""):
        self._pending[str(session)] = {"fp": fp, "message_id": message_id,
                                       "ts": ts, "kind": kind, "body": body,
                                       "answered": False}

    def mark_answered(self, session):
        p = self._pending.get(str(session))
        if p:
            p["answered"] = True

    def clear(self, session):
        return self._pending.pop(str(session), None)


# ── pure logic: per-session rolling "live box" + presence surface (persisted) ──
class LiveStore:
    """Per-session state for the silent rolling live box + presence header (plan
    .omc/plans/telegram-silent-updates.md, Option B / r4 presence). One dict per
    session:
      * message_id          — the live box's Telegram message id (edited in place,
                              invisible; created once with a single silent send).
      * text_hash           — hash of the box's current text; skips no-op edits so
                              an unchanged render never hits `400 not-modified`.
      * candidate_reply_id  — the newest not-yet-promoted session reply id; the
                              promotion tail keys off THIS (Hazard 1), never the
                              per-row loop.
      * rung_reply_id       — the reply id already rung / written as a settled
                              final; blocks re-ringing across polls + restarts.
      * read_ts / done_ts   — HH:MM stamps for the '👀 přečteno' / '✅ hotovo'
                              header states (fixed at event time so the header
                              string is stable across polls -> text_hash skips).
      * idle_phase          — 'read' | 'done': which header to show while idle.
      * body                — the last rendered body text, so a header-only poll
                              (status change, no new reply) re-renders header+body
                              without re-summarizing.
      * awaiting_tg_reply   — the answer-latch (plan telegram-quiet-mode.md): True
                              once Jan acts on this session via Telegram; the next
                              final that post-dates latch_arm_key rings even while
                              quiet, then clears it.
      * latch_arm_key       — the thread-order boundary [ts, seq] at arm time (the
                              latest known key). Only a strictly-later final is the
                              real answer; an in-flight autonomous final recorded
                              at-or-before it can't consume the latch (post-dating
                              guard).
      * latest_key          — the most recent thread-order key [ts, seq] observed
                              for this session; the source of latch_arm_key at arm.
      * limit_rung          — shared episode key: set when a usage-limit ring fires
                              (either the limit-status check or the usage-limit
                              system row) so the episode rings exactly once; cleared
                              when the session leaves `limit` status.
    Persisted (atomic 0600) so a sidecar restart keeps editing the same box
    (no re-create badge) and never re-rings an already-settled final."""

    _FIELDS = ("message_id", "text_hash", "candidate_reply_id", "rung_reply_id",
               "read_ts", "done_ts", "idle_phase", "body",
               "awaiting_tg_reply", "latch_arm_key", "latest_key", "limit_rung")

    def __init__(self, path=LIVE_PATH, state=None):
        self.path = path
        self._live = {str(k): dict(v) for k, v in (state or {}).items()}

    @classmethod
    def load(cls, path=LIVE_PATH):
        try:
            with open(path, encoding="utf-8") as f:
                return cls(path, json.load(f))
        except (OSError, ValueError):
            return cls(path, {})

    def to_dict(self):
        return self._live

    def save(self):
        _atomic_write_0600(self.path, json.dumps(self._live))

    def get(self, session):
        return self._live.get(str(session))

    def set_fields(self, session, **kw):
        d = self._live.setdefault(str(session),
                                  {k: None for k in self._FIELDS})
        for k, v in kw.items():
            d[k] = v
        return d


# ── pure logic: inbound long-poll offset (persisted) ───────────────────────────
class OffsetStore:
    """Telegram getUpdates offset. Advances ONLY after a durable amux ack, so a
    crash between Telegram-receive and amux-persist re-delivers the update."""

    def __init__(self, path=OFFSET_PATH, value=0):
        self.path = path
        self.value = int(value)

    @classmethod
    def load(cls, path=OFFSET_PATH):
        try:
            return cls(path, int(open(path, encoding="utf-8").read().strip() or 0))
        except (OSError, ValueError):
            return cls(path, 0)

    def get(self):
        return self.value

    def advance_to(self, update_id):
        """Advance so this update_id is not re-delivered (offset = update_id + 1)."""
        nxt = int(update_id) + 1
        if nxt > self.value:
            self.value = nxt
            self.save()

    def save(self):
        _atomic_write_0600(self.path, str(self.value))


# ── pure logic: transcript order for a merged thread item ──────────────────────
def _thread_order_key(item):
    """Sort key for a merged /api/chat thread item: (ts, then seq). Owner/system
    rows carry seq=None (sorted before a same-ts reply row, seq >= 0). Shared by
    OutboundTracker.select, sorted_session_replies, and Bot.forward_session's
    notification-routing walk so all three agree on "transcript order"."""
    return (item.get("ts") or 0, item.get("seq") if item.get("seq") is not None else -1)


# ── pure logic: outbound cursor + stable-id dedup (persisted) ──────────────────
class OutboundTracker:
    """Per-session outbound forwarding state: a rowid_seq high-water cursor (fetch
    optimization), the set of stable reply ids already forwarded (the real
    exactly-once key), AND the origin of the most recently observed owner-role
    item (docs/telegram-chat.md "Notifications" — the 'governing origin' used to
    decide whether a forwarded reply rings or is sent silently). Dedup-by-id makes
    forwarding rebuild-safe: if a cache rebuild renumbers rowid_seq below our
    cursor we refetch from 0 (no stall) and the seen-id set prevents re-flooding
    (C-crit-2)."""

    SEEN_CAP = 2000

    def __init__(self, path=OUTBOUND_PATH, state=None):
        self.path = path
        self._state = {}
        for sess, st in (state or {}).items():
            self._state[str(sess)] = {
                "last_seq": int(st.get("last_seq", 0)),
                "seen": list(st.get("seen", [])),
                "last_owner_origin": st.get("last_owner_origin"),
            }

    @classmethod
    def load(cls, path=OUTBOUND_PATH):
        try:
            with open(path, encoding="utf-8") as f:
                return cls(path, json.load(f))
        except (OSError, ValueError):
            return cls(path, {})

    def to_dict(self):
        return self._state

    def save(self):
        _atomic_write_0600(self.path, json.dumps(self._state))

    def known(self, session):
        return str(session) in self._state

    def fetch_since(self, session):
        return int(self._state.get(str(session), {}).get("last_seq", 0))

    def _seen_set(self, session):
        return set(self._state.get(str(session), {}).get("seen", []))

    def is_seen(self, session, item_id):
        return item_id in self._seen_set(session)

    def governing_origin(self, session):
        """The persisted origin of the most recent owner-role item observed for
        `session` (any forward_session walk, not just forwarded rows), or None if
        never observed (fresh state / a gap in the incremental window — e.g. an
        owner row whose created_ts ties the window's since-cutoff is excluded by
        amux-server.py's `created_ts>since_ts` filter and is never seen again).
        Callers must treat None as "not telegram" — fail-quiet, never fail-ring."""
        return self._state.get(str(session), {}).get("last_owner_origin")

    def observe_owner(self, session, origin):
        """Record the origin of an owner-role item as this session's new
        governing origin. Persisted (not just in-memory) so a sidecar restart
        doesn't lose an origin that has since scrolled out of the incremental
        /api/chat window (the window only grows once a reply is forwarded)."""
        st = self._state.setdefault(
            str(session), {"last_seq": 0, "seen": [], "last_owner_origin": None})
        st["last_owner_origin"] = origin or None

    def refetch_from(self, session, reported_cursor):
        """Given the cursor (max rowid_seq) the server just reported, return the
        `since` to (re)fetch with. If the server's max is BELOW our high-water, the
        cache was rebuilt + renumbered downward — refetch from 0 so we never stall;
        dedup-by-id then prevents re-forwarding."""
        last = self.fetch_since(session)
        if reported_cursor is not None and int(reported_cursor) < last:
            return 0
        return last

    def select(self, session, thread):
        """Pure: the ordered list of thread items to forward — role in
        {session, system}, not already forwarded — sorted in transcript order
        (ts, then seq)."""
        seen = self._seen_set(session)
        cand = [it for it in thread
                if it.get("role") in ("session", "system") and it.get("id") not in seen]
        cand.sort(key=_thread_order_key)
        return cand

    def mark_sent(self, session, item):
        st = self._state.setdefault(
            str(session), {"last_seq": 0, "seen": [], "last_owner_origin": None})
        iid = item.get("id")
        if iid and iid not in st["seen"]:
            st["seen"].append(iid)
            if len(st["seen"]) > self.SEEN_CAP:
                del st["seen"][:-self.SEEN_CAP]
        seq = item.get("seq")
        if isinstance(seq, int):
            st["last_seq"] = max(st.get("last_seq", 0), seq)

    def seed_baseline(self, session, thread, reported_cursor):
        """First time we see a session: mark all existing forwardable items as seen
        WITHOUT forwarding (no history flood on startup), and adopt the cursor.
        Deliberately does NOT seed last_owner_origin from pre-existing history —
        a freshly-onboarded session starts with an unknown governing origin (the
        documented fail-quiet default), same as any other gap."""
        st = self._state.setdefault(
            str(session), {"last_seq": 0, "seen": [], "last_owner_origin": None})
        for it in self.select(session, thread):
            self.mark_sent(session, it)
        if reported_cursor is not None:
            st["last_seq"] = max(st.get("last_seq", 0), int(reported_cursor))


# ── shared file helper ─────────────────────────────────────────────────────────
def _atomic_write_0600(path, text):
    tmp = path + ".tmp"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    try:
        os.write(fd, text.encode("utf-8"))
    finally:
        os.close(fd)
    os.replace(tmp, path)
    try:
        os.chmod(path, 0o600)
    except OSError:
        pass


# ── network: Telegram Bot API (injectable) ─────────────────────────────────────
class TelegramError(Exception):
    pass


class TelegramClient:
    def __init__(self, base, token, opener=None):
        self.base = base.rstrip("/")
        self.token = token
        self._opener = opener or urllib.request.build_opener()

    def _call(self, method, params, timeout):
        url = f"{self.base}/bot{self.token}/{method}"
        data = json.dumps(params).encode("utf-8")
        req = urllib.request.Request(
            url, data=data, headers={"Content-Type": "application/json"}, method="POST")
        try:
            with self._opener.open(req, timeout=timeout) as resp:
                body = json.loads(resp.read().decode("utf-8"))
        except urllib.error.HTTPError as e:
            try:
                body = json.loads(e.read().decode("utf-8"))
            except Exception:
                raise TelegramError(f"{method}: HTTP {e.code}")
        except (urllib.error.URLError, OSError, ValueError) as e:
            raise TelegramError(f"{method}: {e}")
        if not body.get("ok"):
            raise TelegramError(f"{method}: {body.get('description', body)}")
        return body.get("result")

    def get_updates(self, offset, timeout):
        # long-poll: server holds up to `timeout` s; give urllib a bit more headroom.
        # callback_query is included so inline permission-button taps are delivered
        # on the SAME monotonic offset as messages (plan B.4).
        return self._call("getUpdates",
                          {"offset": offset, "timeout": timeout,
                           "allowed_updates": ["message", "callback_query"]},
                          timeout=timeout + 10)

    # Telegram caps sendMessage at 4096 UTF-16 code units; stay under it with
    # margin and send long texts as ordered chunks (a >4096 reply otherwise
    # 400s forever and wedges the topic's in-order forward queue).
    _MSG_CHUNK = 3900

    def send_message(self, chat_id, text, topic_id=None, disable_notification=False,
                     reply_markup=None):
        """disable_notification is only included in the request when True — a
        ringing (default) send omits the key entirely rather than sending it
        as False. `params` is built once and reused for every chunk below, so a
        long, silently-forwarded reply stays silent across ALL of its chunks.
        reply_markup (an inline keyboard) is attached to the LAST chunk only, so
        a multi-chunk message shows its buttons under the final part. Returns the
        last chunk's result (its message_id identifies the button-bearing send)."""
        params = {"chat_id": chat_id, "disable_web_page_preview": True}
        if topic_id is not None:
            params["message_thread_id"] = int(topic_id)
        if disable_notification:
            params["disable_notification"] = True
        text = text or ""
        res = None
        starts = list(range(0, max(len(text), 1), self._MSG_CHUNK))
        for idx, i in enumerate(starts):
            chunk = text[i:i + self._MSG_CHUNK]
            if len(text) > self._MSG_CHUNK:
                part = i // self._MSG_CHUNK + 1
                total = (len(text) + self._MSG_CHUNK - 1) // self._MSG_CHUNK
                chunk = f"[{part}/{total}] " + chunk
            p = dict(params, text=chunk)
            if reply_markup is not None and idx == len(starts) - 1:
                p["reply_markup"] = reply_markup
            res = self._call("sendMessage", p, timeout=20)
        return res

    def edit_message_text(self, chat_id, message_id, text, reply_markup=None):
        """Rewrite a previously-sent message. Omitting reply_markup drops any
        inline keyboard (how a resolved/answered prompt loses its buttons)."""
        p = {"chat_id": chat_id, "message_id": int(message_id), "text": text,
             "disable_web_page_preview": True}
        if reply_markup is not None:
            p["reply_markup"] = reply_markup
        return self._call("editMessageText", p, timeout=20)

    def answer_callback(self, callback_id, text="", show_alert=False):
        """Acknowledge an inline-button tap (Telegram shows `text` as a toast;
        capped ~200 chars). Must be called within seconds or Telegram re-shows a
        spinner on the button."""
        p = {"callback_query_id": callback_id}
        if text:
            p["text"] = text[:200]
        if show_alert:
            p["show_alert"] = True
        return self._call("answerCallbackQuery", p, timeout=20)

    def send_chat_action(self, chat_id, action, topic_id=None):
        """A chat action ('typing') for the presence layer. Guaranteed silent —
        a chat action never notifies or badges. Auto-expires in <=5s (or when the
        bot next sends to the chat), so it must be re-sent to persist. Since Bot
        API 6.3 message_thread_id scopes it to a forum topic."""
        p = {"chat_id": chat_id, "action": action}
        if topic_id is not None:
            p["message_thread_id"] = int(topic_id)
        return self._call("sendChatAction", p, timeout=15)

    def set_message_reaction(self, chat_id, message_id, emoji):
        """React to a message (Bot API 7.0). Used ONLY for the opt-in 👀
        read-receipt (TG_PRESENCE_REACT=1). NOT guaranteed silent: a reaction to
        the owner's own message can ding depending on his client's
        Notifications->Reactions setting — hence opt-in, off by default. Pass an
        empty emoji to clear reactions."""
        reaction = [{"type": "emoji", "emoji": emoji}] if emoji else []
        return self._call("setMessageReaction",
                          {"chat_id": chat_id, "message_id": int(message_id),
                           "reaction": reaction}, timeout=15)

    def create_forum_topic(self, chat_id, name):
        res = self._call("createForumTopic",
                        {"chat_id": chat_id, "name": name[:128]}, timeout=20)
        return int(res["message_thread_id"])

    def get_me(self):
        return self._call("getMe", {}, timeout=20)


# ── network: amux localhost HTTP API (injectable) ──────────────────────────────
class AmuxError(Exception):
    pass


class AmuxClient:
    def __init__(self, base, write_token, opener=None, chat_base=None, auth_token=""):
        self.base = base.rstrip("/")
        # /api/chat moved OUT of the server at the rust cutover: upstream's server
        # has no such route (404), so it lives in the amux-chat.py sidecar. Every
        # other path still goes to the main server. Defaults to `base` so a
        # pre-cutover setup, where the python server still owned /api/chat, keeps
        # working with no config change.
        self.chat_base = (chat_base or base).rstrip("/")
        self.write_token = write_token
        # Rust's loopback auth bypass is off on this fork
        # (AMUX_RS_NO_LOOPBACK_BYPASS=1), so locality no longer authenticates and
        # READS need a token too — not just writes.
        self.auth_token = auth_token
        if opener is not None:
            self._opener = opener
        else:
            ctx = ssl.create_default_context()
            ctx.check_hostname = False
            ctx.verify_mode = ssl.CERT_NONE
            self._opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=ctx))

    def _call(self, method, path, params=None, body=None, timeout=20, retries=0):
        """One HTTP call to amux. `retries` re-attempts TRANSPORT failures only
        (never an HTTP status) with a short fixed backoff.

        Why this exists: 439 of 501 forward failures in a 19-day sample were the
        sidecar failing to reach the amux server — 231 read timeouts, 114
        connection-refused, 63 TLS handshake timeouts. None of them lost a message
        (the `since` cursor is not advanced on failure, so the next poll refetches),
        but each one dropped that session's whole forward for a cycle, and a
        connection-refused during a server restart would reliably fail every session
        in the sweep. One immediate retry converts most restart-window failures into
        a ~0.4s delay instead of a lost cycle.

        Deliberately NOT retried: any HTTP status. A 401/404/500 is an answer, and
        retrying it just multiplies load."""
        base = self.chat_base if path.startswith("/api/chat") else self.base
        url = base + path
        if params:
            url += "?" + urllib.parse.urlencode(params)
        headers = {}
        data = None
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if method not in ("GET", "HEAD"):
            headers["X-Amux-Write-Token"] = self.write_token
        # Sent on EVERY request, reads included: with the loopback bypass off a
        # bare GET is a 401, and the pre-cutover python server simply ignores a
        # header it does not use — so one shape works on both sides.
        if self.auth_token:
            headers["Authorization"] = "Bearer " + self.auth_token
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        attempt = 0
        while True:
            try:
                with self._opener.open(req, timeout=timeout) as resp:
                    raw = resp.read().decode("utf-8")
                    return resp.status, (json.loads(raw) if raw else {})
            except urllib.error.HTTPError as e:
                try:
                    payload = json.loads(e.read().decode("utf-8"))
                except Exception:
                    payload = {}
                return e.code, payload            # a status is an answer — never retry
            except (urllib.error.URLError, OSError, ValueError) as e:
                if attempt >= retries:
                    raise AmuxError(f"{method} {path}: {e}")
                attempt += 1
                log.info("%s %s transport error (%s) — retry %d/%d",
                         method, path, e, attempt, retries)
                time.sleep(0.4 * attempt)

    def health(self):
        code, body = self._call("GET", "/health", timeout=8)
        if code != 200:
            raise AmuxError(f"health {code}")
        return body

    def post_chat(self, session, text, origin="telegram", msg_id=""):
        """Owner input -> POST /api/chat. Raises AmuxError on any non-200 so the
        caller does NOT advance the offset (durable-ack ordering)."""
        code, body = self._call("POST", "/api/chat", body={
            "session": session, "text": text, "origin": origin, "id": msg_id})
        if code != 200:
            raise AmuxError(f"POST /api/chat -> {code}: {body}")
        return body

    def get_chat(self, session, since=0):
        # Poll-path call, once per session per ~2s sweep. The default 20s timeout
        # meant one unresponsive session stalled the ENTIRE sweep for 20s — 231
        # read timeouts in the sample, i.e. ~77 minutes of blocked polling. Failing
        # fast and retrying is strictly better here than waiting: the cursor is not
        # advanced, so nothing is lost either way.
        code, body = self._call("GET", "/api/chat",
                                params={"session": session, "since": since},
                                timeout=CHAT_TIMEOUT_SECS, retries=1)
        if code != 200:
            raise AmuxError(f"GET /api/chat -> {code}: {body}")
        return body

    def list_sessions(self):
        code, body = self._call("GET", "/api/sessions", timeout=30)
        if code != 200:
            raise AmuxError(f"GET /api/sessions -> {code}")
        return body

    def peek(self, session, lines=40):
        code, body = self._call(
            "GET", f"/api/sessions/{urllib.parse.quote(session)}/peek",
            params={"lines": lines})
        if code != 200:
            raise AmuxError(f"peek {session} -> {code}")
        return body.get("output") or body.get("live") or ""

    def wake(self, session):
        code, body = self._call(
            "POST", f"/api/sessions/{urllib.parse.quote(session)}/wake")
        if code != 200:
            raise AmuxError(f"wake {session} -> {code}: {body}")
        return body

    def create_session(self, name, directory=""):
        code, body = self._call("POST", "/api/sessions",
                                body={"name": name, "dir": directory})
        if code not in (200, 201):
            raise AmuxError(f"create {name} -> {code}: {body}")
        return body

    def raw_send(self, session, text):
        """Direct tmux-pane text injection via POST .../send with
        record_history=True — this disables the server's busy-deferral
        (steering queue) so the text lands immediately even while the
        session is generating or parked at a tool-approval/dialog picker
        (the whole point of /type: steering would otherwise hold it until a
        turn boundary that a stuck dialog never reaches). record_history is
        semantically right here too (a Telegram /type IS a deliberate owner
        action, same as a human typing in the dashboard).

        NOT deliver_now: live-verified against the running server that
        deliver_now=True WITHOUT record_history crashes the handler with
        "cannot access local variable '_origin'" (amux-server.py's
        POST .../send only initializes _origin inside its `if _defer_busy`
        branch, but references it again unconditionally afterward) — the
        text still lands before the crash, but the request 500s. Zero
        changes to amux-server.py are allowed, so record_history is the
        correct — and working — way to get immediate, non-deferred delivery.

        Confirmed against the handler (action == "send"): send_text()
        ALWAYS types the text then presses Enter to submit it — there is no
        "type without submitting" mode, so a caller must not assume
        otherwise."""
        code, body = self._call(
            "POST", f"/api/sessions/{urllib.parse.quote(session)}/send",
            body={"text": text, "record_history": True})
        if code != 200:
            raise AmuxError(f"raw_send {session} -> {code}: {body}")
        return body

    def send_key(self, session, key):
        """One raw tmux key name via POST .../keys (e.g. "Enter", "C-c",
        "Tab") — confirmed against the handler (action == "keys"): it reads a
        single 'keys' string and validates it against a fixed allow-list, so
        this call carries exactly one key per request. Send a sequence by
        calling this once per key, in order."""
        code, body = self._call(
            "POST", f"/api/sessions/{urllib.parse.quote(session)}/keys",
            body={"keys": key})
        if code != 200:
            raise AmuxError(f"send_key {session} {key!r} -> {code}: {body}")
        return body


# ── smart-mode summarizer: one-shot `claude -p` subprocess (injectable) ─────────
# Compresses a long session reply into a short Czech chat message using the
# OWNER's existing Claude Code plan — a one-shot `claude -p --model haiku`
# call, NOT the API (no API key involved). Runs in the outbound forward path;
# ANY failure falls back to deterministic brief truncation (never blocks or
# drops the reply).
#
# The child runs under launchd's near-empty env (see
# sidecars/com.amux.telegram.plist — only HOME+PATH are set), so PATH,
# CLAUDE_CONFIG_DIR and user identity are resolved explicitly here rather than
# inherited. Empirically verified on this machine: `env -i` with only
# HOME+PATH+CLAUDE_CONFIG_DIR set still fails "Not logged in" — macOS Keychain
# credential lookup additionally needs USER/LOGNAME populated.

SUMMARY_PROMPT = (
    "Následující text je odpověď AI kódovacího agenta z coding session. Shrň "
    "ji do KRÁTKÉ zprávy do chatu (2 až 4 věty, v češtině): co bylo uděláno, "
    "jaký je výsledek, a jestli něco blokuje nebo je potřeba rozhodnutí/"
    "pozornost majitele. Bez implementačních detailů, bez kódu, bez nadpisů "
    "a bez markdown formátování — piš to jako běžnou textovou zprávu člověku."
)

# Checked empirically on this machine: `claude` is a shell alias
# (--dangerously-skip-permissions), not a real PATH entry, and
# /usr/local/bin/claude does not exist — the native installer put the real
# binary at ~/.local/bin/claude. Try known install locations, falling back to
# bare "claude" so PATH resolution still works on a differently-laid-out host.
_CLAUDE_BIN_CANDIDATES = ("~/.local/bin/claude", "/usr/local/bin/claude", "/opt/homebrew/bin/claude")

# Priority order mirrors amux-server.py's account-routing convention
# (MODIFICATIONS.md "Account routing / multi-home"): first config home whose
# .claude.json shows a logged-in oauthAccount wins.
_SUMMARY_CONFIG_DIR_CANDIDATES = ("~/.claude-personal-2", "~/.claude-personal", "~/.claude")

_STDIN_HEAD = 8000
_STDIN_TAIL = 4000


def _resolve_claude_bin():
    for c in _CLAUDE_BIN_CANDIDATES:
        p = os.path.expanduser(c)
        if os.path.isfile(p) and os.access(p, os.X_OK):
            return p
    return "claude"


def _pick_summary_config_dir():
    for c in _SUMMARY_CONFIG_DIR_CANDIDATES:
        d = os.path.expanduser(c)
        try:
            with open(os.path.join(d, ".claude.json"), encoding="utf-8") as f:
                if json.loads(f.read()).get("oauthAccount"):
                    return d
        except (OSError, ValueError):
            continue
    return os.path.expanduser("~/.claude")


def _current_user():
    """OS username without relying on env vars — launchd's own env for this
    sidecar (sidecars/com.amux.telegram.plist) sets only HOME+PATH, so
    os.environ won't have USER/LOGNAME either; pwd resolves off the uid."""
    for key in ("USER", "LOGNAME"):
        v = os.environ.get(key)
        if v:
            return v
    try:
        import pwd
        return pwd.getpwuid(os.getuid()).pw_name
    except Exception:
        return ""


def _cap_stdin(text, head=_STDIN_HEAD, tail=_STDIN_TAIL):
    """Cap the piped reply at ~12k chars: first 8k + an elision marker + last
    4k when longer, so an oversized reply can't blow up the summarizer call."""
    if len(text) <= head + tail:
        return text
    return text[:head] + "\n…\n" + text[-tail:]


class Summarizer:
    """One-shot `claude -p` text summarizer (network-ish; injectable via
    `runner` for tests — never invoke a real subprocess in tests)."""

    def __init__(self, model="haiku", timeout=90.0, config_dir=None, claude_bin=None, runner=None):
        self.model = model
        self.timeout = timeout
        self.config_dir = os.path.expanduser(config_dir) if config_dir else _pick_summary_config_dir()
        self.claude_bin = claude_bin or _resolve_claude_bin()
        self._run = runner or subprocess.run

    def _env(self):
        home = os.path.expanduser("~")
        user = _current_user()
        claude_dir = (os.path.dirname(self.claude_bin) if os.path.isabs(self.claude_bin)
                      else os.path.join(home, ".local", "bin"))
        env = {
            "HOME": home,
            "USER": user,
            "LOGNAME": user,
            "PATH": f"{claude_dir}:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            "CLAUDE_CONFIG_DIR": self.config_dir,
        }
        tmpdir = os.environ.get("TMPDIR")
        if tmpdir:
            env["TMPDIR"] = tmpdir
        return env

    def summarize(self, text):
        """Return a short Czech summary, or None on ANY failure (caller falls
        back to brief truncation). Never raises."""
        try:
            proc = self._run(
                [self.claude_bin, "-p", SUMMARY_PROMPT, "--model", self.model],
                input=_cap_stdin(text), capture_output=True, text=True,
                timeout=self.timeout, env=self._env())
        except subprocess.TimeoutExpired:
            log.info("summarizer timed out after %ss — falling back to brief", self.timeout)
            return None
        except OSError as e:
            log.info("summarizer failed to start (%s) — falling back to brief", e)
            return None
        if proc.returncode != 0:
            log.info("summarizer exited %s — falling back to brief", proc.returncode)
            return None
        out = (proc.stdout or "").strip()
        if not out:
            log.info("summarizer produced empty output — falling back to brief")
            return None
        return out


# ── pure logic: session status label ───────────────────────────────────────────
def session_status_label(s):
    """Map an /api/sessions row to one of idle/active/waiting/limit."""
    if s.get("rate_limit_banner") or s.get("rate_limited_until") or s.get("credit_limited"):
        return "limit"
    st = s.get("status") or ""
    if st == "active":
        return "active"
    if st == "waiting":
        return "waiting"
    return "idle"


# ── pure logic: quiet-mode routing + presence predicates (plan M1 + quiet mode) ─
def notify_class(kind, is_final, ring_off, latch_armed, window_open,
                 origin_is_telegram, quiet=False, terminal=True):
    """Unified quiet-mode router (plan .omc/plans/telegram-quiet-mode.md, design
    A; amended by .omc/plans/chat-improvement.md C2). `kind` in
    {"reply", "question", "failure"}; returns "ring" | "live" | "suppress".
    /mute is handled upstream (is_muted) — it is absolute and never reaches here.

      * "suppress" — no Telegram footprint at all (dashboard + /last hold it).
      * "ring"     — a fresh, ringing sendMessage (disable_notification=False).
      * "live"     — a silent edit into the rolling live box. NOTE: the box's ONE
                     creation message is a real Telegram message and produces a
                     real (soundless) notification — Telegram's
                     `disable_notification` means "no sound", NOT "no
                     notification". Every later update is a free edit. Do not
                     read "live" as "invisible"; that misreading is what made
                     Jan report silent messages still showing up.

    `quiet` and `terminal` were APPENDED LAST, and that is load-bearing:
    tests/test_telegram_silent.py:564 calls this positionally with six arguments,
    so a new parameter anywhere earlier raises TypeError. Keep new inputs at the
    end with defaults that preserve the shipped truth table.

    Questions (Phase B prompts) are actionable and ALWAYS ring — they precede
    every gate, so neither /ring off nor /quiet can silence them (only /mute
    does, upstream). This is deliberate: a fleet-wide flag that could swallow a
    permission prompt is the one thing Principle 5 forbids.

    Failures: a NON-terminal failure (a rate-limit episode amux auto-answers
    itself, ethos D2) goes silently to the live box, which already shows "⛔
    limit" — it has no business making a sound. A TERMINAL failure still rings,
    unless the fleet-scope /quiet flag is on. That single cell is the entire
    behavioural footprint of /quiet.

    For a reply: the finality gate comes first (a non-final always suppresses);
    then /ring off diverts to the live box; then the answer-latch rings the
    answer to Jan's Telegram turn no matter how slow; then (design B only) an
    open window rings continuous mid-chat progress; else the quiet default sends
    the final silently to the live box. `quiet` deliberately does NOT gate a
    reply: under design A `window_open` is always False, so the only reachable
    ringing reply is `latch_armed` — the answer to Jan's own question — and
    silencing that is exactly the defect the latch was added to prevent."""
    if kind == "question":          # Phase B — actionable, never gated
        return "ring"
    if kind == "failure":
        if not terminal:            # self-resolving (rate limit) — box shows it
            return "live"
        return "live" if quiet else "ring"
    # kind == "reply"
    if not is_final:                # finality gate precedes everything (unchanged)
        return "suppress"
    if ring_off:                    # explicit per-session silence wins for replies
        return "live"
    if latch_armed:                 # THE answer to Jan's Telegram turn — any delay
        return "ring"
    if window_open and origin_is_telegram:   # design B: continuous mid-chat progress
        return "ring"
    return "live"                   # quiet default: routine final -> silent live box


def route_reply(is_final, origin_is_telegram, ring_off):
    """Legacy Option-B shim (plan .omc/plans/telegram-silent-updates.md): the
    pre-quiet decision core, preserved verbatim for regression. Equivalent to
    quiet-default OFF — latch off, window permanently open — so with
    (latch_armed=False, window_open=True) the reply branch reduces to
    `ring iff (¬ring_off ∧ origin_tg)`, exactly the shipped truth table."""
    return notify_class("reply", is_final, ring_off, latch_armed=False,
                        window_open=True, origin_is_telegram=origin_is_telegram)


def _thread_key_after(candidate_key, boundary_key):
    """True iff `candidate_key` strictly post-dates `boundary_key` in thread order.
    Keys are (ts, seq) — tuples fresh from _thread_order_key or lists after a JSON
    reload; both are normalized to list so a tuple-vs-list comparison never raises.
    A None boundary means "nothing known at arm time" -> any reply post-dates."""
    if boundary_key is None:
        return True
    return list(candidate_key) > list(boundary_key)


def should_type(status_label, origin_is_telegram):
    """The typing indicator (sendChatAction) fires ONLY while a session is
    actively working on a turn whose governing origin is telegram — i.e. Jan
    launched this from the phone and is waiting for THIS answer. Desk-origin
    active sessions get no typing (he's at the dashboard)."""
    return status_label == "active" and bool(origin_is_telegram)


_ELAPSED_THRESHOLDS = ((3600, "1h+"), (1800, "30m+"), (900, "15m+"), (300, "5m+"))


def elapsed_bucket(secs):
    """Coarsen an elapsed-seconds count for the status header. Returns "" for
    anything under five minutes.

    This used to bucket to 30s, which the docstring described as changing "at most
    every 30s" — but `(secs // 30) * 30 // 60` collapses to whole minutes past 60s,
    so it actually changed once a MINUTE, forever. Measured by replaying the old
    function over a 10-minute active hold at poll_secs=2.0: 11 distinct header
    strings, hence 11 in-place rewrites of a message Jan might be reading. That is
    the "awkward telegram existing message changes" complaint, and the churn was a
    TIMER — nothing happened between "▶ pracuje (2m)" and "▶ pracuje (3m)".

    Thresholds now GROW rather than tick: nothing at all below 5 minutes, then
    5m+/15m+/30m+/1h+. A typical turn finishes inside the first bucket and produces
    ZERO mid-turn edits; a 10-minute hold produces one; an hour produces four. The
    information that survives is the only part that was ever actionable — "this has
    been running a while" — and it now appears when that becomes true instead of
    being recomputed every minute.

    Returning "" (not "0s") is deliberate: the caller omits the parenthesis
    entirely, so the header reads "▶ pracuje" and does not imply a stalled timer."""
    secs = int(secs or 0)
    if secs < 0:
        secs = 0
    for cutoff, label in _ELAPSED_THRESHOLDS:
        if secs >= cutoff:
            return label
    return ""


def _text_hash(text):
    """Short stable hash of a live-box render — drives the no-op-edit skip."""
    return hashlib.sha256((text or "").encode("utf-8")).hexdigest()[:16]


# Live box: an edit can't be chunked, so hard-trim the body to <=3900 chars
# (independent of the topic's /mode — Hazard 3) with a /last hint. Typing is
# re-sent every ~4s (sendChatAction auto-expires in <=5s).
# The live box is a GLANCEABLE status surface, not a transcript. 3900 chars is
# roughly 60 lines on a phone — scrolling a status box is the "too verbose" half of
# Jan's complaint, and the full text is one /last away (the box says so). Kept in
# env rather than as a constant per ethos D4: a cap on what a human may see is a
# policy that belongs in config, not hardcoded where it silently becomes the
# ceiling. The hard Telegram limit is 4096; anything above that would fail the send.
LIVE_BODY_MAX = min(3900, int(os.environ.get("TG_LIVE_BODY_MAX", "1200") or 1200))
LIVE_TRIM_HINT = "\n… (/last = celý výpis)"
TYPING_INTERVAL_SECS = 4.0


def live_trim(text):
    """Hard-cap a live-box body at LIVE_BODY_MAX chars with a /last hint. An edit
    is a single message (never chunked), so anything longer is truncated, full
    text still reachable via /last (Hazard 3)."""
    text = text or ""
    if len(text) <= LIVE_BODY_MAX:
        return text
    return text[:LIVE_BODY_MAX - len(LIVE_TRIM_HINT)].rstrip() + LIVE_TRIM_HINT


# ── pure logic: continuous-idle settle tracker (finality) — plan M1 ────────────
class FinalityTracker:
    """Per-session timer of how long a session has been CONTINUOUSLY reported
    'idle' — the sole in-code signal a run has concluded (plan: a reply row
    carries no finality flag). Mirrors WaitingTracker exactly but for the 'idle'
    label: observe(session, label, now) once per poll; settled(session, now,
    settle) is True once it has been idle >= settle seconds without interruption.
    Any non-'idle' label (active/waiting/limit) resets the timer, so an
    autonomous loop whose idle gaps stay below `settle` never settles (no
    false-ring). In-memory only (rung_reply_id in LiveStore is the durable
    double-ring guard across restarts)."""

    def __init__(self):
        self._idle_since = {}   # session -> ts it most recently entered "idle"

    def observe(self, session, label, now):
        if label == "idle":
            self._idle_since.setdefault(str(session), now)
        else:
            self._idle_since.pop(str(session), None)

    def idle_since(self, session):
        return self._idle_since.get(str(session))

    def settled(self, session, now, settle):
        since = self._idle_since.get(str(session))
        return since is not None and (now - since) >= settle

    def clear(self, session):
        self._idle_since.pop(str(session), None)


# ── pure logic: format an outbound item for Telegram ───────────────────────────
def format_item(session, item):
    role = item.get("role")
    text = item.get("text") or ""
    if role == "system":
        return f"⚙️ [{session}] {text}"
    return text


# ── pure logic: display-mode formatting for outbound session replies ───────────
SHORT_REPLY_CHARS = 300   # replies shorter than this bypass mode processing entirely
BRIEF_CHARS = 600
SMART_PREFIX = "≡ "
SMART_SUFFIX = "\n(/last = celý výpis)"


def brief_truncate(text):
    """Deterministic 'brief' mode: truncate to ~600 chars + a line-count note.
    No AI involved — this is also the smart-mode fallback on any summarizer
    failure."""
    if len(text) <= BRIEF_CHARS:
        return text
    n_lines = text.count("\n") + 1
    return text[:BRIEF_CHARS].rstrip() + f"\n… ({n_lines} lines total)"


# ── pure logic: /last — locate the n-th most recent session reply ──────────────
def sorted_session_replies(thread):
    """role=='session' items from a merged thread, sorted in transcript order
    (ts, then seq) — used by /last to index from the most recent (index -n)."""
    items = [it for it in thread if it.get("role") == "session"]
    items.sort(key=_thread_order_key)
    return items


# ── pure logic: permission-prompt classification + fingerprint (plan B.1/B.4) ──
# A peeked pane of a session amux already reports as status=="waiting" is one of:
#   menu  — a `❯ N.` selector AND permission safety-chrome ("Do you want to
#           proceed", "don't ask again", an MCP "Allow X to Y") → Allow/Always/Deny.
#   open  — a menu without permission chrome (AskUserQuestion / plan approval) OR
#           a free-text question (trailing "?") → notify text-only, no menu buttons.
#   none  — no prompt signal, or a rate-limit menu (must NEVER get permission
#           buttons — a rate-limited session is labelled "limit", not "waiting",
#           but we defend here too).
_MENU_SELECTOR_RE = re.compile(r"❯\s*\d+\s*\.")
_MENU_OPTION_RE = re.compile(r"(?m)^\s*(?:❯\s*)?(\d+)\s*\.\s+\S")
_ALLOW_TO_RE = re.compile(r"\ballow\b.+?\bto\b", re.I)
_PROMPT_RATE_LIMIT_MARKERS = (
    "usage limit", "rate limit", "approaching your", "resets at", "reset at",
    "upgrade to", "out of credits", "credit balance", "usage will reset")


def _prompt_option_numbers(text):
    return sorted({int(m.group(1)) for m in _MENU_OPTION_RE.finditer(text or "")})


def _has_perm_chrome(text):
    # Permission-specific chrome ONLY. "Esc to cancel" + a ❯-menu is deliberately
    # NOT sufficient: the A.0 live capture confirmed AskUserQuestion also renders
    # "Esc to cancel" under a ❯-menu, so these markers must be unique to a
    # permission gate (all absent from AskUserQuestion): the proceed question,
    # the "Permission rule … requires confirmation" banner amux emits under
    # bypass, the /permissions hint, the "don't ask again" option, and an MCP
    # "Allow X to Y" header.
    low = (text or "").lower()
    return ("do you want to proceed" in low
            or "permission rule" in low
            or "requires confirmation" in low
            or "/permissions" in low
            or "don't ask again" in low
            or "dont ask again" in low
            or bool(_ALLOW_TO_RE.search(text or "")))


def classify_prompt(text):
    """Classify a waiting session's peeked pane text. Returns
    {"kind": "menu"|"open"|"none", "options": <int>, "always": <bool>}.
    `always` is True ONLY for a confirmed 3-option menu (an option numbered 3
    present) — on a 2-option prompt sending "2" selects the DENY choice, so an
    Always button there would be a mis-tap hazard (plan B.4).

    The peek endpoint returns the pane WITH ANSI color escapes (tmux capture -e),
    which land between the ❯ selector, option digits, and footer words — every
    marker regex missed until they are stripped (live miss, 2026-08-01)."""
    text = _ANSI_RE.sub("", text or "")
    low = text.lower()
    if any(m in low for m in _PROMPT_RATE_LIMIT_MARKERS):
        return {"kind": "none", "options": 0, "always": False}
    has_menu = bool(_MENU_SELECTOR_RE.search(text))
    if has_menu and _has_perm_chrome(text):
        opts = _prompt_option_numbers(text)
        return {"kind": "menu", "options": len(opts), "always": 3 in opts}
    if has_menu:
        return {"kind": "open", "options": 0, "always": False}
    for line in reversed(text.splitlines()):
        if line.strip():
            return {"kind": "open" if line.strip().endswith("?") else "none",
                    "options": 0, "always": False}
    return {"kind": "none", "options": 0, "always": False}


# fp = short hash of the prompt text with volatile bits (ANSI, clock timestamps,
# elapsed spinners, whitespace) normalized out, so the SAME prompt hashes stable
# across polls while a DISTINCT prompt hashes differently (the staleness guard).
_ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")
_CLOCK_RE = re.compile(r"\b\d{1,2}:\d{2}(?::\d{2})?\s*(?:[AaPp][Mm])?\b")
_ELAPSED_RE = re.compile(r"\(\s*\d+(?:\.\d+)?\s*[smh]\b[^)]*\)")
_WS_RE = re.compile(r"\s+")


def normalize_prompt(text):
    t = _ANSI_RE.sub("", text or "")
    t = _ELAPSED_RE.sub("", t)
    t = _CLOCK_RE.sub("", t)
    return _WS_RE.sub(" ", t).strip()


def prompt_fingerprint(text):
    return hashlib.sha256(normalize_prompt(text).encode("utf-8")).hexdigest()[:8]


def build_callback_data(action, session, fp):
    """`perm:<action>:<session>:<fp>`. Telegram caps callback_data at 64 bytes;
    amux session names are short so this stays well under it."""
    return f"perm:{action}:{session}:{fp}"


def parse_callback_data(data):
    """Inverse of build_callback_data → (action, session, fp), or None if the
    payload isn't a well-formed perm callback. fp is split off the tail first so
    a session name containing ':' still round-trips."""
    if not data or not data.startswith("perm:"):
        return None
    head, sep, fp = data.rpartition(":")
    if not sep or not fp:
        return None
    prefix, _, rest = head.partition(":")
    if prefix != "perm":
        return None
    action, _, session = rest.partition(":")
    if action not in ("allow", "always", "deny", "peek") or not session:
        return None
    return (action, session, fp)


def build_perm_keyboard(session, fp, shape):
    """Inline keyboard for a menu prompt: [✅ Allow] (+[✅ Always] on a 3-option
    menu) / [⛔ Deny] [👁 Peek]."""
    row1 = [{"text": "✅ Allow", "callback_data": build_callback_data("allow", session, fp)}]
    if shape.get("always"):
        row1.append({"text": "✅ Always",
                     "callback_data": build_callback_data("always", session, fp)})
    row2 = [{"text": "⛔ Deny", "callback_data": build_callback_data("deny", session, fp)},
            {"text": "👁 Peek", "callback_data": build_callback_data("peek", session, fp)}]
    return {"inline_keyboard": [row1, row2]}


def build_peek_keyboard(session, fp):
    """Peek-only keyboard for an open-ended prompt (no menu answer to inject)."""
    return {"inline_keyboard": [[
        {"text": "👁 Peek", "callback_data": build_callback_data("peek", session, fp)}]]}


def trim_prompt_text(text, max_lines=25, max_chars=PERM_PROMPT_MAX_CHARS):
    """The last few non-empty lines of a peek, capped — the relevant prompt tail
    for the notify body. ANSI-stripped: the raw peek carries color escapes that
    would render as garbage in a Telegram message."""
    lines = _ANSI_RE.sub("", text or "").splitlines()
    while lines and not lines[-1].strip():
        lines.pop()
    return "\n".join(lines[-max_lines:])[:max_chars]


# ── pure logic: continuous-waiting grace-window tracker (plan B.2) ──────────────
class WaitingTracker:
    """Per-session timer of how long a session has been CONTINUOUSLY "waiting".
    observe(session, label, now) is called once per poll with the session's
    current status label; due(session, now, grace) is True once it has been
    waiting ≥ grace seconds without interruption. Any non-"waiting" label resets
    the timer; re-entering "waiting" starts a fresh window. In-memory only (the
    persisted dedup lives in PromptStore)."""

    def __init__(self):
        self._since = {}   # session -> ts it most recently entered "waiting"

    def observe(self, session, label, now):
        if label == "waiting":
            self._since.setdefault(str(session), now)
        else:
            self._since.pop(str(session), None)

    def waiting_since(self, session):
        return self._since.get(str(session))

    def elapsed(self, session, now):
        since = self._since.get(str(session))
        return None if since is None else now - since

    def due(self, session, now, grace):
        el = self.elapsed(session, now)
        return el is not None and el >= grace

    def clear(self, session):
        self._since.pop(str(session), None)


def _short_hostname():
    try:
        return socket.gethostname().split(".")[0]
    except Exception:
        return "?"


# ── the bot (orchestration; network via injected clients) ──────────────────────
class Bot:
    def __init__(self, config, telegram, amux, topics, offset, outbound, summarizer=None,
                 prompts=None, live=None, counters=None):
        self.cfg = config
        self.default_mode = config.get("default_mode", "smart")
        self.summarizer = summarizer  # callable(text) -> str|None; None disables smart mode
        self.tg = telegram
        self.amux = amux
        self.topics = topics
        self.offset = offset
        self.outbound = outbound
        # Permission-prompt notify state (plan Phase B). prompts persists the
        # per-session pending prompt; waiting is the in-memory grace-window timer.
        self.prompts = prompts if prompts is not None else PromptStore(PROMPTS_PATH, {})
        self.waiting = WaitingTracker()
        # Silent-updates + presence state (plan M1). live persists the rolling
        # live box / presence surface; finality is the in-memory idle-settle
        # timer; the small dicts are per-poll presence bookkeeping (in-memory,
        # self-healing on restart).
        self.live = live if live is not None else LiveStore(LIVE_PATH, {})
        self.finality = FinalityTracker()
        self._candidate_items = {}   # session -> newest unpromoted reply item
        self._active_since = {}      # session -> ts it most recently went active
        self._last_typing = {}       # session -> ts of last sendChatAction
        self.machine = config.get("machine_label") or _short_hostname()
        self.chat_id = config.get("chat_id")
        self.owner_id = config["owner_id"]
        self._stop = threading.Event()
        self._save_lock = threading.Lock()
        # Instrumentation (plan chat-improvement.md C1). The counters are the
        # denominator that did not exist before; _decide is the single funnel
        # every notification class must pass through.
        # Injectable like prompts/live, and for the same reason: the default
        # loads (and SAVES to) the real ~/.amux path. Tests that construct a Bot
        # without overriding it write fabricated traffic into the live counters —
        # measured: one suite run moved reply:ring from 248 to 256 — which would
        # make /quiet status, the instrument this whole change exists to provide,
        # report numbers that never happened.
        self.counters = counters if counters is not None else CounterStore.load()
        self._last_observe = {}      # session -> last status_label logged (tg-observe)

    # ── instrumentation: the ONE place a notification class is chosen ──────────
    def _decide(self, session, kind, *, is_final=True, ring_off=False,
                latch_armed=False, window_open=False, origin_tg=False,
                terminal=True, rule=""):
        """Choose a notification class AND record the decision. Every send site
        routes through here.

        Why a funnel rather than calling notify_class directly: the reason
        `_send_prompt_notify` rang unconditionally for months — overriding
        /ring off, invisible to the router, its own docstring advertising the
        bypass — is that nothing forced a send to consult the router or to leave
        a trace. A site that forgets to call this leaves no `tg-decision` line,
        and C1's gate diffs the counters against those lines, so the omission is
        detectable instead of silent.

        `quiet` is read here rather than passed in, so no caller can forget it."""
        quiet = self.topics.is_quiet()
        klass = notify_class(kind, is_final, ring_off, latch_armed, window_open,
                             origin_tg, quiet, terminal)
        log.info("tg-decision ts=%d session=%s kind=%s is_final=%s ring_off=%s "
                 "latch_armed=%s window_open=%s origin_tg=%s quiet=%s terminal=%s "
                 "-> class=%s rule=%s",
                 int(time.time()), session, kind, is_final, ring_off, latch_armed,
                 window_open, origin_tg, quiet, terminal, klass, rule or kind)
        self.counters.bump(kind, klass)
        try:
            self.counters.save()
        except OSError as e:
            log.warning("counter save failed: %s", e)   # never block a send
        return klass

    # ── inbound ────────────────────────────────────────────────────────────────
    def handle_update(self, update):
        """Process one update. Returns True if the update is fully handled and the
        offset may advance. Raises AmuxError (durable-ack failure) to hold the offset
        so the update is re-delivered after recovery."""
        if not is_owner(update, self.owner_id):
            frm = (update_message(update).get("from") or {})
            log.warning("ignoring non-owner update from id=%s username=%s",
                        frm.get("id"), frm.get("username"))
            return True
        text = message_text(update)
        if not text:
            return True
        topic_id = message_topic_id(update)
        if text.startswith("//"):
            # "//" pass-through: forward a Claude Code slash command / OMC skill into
            # the mapped session by stripping exactly ONE leading slash (text[1:] —
            # never a whitespace-collapsing split, so internal spacing survives) and
            # riding the SAME chat pipeline as a plain message below. This check runs
            # BEFORE parse_command() so a single leading "/" still parses as a
            # sidecar bot command, never this path.
            forwarded = text[1:]
            if not forwarded[1:].strip():
                self._reply(topic_id, "usage: //<command> [args] (e.g. //ralph fix X)")
                return True
            text = forwarded
        else:
            cmd, args = parse_command(text)
            if cmd:
                self.handle_command(update, cmd, args)
                return True
        # Plain owner message (or a "//" pass-through, above) in a session topic ->
        # inject into that session.
        session = self.topics.session_for_topic(topic_id)
        if not session:
            self._reply(topic_id, "No session is mapped to this topic. "
                                  "Use /sessions to list, /wake <name> or /create <name>.")
            return True
        msg_id = derive_inbound_id(update["update_id"])
        res = self.amux.post_chat(session, text, origin="telegram", msg_id=msg_id)  # may raise
        if res.get("deduped"):
            log.info("re-delivered update %s -> %s (deduped)", update["update_id"], session)
        else:
            log.info("owner -> %s: %r", session, text[:80])
            # Presence (plan M1): a durable inject is the read-receipt trigger.
            # Create the live box once (silent) and show '👀 přečteno'. Guarded
            # against a re-delivery (deduped) so a re-fetch never re-creates.
            self._on_inject(session, (update_message(update).get("message_id")))
        return True

    # ── inbound: permission-prompt callbacks (plan B.4) ──────────────────────────
    def handle_callback(self, update):
        """Handle one inline-button tap. Self-contained — NEVER raises back into
        the offset loop (the loop advances the offset for callbacks regardless).
        Ordering is binding: the amux injection is the LAST fallible step, and
        everything after a successful injection is catch-all guarded so a
        post-inject failure can't cause a re-tap to double-inject (plan B.4.5)."""
        cq = callback_query(update)
        cb_id = cq.get("id")
        if not is_callback_owner(update, self.owner_id):
            log.warning("ignoring non-owner callback from id=%s",
                        (cq.get("from") or {}).get("id"))
            self._safe_answer(cb_id, "Not authorized")
            return
        parsed = parse_callback_data(cq.get("data") or "")
        if parsed is None:
            self._safe_answer(cb_id, "Unrecognized action")
            return
        action, session, fp = parsed
        message_id = (cq.get("message") or {}).get("message_id")
        if action == "peek":
            self._callback_peek(cb_id, session)
            return
        # Idempotency guard (first double-inject guard): a re-tap of an
        # already-answered prompt is a no-op even while the session is still
        # briefly waiting on the same (now-answered) pane before Claude advances.
        pending = self.prompts.get(session)
        if pending and pending.get("answered") and pending.get("fp") == fp:
            self._safe_answer(cb_id, "Already resolved")
            return
        # Freshness re-check (second double-inject guard): re-fetch status + peek;
        # a stale tap (session left waiting, or the prompt changed → fp mismatch)
        # resolves WITHOUT injecting.
        if not self._prompt_is_fresh(session, fp):
            self._safe_answer(cb_id, "Already resolved")
            self._edit_message(message_id, self._answered_body(session, message_id,
                                                               "✔️ Resolved elsewhere"))
            return
        # Inject (LAST fallible op). A failure here (e.g. session killed) resolves
        # the tap cleanly and leaves no half-written state.
        try:
            self._perm_inject(session, action)
        except AmuxError as e:
            log.warning("permission inject %s for %s failed: %s", action, session, e)
            self.waiting.clear(session)
            self.prompts.clear(session)
            self._save_prompts()
            self._safe_answer(cb_id, "Session no longer running")
            self._edit_message(message_id, self._answered_body(
                session, message_id, "⚠️ Session no longer running"))
            return
        # Injection succeeded — nothing below may raise into the offset loop.
        self._finalize_callback(cb_id, session, message_id, action)

    def _perm_inject(self, session, action):
        """Allow → send "1"; Always → send "2" (3-option menu only); Deny →
        Escape. Digits go via /send (proven by the server's yolo auto-responder,
        whose key allow-list has no digits); Escape is an allow-listed /keys."""
        if action == "allow":
            self.amux.raw_send(session, "1")
        elif action == "always":
            self.amux.raw_send(session, "2")
        elif action == "deny":
            self.amux.send_key(session, "Escape")

    def _finalize_callback(self, cb_id, session, message_id, action):
        """Post-injection wrap-up. Fully catch-all guarded (plan B.4.5). Keeps the
        pending entry (same fp) but marks it answered so the outbound loop neither
        re-notifies this prompt nor overwrites this "✅ …" edit when the session
        later leaves waiting."""
        stamp = time.strftime("%H:%M")
        label = {"allow": "✅ Allowed", "always": "✅ Always-allowed",
                 "deny": "⛔ Denied"}.get(action, action)
        # A permission tap is a Telegram inbound action: arm the answer-latch so
        # the session's next post-boundary final rings even while quiet.
        self._arm_latch(session)
        pending = self.prompts.get(session)
        try:
            if pending:
                self.prompts.mark_answered(session)
                self._save_prompts()
            self.waiting.clear(session)
        except Exception as e:
            log.warning("post-inject state update for %s failed: %s", session, e)
        self._safe_answer(cb_id, f"{label} {stamp}")
        body = (pending or {}).get("body") or f"🔐 {session}"
        self._edit_message(message_id, f"{body}\n\n{label} at {stamp}")

    def _prompt_is_fresh(self, session, fp):
        """True iff the session is STILL waiting AND the live prompt's fingerprint
        still matches `fp`. Any failure / mismatch → not fresh (fail-closed: no
        injection on doubt)."""
        try:
            rows = self.amux.list_sessions()
        except AmuxError:
            return False
        row = next((s for s in rows if s.get("name") == session), None)
        if row is None or session_status_label(row) != "waiting":
            return False
        try:
            text = self.amux.peek(session, lines=PERM_PEEK_LINES)
        except AmuxError:
            return False
        return prompt_fingerprint(text) == fp

    def _callback_peek(self, cb_id, session):
        """Peek button: no state change — toast an ack and post the current pane
        tail into the session topic (answerCallbackQuery text is too short for it).
        A peek is a Telegram inbound action (Jan looked), so it arms the
        answer-latch — its own arm site because a peek returns here BEFORE
        _finalize_callback (plan §1)."""
        self._arm_latch(session)
        try:
            out = self.amux.peek(session, lines=PERM_PEEK_LINES)
            tail = "\n".join(out.splitlines()[-15:]) or "(empty)"
        except AmuxError as e:
            tail = f"peek failed: {e}"
        self._safe_answer(cb_id, "Peek sent to topic")
        tid = self.topics.topic_for_session(session)
        if tid is not None:
            self._reply(tid, f"peek {session}:\n{tail[:3500]}")

    def _answered_body(self, session, message_id, note):
        pending = self.prompts.get(session)
        body = (pending or {}).get("body") if pending and pending.get("message_id") == message_id \
            else None
        return f"{body or ('🔐 ' + str(session))}\n\n{note}"

    def _safe_answer(self, cb_id, text):
        if not cb_id:
            return
        try:
            self.tg.answer_callback(cb_id, text)
        except TelegramError as e:
            log.info("answerCallbackQuery failed: %s", e)

    def _edit_message(self, message_id, text, reply_markup=None):
        """Rewrite a sent message. `reply_markup` MUST be passed when the message
        should keep its inline keyboard: Telegram drops the keyboard on any edit
        that omits it (see edit_message_text). That is load-bearing in both
        directions — _resolve_pending/_edit_superseded deliberately omit it so an
        answered prompt loses its now-dead buttons, while the C2b' drift-edit
        deliberately passes it so a still-live prompt keeps tap-to-answer. The
        prompt keyboard is the ENTIRE tap-to-answer affordance and it exists on no
        other message, so silently dropping it would leave Jan a notification he
        can only act on by opening the session."""
        if not message_id:
            return
        try:
            self.tg.edit_message_text(self.chat_id, message_id, text,
                                      reply_markup=reply_markup)
        except TelegramError as e:
            log.info("editMessageText failed: %s", e)

    def handle_command(self, update, cmd, args):
        topic_id = message_topic_id(update)
        # Log EVERY accepted command. Nothing here used to log at all except /type
        # and /keys, which meant "the log contains no /mute traffic" was a silent
        # probe: a grep that could not have produced a positive. During planning
        # that non-evidence was read as proof the mute controls had never been
        # used, and the conclusion was wrong. One line per command makes "did Jan's
        # command arrive?" answerable instead of unknowable. Args are logged for
        # routing commands only — never for /type, whose payload may be an OAuth
        # code or other secret (it logs its own length-only line).
        log.info("owner-cmd %s%s topic=%s", cmd,
                 "" if cmd in ("/type", "/keys") else (" " + " ".join(args) if args else ""),
                 topic_id)
        try:
            if cmd == "/sessions":
                self._reply(topic_id, self._render_sessions())
            elif cmd == "/peek":
                self._cmd_peek(update, topic_id, args)
            elif cmd in ("/wake", "/start"):
                self._cmd_wake(topic_id, args)
            elif cmd == "/create":
                self._cmd_create(topic_id, args)
            elif cmd == "/mute":
                self._cmd_mute(topic_id, True)
            elif cmd == "/unmute":
                self._cmd_mute(topic_id, False)
            elif cmd == "/ring":
                self._cmd_ring(topic_id, args)
            elif cmd == "/quiet":
                self._cmd_quiet(topic_id, args)
            elif cmd == "/type":
                self._cmd_type(update, topic_id)
            elif cmd == "/keys":
                self._cmd_keys(topic_id, args)
            elif cmd == "/mode":
                self._cmd_mode(topic_id, args)
            elif cmd == "/last":
                self._cmd_last(topic_id, args)
            else:
                self._reply(topic_id, self._help())
        except AmuxError as e:
            self._reply(topic_id, f"⚠️ amux error: {e}")

    def _render_sessions(self):
        try:
            rows = self.amux.list_sessions()
        except AmuxError as e:
            return f"⚠️ cannot list sessions: {e}"
        icon = {"idle": "⚪", "active": "\U0001f7e2", "waiting": "\U0001f7e1", "limit": "\U0001f534"}
        lines = []
        for s in rows:
            if s.get("archived"):
                continue
            label = session_status_label(s)
            lines.append(f"{icon.get(label, '⚪')} {s.get('name')} — {label}")
        return "\n".join(lines) if lines else "(no active sessions)"

    def _cmd_peek(self, update, topic_id, args):
        n = 40
        session = None
        for a in args:
            if a.isdigit():
                n = min(200, int(a))
            else:
                session = a
        if not session:
            session = self.topics.session_for_topic(topic_id)
        if not session:
            self._reply(topic_id, "usage: /peek [session] [N]")
            return
        out = self.amux.peek(session, lines=n)
        tail = "\n".join(out.splitlines()[-n:]) or "(empty)"
        self._reply(topic_id, f"peek {session} (last {n}):\n{tail[:3500]}")

    def _ensure_topic(self, session):
        tid = self.topics.topic_for_session(session)
        if tid is None:
            tid = self.tg.create_forum_topic(self.chat_id, session)
            self.topics.set(session, tid)
            self._save_topics()
        return tid

    def _cmd_wake(self, topic_id, args):
        if not args:
            self._reply(topic_id, "usage: /wake <session>")
            return
        session = args[0]
        self.amux.wake(session)
        tid = self._ensure_topic(session)
        self._reply(tid, f"woke {session}")

    def _cmd_create(self, topic_id, args):
        if not args:
            self._reply(topic_id, "usage: /create <session> [dir]")
            return
        session = args[0]
        directory = args[1] if len(args) > 1 else ""
        self.amux.create_session(session, directory)
        tid = self._ensure_topic(session)
        self._reply(tid, f"created {session}")

    def _cmd_mute(self, topic_id, mute):
        session = self.topics.session_for_topic(topic_id)
        if not session:
            self._reply(topic_id, "Run /mute inside a session topic.")
            return
        if mute:
            self.topics.mute(session)
        else:
            self.topics.unmute(session)
        self._save_topics()
        if mute:
            # Say what it costs. /mute is absolute — it suppresses content upstream
            # of every router branch, INCLUDING permission prompts. That makes it
            # the one control that can leave a session blocked with no Telegram
            # trace at all. Being silent about a session is fine; being silent
            # about having silenced it is not.
            self._reply(topic_id, f"muted {session}\n"
                        "⚠️ nedozvíš se ani to, že na tebe čeká (mute vypíná i dotazy). "
                        "Pro pouhé ztišení použij /ring off.")
        else:
            self._reply(topic_id, f"unmuted {session}")

    def _cmd_ring(self, topic_id, args):
        """/ring off forces disable_notification on every routine reply forward for
        this topic — a full mute-of-sound, distinct from /mute's content suppression
        (the reply still arrives, just silently). Questions and failures OVERRIDE
        ring_off and still ring (they precede the ring_off check in notify_class —
        plan telegram-quiet-mode.md §4). /ring on restores the quiet default
        (latch-armed answers ring; routine finals stay silent). Command responses
        (this ack included) always ring — only the reply-routing path reads this flag."""
        session = self.topics.session_for_topic(topic_id)
        if not session:
            self._reply(topic_id, "Run /ring inside a mapped session topic.")
            return
        val = args[0].strip().lower() if args else ""
        if val not in ("on", "off"):
            self._reply(topic_id, "usage: /ring on|off")
            return
        self.topics.set_ring_off(session, val == "off")
        self._save_topics()
        self._reply(topic_id, f"ring {val} for {session}")

    def _cmd_quiet(self, topic_id, args):
        """`/quiet on|off|status` — fleet-scope, and deliberately honest about how
        little it covers.

        It changes exactly ONE cell of the routing table: a TERMINAL failure (a
        credit limit — something no amount of waiting fixes) goes to the live box
        instead of ringing. Everything else is already silent or must stay loud:

          * `question` (permission prompts) is NEVER gated. Principle 5 — the cost
            of a spurious ping is annoyance, the cost of a missed prompt is a
            session stuck until Jan happens to look. Use /mute per session, or the
            grace period (TG_PERM_GRACE_SECS, now 90s), to make prompts quieter.
          * a rate/usage limit is already silent (it self-resolves — ethos D2).
          * a routine final reply is already silent (it edits the live box).
          * the only reply that rings is the answer-latch — the answer to Jan's own
            Telegram question — and silencing THAT is the exact defect the latch
            was added to prevent.

        `status` prints the counters, which are the point of the whole exercise:
        before they existed, "how many messages arrived and how many rang" could
        not be answered from anything the sidecar kept."""
        val = args[0].strip().lower() if args else "status"
        if val in ("on", "off"):
            self.topics.set_quiet(val == "on")
            self._save_topics()
            self._reply(topic_id, f"quiet {val} (fleet)\n\n{self._quiet_status()}")
            return
        if val != "status":
            self._reply(topic_id, "usage: /quiet on|off|status")
            return
        self._reply(topic_id, self._quiet_status())

    def _quiet_status(self):
        muted = sorted(s for s in self.topics.to_dict().get("muted") or [])
        ringoff = sorted(s for s in self.topics.to_dict().get("ring_off") or [])
        out = [
            f"🔕 fleet quiet: {'ON' if self.topics.is_quiet() else 'off'}",
            "   pokrývá POUZE terminální selhání (credit limit).",
            f"   dotazy (permission prompty) NIKDY neztlumí — na ty je /mute nebo "
            f"grace ({int(PERM_GRACE_SECS)}s).",
            f"🔇 /mute: {', '.join(muted) if muted else '—'}"
            + ("   ⚠️ u ztlumené session se NEDOZVÍŠ, že na tebe čeká" if muted else ""),
            f"🔈 /ring off: {', '.join(ringoff) if ringoff else '—'}",
            "",
            "📊 " + self.counters.render(),
        ]
        return "\n".join(out)

    def _cmd_type(self, update, topic_id):
        """Raw-inject text into the session's tmux pane, DELIBERATELY bypassing
        the /api/chat steering path (that queues until a turn boundary, which
        a stuck dialog/picker never reaches). Owner-only is already enforced
        by the caller. Never log the raw text — it may be an OAuth code or
        other secret — only its length."""
        session = self.topics.session_for_topic(topic_id)
        if not session:
            self._reply(topic_id, "Run /type inside a mapped session topic.")
            return
        text = command_raw_arg(message_text(update))
        if not text:
            self._reply(topic_id, "usage: /type <text>")
            return
        self.amux.raw_send(session, text)
        log.info("owner /type -> %s (%d chars)", session, len(text))
        self._reply(topic_id, "typed ✓")

    def _cmd_keys(self, topic_id, args):
        """Send one or more raw tmux key names in order (Enter, Up, Down,
        Escape, C-c, Tab, ...; the server validates each against its
        allow-list). DELIBERATELY bypasses steering, same rationale as /type."""
        session = self.topics.session_for_topic(topic_id)
        if not session:
            self._reply(topic_id, "Run /keys inside a mapped session topic.")
            return
        if not args:
            self._reply(topic_id, "usage: /keys <key> [key...] (e.g. Enter, C-c, Tab)")
            return
        for key in args:
            self.amux.send_key(session, key)
        log.info("owner /keys -> %s: %s", session, " ".join(args))
        self._reply(topic_id, "keys sent ✓")

    def _cmd_mode(self, topic_id, args):
        session = self.topics.session_for_topic(topic_id)
        if not session:
            self._reply(topic_id, "Run /mode inside a mapped session topic.")
            return
        if not args:
            current = self.topics.mode_for_session(session)
            if current:
                self._reply(topic_id, f"mode: {current}")
            else:
                self._reply(topic_id, f"mode: {self.default_mode} (default)")
            return
        mode = args[0].strip().lower()
        if mode not in VALID_MODES:
            self._reply(topic_id, "usage: /mode smart|brief|full")
            return
        self.topics.set_mode(session, mode)
        self._save_topics()
        self._reply(topic_id, f"mode set to {mode} for {session}")

    def _cmd_last(self, topic_id, args):
        session = self.topics.session_for_topic(topic_id)
        if not session:
            self._reply(topic_id, "Run /last inside a mapped session topic.")
            return
        n = 1
        if args:
            if not args[0].isdigit() or int(args[0]) < 1:
                self._reply(topic_id, "usage: /last [n]")
                return
            n = int(args[0])
        data = self.amux.get_chat(session, since=0)  # may raise AmuxError
        replies = sorted_session_replies(data.get("thread", []))
        if n > len(replies):
            self._reply(topic_id,
                        f"only {len(replies)} repl{'y' if len(replies) == 1 else 'ies'} available")
            return
        self.tg.send_message(self.chat_id, replies[-n].get("text") or "", topic_id)

    def _help(self):
        return ("commands:\n"
                "/sessions — list sessions + status\n"
                "/peek [session] [N] — last N lines\n"
                "/wake <session> — resume a session\n"
                "/create <session> [dir] — create a session\n"
                "/mute · /unmute — stop/resume ALL forwarding in this topic "
                "(absolute — you won't be told this session needs you either)\n"
                "/quiet on|off|status — fleet-wide. Covers only TERMINAL failures; "
                "permission prompts are never silenced. `status` shows the counters "
                "and which sessions are muted\n"
                "/ring on|off — force-silence routine reply forwards for this topic "
                "(questions + failures still ring; on restores the quiet default: ring the "
                "latch-armed answer to your Telegram turn, routine finals stay silent)\n"
                "/mode [smart|brief|full] — show or set this topic's reply display mode\n"
                "/last [n] — full text of the n-th most recent reply (default 1)\n"
                "/type <text> — raw-inject text into the pane (owner-only)\n"
                "/keys <key> [key...] — send raw keys, e.g. Enter, C-c, Tab (owner-only)\n"
                "//<cmd> — forward a slash command to the session (e.g. //ralph fix X)\n"
                "⚠️ /type and /keys bypass turn-boundary steering — they can interrupt "
                "a live turn, so use them only for dialogs/logins steering can't reach.")

    def _reply(self, topic_id, text):
        """Answer a command Jan just typed. DELIBERATELY EXEMPT from the router.

        This is the widest send site in the file — ~34 call sites fan into it — and
        it rings by default. That is correct and must not be "fixed": Jan typed a
        command a second ago and is looking at the screen, so the reply is the
        thing he is waiting for, not an interruption. Routing it through
        notify_class would put command acks under a fleet quiet flag and make the
        bot appear dead when he talks to it.

        The exemption is written down because C1's coverage gate enumerates every
        send site and asserts each one either consults _decide or is named here.
        An undocumented exemption is indistinguishable from the bypass that made
        _send_prompt_notify ring unconditionally for months."""
        try:
            self.tg.send_message(self.chat_id, text, topic_id)
        except TelegramError as e:
            log.warning("reply send failed: %s", e)

    def inbound_loop(self):
        backoff = 1.0
        while not self._stop.is_set():
            try:
                updates = self.tg.get_updates(self.offset.get(), self.cfg["long_poll_secs"])
                backoff = 1.0
            except TelegramError as e:
                log.warning("getUpdates failed: %s (backoff %.1fs)", e, backoff)
                self._sleep(backoff)
                backoff = min(60.0, backoff * 2)
                continue
            for u in updates or []:
                if "callback_query" in u:
                    # Callbacks are at-most-once by design (plan B.4): a dropped
                    # tap is recoverable (Jan re-taps; the fp re-check makes the
                    # retry safe), so a callback error must NEVER stall the shared
                    # offset the way an un-acked message does. Advance regardless.
                    try:
                        self.handle_callback(u)
                    except Exception as e:  # handle_callback is self-contained; belt-and-braces
                        log.exception("callback %s crashed: %s", u.get("update_id"), e)
                    self.offset.advance_to(u["update_id"])
                    continue
                try:
                    self.handle_update(u)
                except AmuxError as e:
                    # Durable-ack failure: do NOT advance past this update; retry it.
                    log.warning("amux not durable for update %s: %s — will re-deliver",
                                u.get("update_id"), e)
                    self._sleep(backoff)
                    backoff = min(60.0, backoff * 2)
                    break
                except Exception as e:  # never die on a single bad update
                    log.exception("update %s handler crashed: %s", u.get("update_id"), e)
                self.offset.advance_to(u["update_id"])

    # ── outbound ─────────────────────────────────────────────────────────────
    def forward_session(self, session, status_label=None):
        """Ingest new session-reply / system rows for one session and route them.
        Exactly-once via stable-id dedup; transcript order; rebuild-safe cursor.

        Two routing regimes, selected by `status_label`:

        * `status_label is None` (legacy / unit-test path, and a defensive fallback
          when the session status is somehow unknown): every forwardable
          session/system row is sent immediately, ringing per the governing-origin
          rule (docs/telegram-chat.md "Notifications") — the pre-finality behavior.
        * `status_label` provided (the outbound loop always passes it): silent-
          updates Option B (plan .omc/plans/telegram-silent-updates.md) + quiet
          mode (plan telegram-quiet-mode.md). SYSTEM rows are the FAILURE class —
          they ring regardless of origin / ring_off (only /mute silences, via the
          loop break), deduped against a usage-limit episode by the shared
          limit_rung key. SESSION reply rows are NOT sent here: each new one is
          deduped and recorded as the promotion candidate; the separate
          presence/promotion tail (`_presence_tail`, run EVERY poll — Hazard 1)
          decides suppress/live/ring once finality settles.

        Owner rows are walked on every poll (never marked "seen") to keep the
        governing origin current — amux-server.py's incremental window for them is
        ts-based, so the same row legitimately reappears until a later reply
        forwards past it; re-observing it is idempotent."""
        if self.topics.is_muted(session):
            return
        first_time = not self.outbound.known(session)
        since = self.outbound.fetch_since(session)
        data = self.amux.get_chat(session, since)
        cursor = data.get("cursor", 0)
        thread = data.get("thread", [])
        if cursor is not None and int(cursor) < since:
            # cache rebuilt + renumbered downward — refetch full, dedup-by-id guards.
            data = self.amux.get_chat(session, 0)
            cursor = data.get("cursor", 0)
            thread = data.get("thread", [])
        if first_time:
            # Don't flood the topic with pre-existing history on first sight.
            self.outbound.seed_baseline(session, thread, cursor)
            self._save_outbound()
            return
        ring_off = self.topics.is_ring_off(session)
        newest_reply = None
        for item in sorted(thread, key=_thread_order_key):
            if self.topics.is_muted(session):
                break
            role = item.get("role")
            if role == "owner":
                self.outbound.observe_owner(session, item.get("origin"))
                self._save_outbound()
                continue
            if role not in ("session", "system"):
                continue
            if self.outbound.is_seen(session, item.get("id")):
                continue
            if role == "session" and status_label is not None:
                # Option B: defer the send — dedup now, promote later via the tail.
                self.outbound.mark_sent(session, item)
                self._save_outbound()
                newest_reply = item
                continue
            # Legacy path (status unknown) + all system rows: immediate send.
            # _ensure_topic can raise (createForumTopic 429 under fleet load), and
            # it sits OUTSIDE the send's `except TelegramError` below — so the
            # exception escaped forward_session entirely and outbound_loop logged
            # "forward <s> crashed" with a full traceback, killing the rest of that
            # session's thread for the poll. 36 of those in 19 days, all on
            # 2026-07-31 when several topics were created at once.
            # Treat it like any other transient Telegram error: leave the item
            # un-marked and retry next poll.
            try:
                tid = self._ensure_topic(session)
            except TelegramError as e:
                log.warning("ensure_topic for %s failed: %s — retry next poll", session, e)
                break
            if role == "system" and status_label is not None:
                # Quiet mode: a system row is the FAILURE class — ring regardless
                # of origin / ring_off, unless this usage-limit episode already
                # rang (shared limit_rung dedup with the per-poll limit check).
                already = bool((self.live.get(session) or {}).get("limit_rung"))
                # A system row IS the self-resolving rate-limit episode (ethos D2:
                # amux answers that menu itself), so terminal=False routes it to the
                # live box, which already renders "⛔ limit". It has no business
                # making a sound. `already` still short-circuits the repeat.
                klass = "live" if already else self._decide(
                    session, "failure", ring_off=ring_off, terminal=False,
                    rule="system-row")
                silent = (klass != "ring")
            else:
                # LEGACY PATH ONLY (status_label is None). Unreachable in
                # production: session_status_label() returns a string on all four
                # branches and outbound_loop is forward_session's only caller, so it
                # always passes a label. It stays reachable from tests, which pin
                # the pre-finality behaviour as a regression contract.
                #
                # This used to hold its own inline rule —
                #   silent = ring_off or governing_origin(session) != "telegram"
                # — a FIFTH decision core beside the router, the shim, the prompt
                # notify and the (now-removed) hardcoded ring. It cost real time
                # during diagnosis: it reads like live routing logic, so it was
                # chased as the cause of a bug it could not possibly produce.
                #
                # It is not deleted, because the tests that reach it are a real
                # contract. Instead it now asks the router, which is PROVABLY the
                # same rule: with latch off and window open the reply branch reduces
                # to `ring iff (¬ring_off ∧ origin_tg)` — verified equal across all
                # four (ring_off, origin_tg) combinations. One decision core, one
                # tg-decision line, same behaviour.
                origin_tg = self.outbound.governing_origin(session) == "telegram"
                silent = self._decide(session, "reply", is_final=True,
                                      ring_off=ring_off, latch_armed=False,
                                      window_open=True, origin_tg=origin_tg,
                                      rule="legacy-no-status-label") != "ring"
            try:
                self.tg.send_message(self.chat_id, self._render_outbound(session, item), tid,
                                     disable_notification=silent)
            except TelegramError as e:
                log.warning("forward to %s failed: %s — retry next poll", session, e)
                break  # leave item un-marked; retried next poll
            self.outbound.mark_sent(session, item)
            self._save_outbound()
            if role == "system" and status_label is not None and not silent:
                # This failure ring owns the episode — set the shared key so the
                # per-poll limit-status check does not double-ring.
                self.live.set_fields(session, limit_rung=True)
                self._save_live()
        self._observe_latest_key(session, thread)
        if status_label is not None:
            if newest_reply is not None:
                self._record_candidate(session, newest_reply)
            self._presence_tail(session, status_label, time.time())

    def _observe_latest_key(self, session, thread):
        """Advance the session's persisted latest-known thread-order key from the
        current thread. This is the monotonic marker the answer-latch stamps as its
        arm boundary, so a just-recorded autonomous final can never post-date a
        later arm (post-dating guard)."""
        if not thread:
            return
        mx = max((_thread_order_key(it) for it in thread), default=None)
        if mx is None:
            return
        cur = (self.live.get(session) or {}).get("latest_key")
        if cur is None or list(mx) > list(cur):
            self.live.set_fields(session, latest_key=list(mx))
            self._save_live()

    def _render_outbound(self, session, item):
        """Render one outbound item per the session's display mode. System
        rows and short session replies are always forwarded verbatim; smart
        mode runs the reply through the summarizer with a brief-truncation
        fallback on ANY failure (never blocks or drops the reply).

        A reply's server-provided `summary` (docs/reply-summary.md — either the
        owner's own "⌁" marker or the server's background Haiku fill-in) is
        preferred over a local summarizer call: it's free (no extra `claude -p`
        subprocess) and, for a marker, has full session context the summarizer
        never sees. The local summarizer is only a fallback for items the server
        never summarized."""
        if item.get("role") == "system":
            return format_item(session, item)
        text = item.get("text") or ""
        if len(text) < SHORT_REPLY_CHARS:
            return text
        mode = self.topics.mode_for_session(session) or self.default_mode
        if mode == "full":
            return text
        if mode == "brief":
            return brief_truncate(text)
        # smart
        server_summary = (item.get("summary") or "").strip()
        if server_summary:
            return SMART_PREFIX + server_summary + SMART_SUFFIX
        summary = None
        if self.summarizer is not None:
            try:
                summary = self.summarizer(text)
            except Exception as e:
                log.info("summarizer raised for %s — falling back to brief: %s", session, e)
                summary = None
        if not summary:
            return brief_truncate(text)
        return SMART_PREFIX + summary + SMART_SUFFIX

    # ── silent updates + presence (plan .omc/plans/telegram-silent-updates.md, M1) ─
    def _presence_on(self):
        return bool(self.cfg.get("presence", True))

    def _presence_react(self):
        return bool(self.cfg.get("presence_react", False))

    def _settle_secs(self):
        return float(self.cfg.get("final_settle_secs", 4.0))

    def _save_live(self):
        with self._save_lock:
            self.live.save()

    def _quiet_default(self):
        """Quiet mode master switch (plan telegram-quiet-mode.md). Default ON.
        OFF forces the legacy route_reply path (latch off, window permanently
        open) — exact backward-compatible behavior."""
        return bool(self.cfg.get("quiet_default", True))

    def _arm_latch(self, session):
        """Arm the answer-latch on a Telegram inbound action (message, permission
        tap, peek). The boundary is the session's latest known thread-order key at
        arm time, so an in-flight autonomous final already recorded at-or-before it
        cannot consume the latch (post-dating guard) — only a strictly-later final,
        the real answer to Jan's turn, rings and clears it. Self-guarded: latch
        arming is core routing and must never raise into the inbound path, but it
        also must not depend on presence being on."""
        try:
            live = self.live.get(session) or {}
            self.live.set_fields(session, awaiting_tg_reply=True,
                                 latch_arm_key=live.get("latest_key"))
            self._save_live()
        except Exception as e:
            log.info("arm latch for %s failed: %s", session, e)

    def _on_inject(self, session, in_message_id):
        """Presence read-receipt on a durable telegram-origin inject (E1/E2).
        Best-effort — never raises into handle_update. Creates the live box once
        (silent, guarded by the persisted message_id so a re-inject never
        re-creates), stamps '👀 přečteno', and — only when TG_PRESENCE_REACT=1 —
        adds the opt-in 👀 reaction to Jan's own message.

        Also arms the answer-latch (quiet mode) FIRST — before the presence gate —
        so a Telegram inbound rings its slow answer even when presence is off."""
        self._arm_latch(session)
        if not self._presence_on():
            return
        try:
            now = time.time()
            hhmm = time.strftime("%H:%M", time.localtime(now))
            # New inbound turn: reset the idle-phase to read, drop any stale
            # active-elapsed clock so the header shows 👀, not a stale ✅/▶.
            self.live.set_fields(session, read_ts=hhmm, idle_phase="read")
            self._active_since.pop(session, None)
            if not (self.live.get(session) or {}).get("message_id"):
                self._live_create(session, self._live_render(session, None, "idle", now))
            else:
                self._save_live()
            if self._presence_react() and in_message_id:
                try:
                    self.tg.set_message_reaction(self.chat_id, in_message_id, "👀")
                except TelegramError as e:
                    log.info("presence reaction for %s failed (cosmetic): %s", session, e)
        except Exception as e:  # presence is best-effort; never break the inject
            log.info("presence on-inject for %s failed: %s", session, e)

    def _record_candidate(self, session, item):
        """Stash the newest unpromoted reply (in-memory for rendering, id
        persisted) so the promotion tail — which fires on a LATER, row-less poll
        once finality settles — knows what to promote (Hazard 1)."""
        self._candidate_items[session] = item
        self.live.set_fields(session, candidate_reply_id=item.get("id"))
        self._save_live()

    def _presence_tail(self, session, status_label, now):
        """Runs EVERY poll for a non-muted, known session (Hazard 1). Drives the
        idle-settle timer, promotes a settled candidate (ring or silent live
        edit), refreshes the status header (the ONE allowed live-box edit per
        poll), and re-sends the typing indicator for a telegram-origin active
        turn. Fully best-effort — a Telegram error skips this poll, retried next."""
        if self.topics.is_muted(session):
            return
        try:
            # tg-observe: one line per session per status TRANSITION (not per poll —
            # 22 sessions x 2s would be 40k lines/hour of noise). This is the second
            # corpus C1 needs: tg-decision records what the router DECIDED, which
            # cannot answer "how many times did the live box get edited for one
            # turn". Together they replay both the routing table and the edit churn.
            prev = self._last_observe.get(session)
            if prev != status_label:
                self._last_observe[session] = status_label
                log.info("tg-observe ts=%d session=%s status=%s prev=%s",
                         int(now), session, status_label, prev or "-")
            self.finality.observe(session, status_label, now)
            self._track_active(session, status_label, now)
            settled = self.finality.settled(session, now, self._settle_secs())
            live = self.live.get(session) or {}
            cand = live.get("candidate_reply_id")
            rung = live.get("rung_reply_id")
            did_edit = False
            if cand and cand != rung and settled:
                did_edit = self._promote_final(session, status_label, now, cand)
            # Status header — the single allowed live-box edit per poll — only if
            # a promotion didn't already spend it, and a box exists.
            if (self._presence_on() and not did_edit
                    and (self.live.get(session) or {}).get("message_id")):
                self._live_edit(session, self._live_render(session, None, status_label, now))
            # Typing indicator (telegram-origin active turns only).
            if self._presence_on() and should_type(
                    status_label, self.outbound.governing_origin(session) == "telegram"):
                self._maybe_type(session, now)
        except TelegramError as e:
            log.info("presence tail for %s skipped (%s) — retry next poll", session, e)
        except Exception as e:
            log.warning("presence tail for %s crashed: %s", session, e)

    def _promote_final(self, session, status_label, now, cand):
        """Promote a settled final candidate. Returns True iff it consumed the
        poll's live-box edit.

        Quiet mode (plan telegram-quiet-mode.md, design A): the answer-latch is
        EFFECTIVE only when it is armed AND this candidate strictly post-dates the
        arm boundary (post-dating guard — an in-flight autonomous final recorded
        at-or-before the boundary must not consume the latch meant for the real
        answer). An effective latch rings and CLEARS; a predating candidate routes
        by ordinary reply rules and leaves the latch armed. window_open is always
        False here (design A has no wall-clock window). With quiet OFF the latch is
        forced off and the window forced open -> exact legacy route_reply behavior."""
        item = self._candidate_items.get(session) or self._refetch_reply(session, cand)
        if item is None:
            return False
        origin_tg = self.outbound.governing_origin(session) == "telegram"
        ring_off = self.topics.is_ring_off(session)
        live = self.live.get(session) or {}
        if self._quiet_default():
            latch_effective = bool(live.get("awaiting_tg_reply")) and _thread_key_after(
                _thread_order_key(item), live.get("latch_arm_key"))
            window_open = False
        else:
            latch_effective = False
            window_open = True
        route = self._decide(session, "reply", is_final=True, ring_off=ring_off,
                             latch_armed=latch_effective, window_open=window_open,
                             origin_tg=origin_tg, rule="promote-final")
        if route == "ring":
            consumed = self._ring_final(session, item, cand, now)
            # Clear the latch only on an EFFECTIVE latched ring that actually sent
            # (rung_reply_id == cand is the durable success guard set by _ring_final).
            if latch_effective and (self.live.get(session) or {}).get("rung_reply_id") == cand:
                self.live.set_fields(session, awaiting_tg_reply=False, latch_arm_key=None)
                self._save_live()
            return consumed
        # "live": silent edit into the box (create if missing), mark rung.
        self._live_edit(session, self._live_render(session, item, status_label, now))
        self.live.set_fields(session, rung_reply_id=cand, idle_phase="done",
                             done_ts=time.strftime("%H:%M", time.localtime(now)))
        self._save_live()
        return True

    def _ring_final(self, session, item, cand, now):
        """Fresh ringing sendMessage for a telegram-origin final (the only case
        that produces a new/unread Telegram message). Then flip the live box (if
        any) to a settled breadcrumb so its silent content doesn't sit stale
        above the real answer that landed below (ordering, edge 3)."""
        tid = self._ensure_topic(session)
        try:
            self.tg.send_message(self.chat_id, self._render_outbound(session, item), tid,
                                 # literal by design: this function is only ever
                                 # reached when _decide already returned "ring"
                                 disable_notification=False)
        except TelegramError as e:
            log.warning("final ring for %s failed: %s — retry next poll", session, e)
            return False
        self.live.set_fields(session, rung_reply_id=cand, idle_phase="done",
                             done_ts=time.strftime("%H:%M", time.localtime(now)))
        self._save_live()
        if (self.live.get(session) or {}).get("message_id"):
            self._live_edit(session, self._live_breadcrumb(session, now))
            return True
        return False

    def _refetch_reply(self, session, reply_id):
        """Locate a reply item by stable id (used when the in-memory candidate was
        lost to a restart). Best-effort — returns None on any failure."""
        try:
            data = self.amux.get_chat(session, 0)
        except AmuxError:
            return None
        for it in data.get("thread", []):
            if it.get("role") == "session" and it.get("id") == reply_id:
                return it
        return None

    def _live_create(self, session, text):
        """Send the one silent creation message for a session's live box and
        persist its id (R1: one no-sound badge per session, ever)."""
        tid = self._ensure_topic(session)
        try:
            # literal by design: reached only when _decide returned "live". NOTE
            # this still produces a real (soundless) Telegram notification —
            # "live" means one badge then free edits, NOT invisible.
            res = self.tg.send_message(self.chat_id, text, tid, disable_notification=True)
        except TelegramError as e:
            log.warning("live-box create for %s failed: %s", session, e)
            return False
        self.live.set_fields(session, message_id=(res or {}).get("message_id"),
                             text_hash=_text_hash(text))
        self._save_live()
        return True

    def _live_edit(self, session, text):
        """Edit the live box to `text`, hash-guarded (skips a no-op edit, dodging
        `400 not-modified`). Its own wrapper (Hazard 2): a 'message to edit not
        found' recreates the box ONCE; any other error (429/5xx) is skipped and
        retried next poll (the ~2s poll cadence is the backoff). No box yet ->
        create. Returns True iff the box now reflects `text`."""
        live = self.live.get(session) or {}
        mid = live.get("message_id")
        if not mid:
            return self._live_create(session, text)
        if live.get("text_hash") == _text_hash(text):
            return True   # unchanged — skip the edit entirely
        try:
            self.tg.edit_message_text(self.chat_id, mid, text)
        except TelegramError as e:
            low = str(e).lower()
            if "message to edit not found" in low or "message can't be edited" in low:
                self.live.set_fields(session, message_id=None)
                return self._live_create(session, text)   # recreate once
            log.info("live-box edit for %s skipped (%s) — retry next poll", session, e)
            return False
        self.live.set_fields(session, text_hash=_text_hash(text))
        self._save_live()
        return True

    def _live_render(self, session, item, status_label, now):
        """The live box's full text = status header (presence on) + trimmed body.
        `item` None -> header-only refresh reusing the last stored body (N-a: a
        box created on inject with no reply yet renders the header alone, never an
        empty edit). Body is hard-trimmed to <=3900 regardless of /mode (Hazard 3)."""
        live = self.live.get(session) or {}
        if item is not None:
            body = live_trim(self._render_outbound(session, item))
            if body != live.get("body"):
                self.live.set_fields(session, body=body)
        else:
            body = live.get("body") or ""
        if not self._presence_on():
            return body
        header = self._presence_header_line(session, status_label, now)
        if not body:
            return header
        return f"{header}\n\n{body}"

    def _live_breadcrumb(self, session, now):
        live = self.live.get(session) or {}
        done = live.get("done_ts") or time.strftime("%H:%M", time.localtime(now))
        head = f"✅ hotovo {done}" if self._presence_on() else "✅"
        return f"{head}\n≡ viz odpověď níže"

    def _presence_header_line(self, session, status_label, now):
        """The status header's first line. States: ▶ pracuje (elapsed, 30s-bucketed)
        · ⏳ čeká na rozhodnutí · ⛔ limit · ✅ hotovo HH:MM · 👀 přečteno HH:MM.
        HH:MM stamps are fixed at event time (stored) so the string is stable
        across polls and the text_hash guard skips the intervening edits."""
        live = self.live.get(session) or {}
        if status_label == "active":
            el = elapsed_bucket(self._active_elapsed(session, now))
            return f"▶ pracuje ({el})" if el else "▶ pracuje"
        if status_label == "waiting":
            return "⏳ čeká na rozhodnutí"
        if status_label == "limit":
            return "⛔ limit"
        # idle
        if live.get("idle_phase") == "done":
            return f"✅ hotovo {live.get('done_ts') or ''}".rstrip()
        if live.get("idle_phase") == "read" or live.get("read_ts"):
            return f"👀 přečteno {live.get('read_ts') or ''}".rstrip()
        return "✅ hotovo"

    def _track_active(self, session, status_label, now):
        if status_label == "active":
            self._active_since.setdefault(session, now)
        else:
            self._active_since.pop(session, None)

    def _active_elapsed(self, session, now):
        since = self._active_since.get(session)
        return 0 if since is None else max(0, now - since)

    def _maybe_type(self, session, now):
        if now - self._last_typing.get(session, 0) < TYPING_INTERVAL_SECS:
            return
        tid = self.topics.topic_for_session(session)
        if tid is None:
            return
        try:
            self.tg.send_chat_action(self.chat_id, "typing", tid)
        except TelegramError as e:
            log.info("typing action for %s failed (cosmetic): %s", session, e)
        self._last_typing[session] = now

    def _attached_sessions(self):
        """Session names (prefix stripped) that currently have a tmux client
        attached — i.e. Jan is at the CLI. Returns an empty set when suppression
        is disabled or tmux can't be queried, so a failure never hides a prompt."""
        if not SUPPRESS_ATTACHED:
            return set()
        try:
            r = subprocess.run(
                ["tmux", "list-sessions", "-F", "#{session_name} #{session_attached}"],
                capture_output=True, text=True, timeout=5,
            )
            if r.returncode != 0:
                return set()
            return parse_attached_sessions(r.stdout)
        except Exception as e:
            log.debug("tmux attached-session probe failed (no suppression): %s", e)
            return set()

    # ── outbound: permission-prompt detection + notify (plan B.1/B.2/B.3/B.5) ────
    def _check_permission_prompts(self, sessions):
        """Once per poll: drive each session's continuous-waiting timer, notify
        when the grace window elapses on a fresh prompt, and clean up pending
        state when a session leaves waiting or disappears entirely."""
        now = time.time()
        attached = self._attached_sessions()
        live = set()
        for s in sessions:
            if s.get("archived"):
                continue
            name = s.get("name")
            if not name:
                continue
            live.add(name)
            label = session_status_label(s)
            self.waiting.observe(name, label, now)
            if label != "waiting":
                self._resolve_pending(name, "resolved")
                continue
            # Jan is attached at the CLI in this session — he sees the prompt live,
            # so don't ping his phone. Keep the waiting timer running above so that
            # if he detaches with the prompt still open, the next poll pings at once.
            if name in attached:
                continue
            if self.waiting.due(name, now, PERM_GRACE_SECS):
                self._maybe_notify_prompt(name, now)
        # A session that vanished from the list (killed/missing) is treated as
        # resolved — its prompt is no longer answerable.
        for name in self.prompts.pending_sessions():
            if name not in live:
                self.waiting.clear(name)
                self._resolve_pending(name, "gone")

    def _maybe_notify_prompt(self, session, now):
        """Peek + classify a due waiting session; notify ONCE per waiting episode.
        Honors /mute (suppresses entirely).

        Per-episode, not per-fingerprint (changed 2026-08-18, plan C2b'). The old
        rule keyed the dedup on `prompt_fingerprint`, so a session whose prompt
        TEXT shifted while it stayed blocked — a picker redrawing, a diff scrolling,
        a retry re-rendering the same question — sent a fresh ringing message each
        time and marked the previous one "superseded". Jan's phone buzzed N times
        for one block. But a prompt whose text moved is still the SAME block: he is
        needed exactly once.

        So a changed fingerprint now EDITS the existing message in place, keeping
        its tap-to-answer keyboard, and does not ring again. A genuinely new episode
        — the session left `waiting` and came back — clears `prompts` via
        _resolve_pending and rings normally."""
        if self.topics.is_muted(session):
            return
        try:
            text = self.amux.peek(session, lines=PERM_PEEK_LINES)
        except AmuxError as e:
            log.warning("peek %s for permission notify failed: %s", session, e)
            return
        shape = classify_prompt(text)
        if shape["kind"] == "none":
            return
        fp = prompt_fingerprint(text)
        pending = self.prompts.get(session)
        if pending and pending.get("fp") == fp:
            return  # same prompt still standing — already notified
        body = self._format_prompt_notify(session, text, shape)
        if pending and not pending.get("answered"):
            # Same episode, drifted text: refresh in place, do NOT ring again.
            kb = build_perm_keyboard(session, fp, shape) if shape["kind"] == "menu" \
                else build_peek_keyboard(session, fp)
            self._decide(session, "question", ring_off=self.topics.is_ring_off(session),
                         rule="prompt-drift-edit")
            self._edit_message(pending.get("message_id"), body, reply_markup=kb)
            self.prompts.set(session, fp, pending.get("message_id"), pending.get("ts") or now,
                             shape["kind"], body=body)
            self._save_prompts()
            return
        msg_id = self._send_prompt_notify(session, body, shape, fp)
        if msg_id is not None:
            self.prompts.set(session, fp, msg_id, now, shape["kind"], body=body)
            self._save_prompts()

    def _format_prompt_notify(self, session, text, shape):
        if shape["kind"] == "menu":
            header = f"🔐 {session} @ {self.machine} — permission decision"
            hint = ""
        else:
            header = f"❓ {session} @ {self.machine} — waiting on you"
            hint = "\n\nReply with /type <text> to answer."
        return f"{header}\n\n{trim_prompt_text(text)}{hint}"

    def _send_prompt_notify(self, session, body, shape, fp):
        """Send the notify to the session topic. Returns the message_id or None.

        A permission decision is actionable, so this still RINGS — `question`
        returns "ring" from every branch of notify_class, ahead of /ring off and
        ahead of /quiet. What changed 2026-08-18 (plan chat-improvement.md C2a) is
        that it now ASKS the router instead of asserting the answer.

        The old version held a literal `disable_notification=False` and its own
        docstring advertised that it "overrides /ring off". That made it a fourth
        decision core outside the router: invisible to the counters, unaffected by
        any control, and the reason "/ring off doesn't work" was a true report
        about the loudest class in the fleet. Behaviour here is deliberately
        unchanged — the fix is that the decision is now made in one place and
        leaves a `tg-decision` line, so the next person can SEE that questions
        ring rather than having to read this function to find out.

        /mute is still honored upstream in _maybe_notify_prompt, and so is the
        attached-CLI skip in _check_permission_prompts."""
        tid = self._ensure_topic(session)
        kb = build_perm_keyboard(session, fp, shape) if shape["kind"] == "menu" \
            else build_peek_keyboard(session, fp)
        klass = self._decide(session, "question", ring_off=self.topics.is_ring_off(session),
                             rule="permission-prompt")
        try:
            res = self.tg.send_message(self.chat_id, body, tid,
                                       disable_notification=(klass != "ring"),
                                       reply_markup=kb)
        except TelegramError as e:
            log.warning("permission notify for %s failed: %s", session, e)
            return None
        return (res or {}).get("message_id")

    def _resolve_pending(self, session, reason):
        """Clear a pending prompt whose session left waiting / disappeared. If it
        was answered via a callback the message already shows the outcome — leave
        it; otherwise annotate it as resolved."""
        pending = self.prompts.get(session)
        if not pending:
            return
        self.prompts.clear(session)
        self._save_prompts()
        if pending.get("answered"):
            return
        note = {"gone": "✔️ Session ended — prompt no longer active.",
                "resolved": "✔️ Resolved (session continued)."}.get(reason, "✔️ Resolved.")
        self._edit_message(pending.get("message_id"),
                           f"{pending.get('body') or ('🔐 ' + str(session))}\n\n{note}")

    def _edit_superseded(self, pending):
        self._edit_message(pending.get("message_id"),
                           f"{pending.get('body') or '🔐'}\n\n♻️ Superseded by a newer prompt.")

    # ── outbound: usage-limit failure ring (plan telegram-quiet-mode.md §3) ──────
    def _check_limit_rings(self, sessions):
        """Once per poll: ring EXACTLY once when a session transitions into `limit`
        status (usage/credit/rate limit), and clear the shared episode key when it
        leaves. This check lives OUTSIDE forward_session's per-item loop, so it does
        NOT inherit that loop's /mute break — it MUST call is_muted itself so a muted
        flaky session's limit never rings (plan §3). Deduped against the usage-limit
        system row via the shared limit_rung LiveStore key (first-writer-wins in the
        single-threaded poll) -> one ring per limit episode regardless of path order."""
        for s in sessions:
            if s.get("archived"):
                continue
            name = s.get("name")
            if not name:
                continue
            label = session_status_label(s)
            already = bool((self.live.get(name) or {}).get("limit_rung"))
            if label == "limit":
                if self.topics.is_muted(name):
                    continue  # explicit mute guard (this loop has no is_muted break)
                if already:
                    continue  # this episode already rang
                # Rate limit vs CREDIT limit are not the same event, and treating
                # them alike is the bug this split fixes. A rate/usage limit
                # self-resolves — amux answers that menu itself (ethos D2) and the
                # live box already renders "⛔ limit", so buzzing the phone tells
                # Jan about something that fixes itself. A CREDIT limit does not
                # self-resolve: it needs a human to act, so it stays terminal and
                # still rings (unless the fleet /quiet flag is on).
                terminal = bool(s.get("credit_limited"))
                klass = self._decide(name, "failure",
                                     ring_off=self.topics.is_ring_off(name),
                                     terminal=terminal,
                                     rule="credit-limit" if terminal else "usage-limit")
                if klass == "ring" and self._ring_failure(
                        name, f"⛔ {name} @ {self.machine} — usage limit reached"):
                    self.live.set_fields(name, limit_rung=True)
                    self._save_live()
                elif klass != "ring":
                    # Claim the episode even though nothing rang, so the next poll
                    # doesn't re-decide (and re-log) the same limit every 2s.
                    # Safe ordering note: forward_session runs BEFORE this check and
                    # now only sets limit_rung when its system row actually RANG —
                    # which, being non-terminal, it no longer does. So this check is
                    # the sole owner of the episode and a credit limit cannot be
                    # pre-empted into silence by the system row that precedes it.
                    self.live.set_fields(name, limit_rung=True)
                    self._save_live()
            elif already:
                # Left `limit` — re-arm the next episode.
                self.live.set_fields(name, limit_rung=False)
                self._save_live()

    def _ring_failure(self, session, text):
        """Fresh ringing sendMessage for a failure (usage limit). Returns True on a
        successful send so the caller can set the shared dedup key; a send failure
        leaves the key unset so the ring is retried next poll (one ring guaranteed)."""
        tid = self._ensure_topic(session)
        try:
            # literal by design: reached only when _decide returned "ring".
            self.tg.send_message(self.chat_id, text, tid, disable_notification=False)
            return True
        except TelegramError as e:
            log.warning("limit ring for %s failed: %s — retry next poll", session, e)
            return False

    def outbound_loop(self):
        backoff = self.cfg["poll_secs"]
        while not self._stop.is_set():
            try:
                sessions = self.amux.list_sessions()
                backoff = self.cfg["poll_secs"]
            except AmuxError as e:
                log.warning("list_sessions failed: %s (amux restarting?) backoff %.1fs", e, backoff)
                self._sleep(backoff)
                backoff = min(30.0, backoff * 2)
                continue
            for s in sessions:
                if self._stop.is_set():
                    break
                if s.get("archived"):
                    continue
                try:
                    self.forward_session(s.get("name"), status_label=session_status_label(s))
                except AmuxError as e:
                    log.warning("forward %s failed: %s", s.get("name"), e)
                except Exception as e:
                    log.exception("forward %s crashed: %s", s.get("name"), e)
            try:
                self._check_limit_rings(sessions)
            except Exception as e:
                log.exception("limit-ring check crashed: %s", e)
            try:
                self._check_permission_prompts(sessions)
            except Exception as e:
                log.exception("permission-prompt check crashed: %s", e)
            self._sleep(self.cfg["poll_secs"])

    # ── persistence (guarded) ────────────────────────────────────────────────
    def _save_topics(self):
        with self._save_lock:
            self.topics.save()

    def _save_outbound(self):
        with self._save_lock:
            self.outbound.save()

    def _save_prompts(self):
        with self._save_lock:
            self.prompts.save()

    def _sleep(self, secs):
        self._stop.wait(secs)

    def stop(self):
        self._stop.set()

    def run(self):
        log.info("amux-telegram starting (owner=%s chat=%s amux=%s tg=%s)",
                 self.owner_id, self.chat_id, self.cfg["amux_base"], self.cfg["tg_api_base"])
        try:
            self.tg.get_me()
        except TelegramError as e:
            log.error("Telegram getMe failed — check TG_BOT_TOKEN: %s", e)
            return 1
        if not self.chat_id:
            log.error("TG_CHAT_ID is not set — cannot create/forward to a forum group.")
            return 1
        t_in = threading.Thread(target=self.inbound_loop, name="inbound", daemon=True)
        t_out = threading.Thread(target=self.outbound_loop, name="outbound", daemon=True)
        t_in.start()
        t_out.start()
        try:
            while not self._stop.is_set():
                time.sleep(0.5)
        except KeyboardInterrupt:
            self.stop()
        return 0


def build_bot(config):
    telegram = TelegramClient(config["tg_api_base"], config["bot_token"])
    amux = AmuxClient(config["amux_base"], config["write_token"],
                      chat_base=config.get("chat_base"),
                      auth_token=config.get("auth_token", ""))
    topics = TopicStore.load()
    offset = OffsetStore.load()
    outbound = OutboundTracker.load()
    prompts = PromptStore.load()
    live = LiveStore.load()
    summarizer = Summarizer(model=config["summary_model"], timeout=config["summary_timeout"],
                            config_dir=config.get("summary_config_dir"))
    return Bot(config, telegram, amux, topics, offset, outbound,
               summarizer=summarizer.summarize, prompts=prompts, live=live)


def main():
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s")
    try:
        config = load_config()
    except ConfigError as e:
        sys.stderr.write("amux-telegram: " + str(e) + "\n")
        return 2
    return build_bot(config).run()


if __name__ == "__main__":
    sys.exit(main())
