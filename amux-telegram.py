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
import json
import logging
import os
import ssl
import stat
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

DEFAULT_TG_API_BASE = "https://api.telegram.org"
DEFAULT_AMUX_BASE = "https://localhost:8822"


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
    return {
        "bot_token": token,
        "owner_id": owner_id,
        "chat_id": chat_id,
        "tg_api_base": (get("TG_API_BASE") or DEFAULT_TG_API_BASE).rstrip("/"),
        "amux_base": (get("AMUX_BASE") or DEFAULT_AMUX_BASE).rstrip("/"),
        "write_token": write_token,
        "poll_secs": float(get("TG_POLL_SECS", "2.0") or 2.0),
        "long_poll_secs": int(get("TG_LONG_POLL_SECS", "25") or 25),
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


# ── pure logic: topic <-> session mapping (persisted) ──────────────────────────
class TopicStore:
    """session <-> forum-topic-id map + per-session mute set. Pure logic; JSON persist."""

    def __init__(self, path=TOPICS_PATH, state=None):
        self.path = path
        state = state or {}
        # session -> topic_id
        self._topics = {str(k): int(v) for k, v in (state.get("topics") or {}).items()}
        self._muted = set(str(s) for s in (state.get("muted") or []))

    @classmethod
    def load(cls, path=TOPICS_PATH):
        try:
            with open(path, encoding="utf-8") as f:
                return cls(path, json.load(f))
        except (OSError, ValueError):
            return cls(path, {})

    def to_dict(self):
        return {"topics": dict(self._topics), "muted": sorted(self._muted)}

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


# ── pure logic: outbound cursor + stable-id dedup (persisted) ──────────────────
class OutboundTracker:
    """Per-session outbound forwarding state: a rowid_seq high-water cursor (fetch
    optimization) AND the set of stable reply ids already forwarded (the real
    exactly-once key). Dedup-by-id makes forwarding rebuild-safe: if a cache rebuild
    renumbers rowid_seq below our cursor we refetch from 0 (no stall) and the seen-id
    set prevents re-flooding (C-crit-2)."""

    SEEN_CAP = 2000

    def __init__(self, path=OUTBOUND_PATH, state=None):
        self.path = path
        self._state = {}
        for sess, st in (state or {}).items():
            self._state[str(sess)] = {
                "last_seq": int(st.get("last_seq", 0)),
                "seen": list(st.get("seen", [])),
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
        st = self._state.get(str(session), {})
        seen = set(st.get("seen", []))
        cand = [it for it in thread
                if it.get("role") in ("session", "system") and it.get("id") not in seen]
        cand.sort(key=lambda x: (x.get("ts") or 0,
                                 x.get("seq") if x.get("seq") is not None else -1))
        return cand

    def mark_sent(self, session, item):
        st = self._state.setdefault(str(session), {"last_seq": 0, "seen": []})
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
        WITHOUT forwarding (no history flood on startup), and adopt the cursor."""
        st = self._state.setdefault(str(session), {"last_seq": 0, "seen": []})
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
        return self._call("getUpdates",
                          {"offset": offset, "timeout": timeout,
                           "allowed_updates": ["message"]},
                          timeout=timeout + 10)

    def send_message(self, chat_id, text, topic_id=None):
        params = {"chat_id": chat_id, "text": text, "disable_web_page_preview": True}
        if topic_id is not None:
            params["message_thread_id"] = int(topic_id)
        return self._call("sendMessage", params, timeout=20)

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


# ── the bot (orchestration; network via injected clients) ──────────────────────
class Bot:
    def __init__(self, config, telegram, amux, topics, offset, outbound):
        self.cfg = config
        self.tg = telegram
        self.amux = amux
        self.topics = topics
        self.offset = offset
        self.outbound = outbound
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
        cmd, args = parse_command(text)
        if cmd:
            self.handle_command(update, cmd, args)
            return True
        # Plain owner message in a session topic -> inject into that session.
        topic_id = message_topic_id(update)
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

    def _help(self):
        return ("commands:\n"
                "/sessions — list sessions + status\n"
                "/peek [session] [N] — last N lines\n"
                "/wake <session> — resume a session\n"
                "/create <session> [dir] — create a session\n"
                "/mute · /unmute — stop/resume forwarding in this topic")

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
        Exactly-once via stable-id dedup; transcript order; rebuild-safe cursor."""
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
        for item in self.outbound.select(session, thread):
            if self.topics.is_muted(session):
                break
            tid = self._ensure_topic(session)
            try:
                self.tg.send_message(self.chat_id, format_item(session, item), tid)
            except TelegramError as e:
                log.warning("forward to %s failed: %s — retry next poll", session, e)
                break  # leave item un-marked; retried next poll
            self.outbound.mark_sent(session, item)
            self._save_outbound()

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
            self._sleep(self.cfg["poll_secs"])

    # ── persistence (guarded) ────────────────────────────────────────────────
    def _save_topics(self):
        with self._save_lock:
            self.topics.save()

    def _save_outbound(self):
        with self._save_lock:
            self.outbound.save()

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
    return Bot(config, telegram, amux, topics, offset, outbound)


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
