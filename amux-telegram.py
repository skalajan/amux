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

# Permission-prompt notify (plan .omc/plans/telegram-permissions.md Phase B).
# Only ping once a session has been CONTINUOUSLY "waiting" this long — a debounce
# longer than the server's yolo auto-responder cooldown, so recognized/auto-
# answerable prompts self-clear and never reach Jan (plan B.2).
PERM_GRACE_SECS = float(os.environ.get("TG_PERM_GRACE_SECS", "10") or 10)
PERM_PEEK_LINES = 40
PERM_PROMPT_MAX_CHARS = 1500

DEFAULT_TG_API_BASE = "https://api.telegram.org"
DEFAULT_AMUX_BASE = "https://localhost:8822"

# Per-topic display mode for outbound session replies. "smart" is the DEFAULT
# for every topic (new and pre-existing) unless overridden by /mode or
# TG_DEFAULT_MODE.
VALID_MODES = ("smart", "brief", "full")


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
        "write_token": write_token,
        "poll_secs": float(get("TG_POLL_SECS", "2.0") or 2.0),
        "long_poll_secs": int(get("TG_LONG_POLL_SECS", "25") or 25),
        "default_mode": default_mode,
        "summary_model": (get("TG_SUMMARY_MODEL") or "haiku").strip(),
        "summary_timeout": float(get("TG_SUMMARY_TIMEOUT", "90") or 90),
        "summary_config_dir": (get("TG_SUMMARY_CONFIG_DIR") or "").strip() or None,
        "machine_label": (get("TG_MACHINE_LABEL") or "").strip() or None,
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

    @classmethod
    def load(cls, path=TOPICS_PATH):
        try:
            with open(path, encoding="utf-8") as f:
                return cls(path, json.load(f))
        except (OSError, ValueError):
            return cls(path, {})

    def to_dict(self):
        return {"topics": dict(self._topics), "muted": sorted(self._muted),
                "modes": dict(self._modes), "ring_off": sorted(self._ring_off)}

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
    def __init__(self, base, write_token, opener=None):
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
        code, body = self._call("GET", "/api/chat",
                                params={"session": session, "since": since})
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
                 prompts=None):
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
        self.machine = config.get("machine_label") or _short_hostname()
        self.chat_id = config.get("chat_id")
        self.owner_id = config["owner_id"]
        self._stop = threading.Event()
        self._save_lock = threading.Lock()

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
        tail into the session topic (answerCallbackQuery text is too short for it)."""
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

    def _edit_message(self, message_id, text):
        if not message_id:
            return
        try:
            self.tg.edit_message_text(self.chat_id, message_id, text)
        except TelegramError as e:
            log.info("editMessageText failed: %s", e)

    def handle_command(self, update, cmd, args):
        topic_id = message_topic_id(update)
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
        self._reply(topic_id, f"{'muted' if mute else 'unmuted'} {session}")

    def _cmd_ring(self, topic_id, args):
        """/ring off forces disable_notification on EVERY forwarded reply for
        this topic regardless of governing origin — a full mute-of-sound,
        distinct from /mute's content suppression (the reply still arrives,
        just silently). /ring on restores the origin rule (docs/telegram-chat.md
        "Notifications"). Command responses (this ack included) always ring —
        only the outbound reply-forward path in forward_session reads this flag."""
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
                "/mute · /unmute — stop/resume forwarding in this topic\n"
                "/ring on|off — force-silence every forward regardless of origin "
                "(on restores the default: ring only for replies to a Telegram-originated message)\n"
                "/mode [smart|brief|full] — show or set this topic's reply display mode\n"
                "/last [n] — full text of the n-th most recent reply (default 1)\n"
                "/type <text> — raw-inject text into the pane (owner-only)\n"
                "/keys <key> [key...] — send raw keys, e.g. Enter, C-c, Tab (owner-only)\n"
                "//<cmd> — forward a slash command to the session (e.g. //ralph fix X)\n"
                "⚠️ /type and /keys bypass turn-boundary steering — they can interrupt "
                "a live turn, so use them only for dialogs/logins steering can't reach.")

    def _reply(self, topic_id, text):
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
    def forward_session(self, session):
        """Forward any new session-reply / system rows for one session to its topic.
        Exactly-once via stable-id dedup; transcript order; rebuild-safe cursor.

        Notification routing (docs/telegram-chat.md "Notifications"): walks the
        FULL merged thread — owner rows included, not just the forwardable ones —
        in transcript order. Every owner row updates the session's persisted
        "governing origin"; every forwardable session/system row rings only when
        the CURRENT governing origin is exactly "telegram" (i.e. it answers a
        Telegram-originated owner message). Everything else — dashboard/other-
        session origin, no preceding owner message at all, system rows — is sent
        with disable_notification=True. /ring off forces silent unconditionally,
        overriding the origin rule. Owner rows are walked on every poll (never
        marked "seen") because amux-server.py's incremental window for them is
        ts-based (created_ts > since_ts of the last forwarded reply), not
        id-based — the same row legitimately reappears until a later reply
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
            tid = self._ensure_topic(session)
            silent = ring_off or self.outbound.governing_origin(session) != "telegram"
            try:
                self.tg.send_message(self.chat_id, self._render_outbound(session, item), tid,
                                     disable_notification=silent)
            except TelegramError as e:
                log.warning("forward to %s failed: %s — retry next poll", session, e)
                break  # leave item un-marked; retried next poll
            self.outbound.mark_sent(session, item)
            self._save_outbound()

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

    # ── outbound: permission-prompt detection + notify (plan B.1/B.2/B.3/B.5) ────
    def _check_permission_prompts(self, sessions):
        """Once per poll: drive each session's continuous-waiting timer, notify
        when the grace window elapses on a fresh prompt, and clean up pending
        state when a session leaves waiting or disappears entirely."""
        now = time.time()
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
            if self.waiting.due(name, now, PERM_GRACE_SECS):
                self._maybe_notify_prompt(name, now)
        # A session that vanished from the list (killed/missing) is treated as
        # resolved — its prompt is no longer answerable.
        for name in self.prompts.pending_sessions():
            if name not in live:
                self.waiting.clear(name)
                self._resolve_pending(name, "gone")

    def _maybe_notify_prompt(self, session, now):
        """Peek + classify a due waiting session; notify on a new fingerprint,
        supersede the old message on a changed one, no-op while the fp is
        unchanged (dedup). Honors /mute (suppresses entirely)."""
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
        if pending and not pending.get("answered"):
            self._edit_superseded(pending)
        body = self._format_prompt_notify(session, text, shape)
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
        """Send the notify to the session topic. RINGS unconditionally (a
        permission decision is actionable — plan B.3, overrides /ring off; /mute
        is already honored upstream). Returns the message_id or None."""
        tid = self._ensure_topic(session)
        kb = build_perm_keyboard(session, fp, shape) if shape["kind"] == "menu" \
            else build_peek_keyboard(session, fp)
        try:
            res = self.tg.send_message(self.chat_id, body, tid,
                                       disable_notification=False, reply_markup=kb)
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
                    self.forward_session(s.get("name"))
                except AmuxError as e:
                    log.warning("forward %s failed: %s", s.get("name"), e)
                except Exception as e:
                    log.exception("forward %s crashed: %s", s.get("name"), e)
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
    amux = AmuxClient(config["amux_base"], config["write_token"])
    topics = TopicStore.load()
    offset = OffsetStore.load()
    outbound = OutboundTracker.load()
    prompts = PromptStore.load()
    summarizer = Summarizer(model=config["summary_model"], timeout=config["summary_timeout"],
                            config_dir=config.get("summary_config_dir"))
    return Bot(config, telegram, amux, topics, offset, outbound,
               summarizer=summarizer.summarize, prompts=prompts)


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
