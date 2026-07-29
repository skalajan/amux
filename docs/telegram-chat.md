# Telegram session chat (amux-telegram sidecar)

`amux-telegram.py` is a standalone, stdlib-only **sidecar** that bridges a private
Telegram forum supergroup to amux's session-chat core (Scope B3). It makes **zero**
changes to `amux-server.py` — it only talks to the running server over its localhost
HTTP API, so it tracks upstream cleanly and is crash-isolated from the dashboard.

```
Telegram  --getUpdates long-poll-->  amux-telegram  --POST /api/chat-->   session (steering)
session reply --> chat_replies --> GET /api/chat?since= --> amux-telegram --sendMessage--> topic
```

- **One forum topic per session.** The sidecar maps a forum topic ⇄ a session and
  forwards that session's replies + system events into its topic. Your message in a
  session's topic is injected into that session on its next turn boundary.
- **Owner-only.** Messages from any Telegram user other than `TG_OWNER_ID` are ignored
  and logged.
- **Durable + exactly-once.** The long-poll offset advances only after amux durably
  persists your message (idempotent by Telegram `update_id`, so a crash re-delivers
  harmlessly). Outbound replies are deduped by their stable id, so a sidecar restart
  never re-floods or stalls a topic.

This is a **separate bot** from any "agents" system bot — use a fresh BotFather bot.

---

## 1. Create the bot (BotFather)

1. In Telegram, open [@BotFather](https://t.me/BotFather) → `/newbot`.
2. Pick a name and username. BotFather returns a **token** like
   `123456789:AAExxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx` — this is `TG_BOT_TOKEN`.
3. Disable group privacy so the bot can read topic messages: BotFather → `/mybots`
   → your bot → *Bot Settings* → *Group Privacy* → **Turn off**.

## 2. Find your numeric user id (`TG_OWNER_ID`)

Message [@userinfobot](https://t.me/userinfobot) (or [@RawDataBot](https://t.me/RawDataBot));
it replies with your numeric `id`. That number is the **only** account allowed to drive
the sidecar.

## 3. Create the forum supergroup (`TG_CHAT_ID`)

1. Create a **new group**, then in *Group Settings* enable **Topics** (this turns it
   into a forum supergroup — required; the sidecar creates one topic per session).
2. Add your bot to the group and make it an **admin** with *Manage Topics* permission
   (needed for `createForumTopic`).
3. Get the group's id: send any message in the group, then open
   `https://api.telegram.org/bot<TG_BOT_TOKEN>/getUpdates` in a browser and read
   `result[].message.chat.id` — a forum supergroup id looks like `-1001234567890`.
   That is `TG_CHAT_ID`.

## 4. Write the config — `~/.amux/telegram.env` (0600)

```
TG_BOT_TOKEN=123456789:AAExxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
TG_OWNER_ID=11111111
TG_CHAT_ID=-1001234567890
```

```bash
touch ~/.amux/telegram.env
chmod 600 ~/.amux/telegram.env
# ...paste the three lines above...
```

The sidecar **refuses to start** if the file is missing, group/other-readable, or
missing a required key. It reads the amux write token from `~/.amux/write_token`
(created by the server) to authenticate its writes — no extra step needed.

Optional overrides (env or file): `TG_API_BASE` (default `https://api.telegram.org`),
`AMUX_BASE` (default `https://localhost:8822`), `TG_POLL_SECS` (outbound poll cadence,
default `2.0`), `TG_LONG_POLL_SECS` (inbound long-poll hold, default `25`).

## 5. Run it

Foreground (to check it connects):

```bash
python3 amux-telegram.py
```

As a LaunchAgent (auto-start at login, KeepAlive — mirrors `com.amux.serve`):

```bash
cp sidecars/com.amux.telegram.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.amux.telegram.plist
launchctl enable  gui/$(id -u)/com.amux.telegram
# logs: ~/.amux/logs/telegram.out.log  ~/.amux/logs/telegram.err.log
```

Edit the two absolute paths in the plist if your checkout is not at
`/Users/janskala/Desktop/Projects/amux`.

To stop / reload:

```bash
launchctl bootout gui/$(id -u)/com.amux.telegram
```

---

## Commands (owner-only, sent in the group)

| command | effect |
|---|---|
| `/sessions` | list sessions with status (⚪ idle · 🟢 active · 🟡 waiting · 🔴 limit) |
| `/peek [session] [N]` | last N lines of a session (defaults to the current topic's session, N=40) |
| `/wake <session>` | resume a session and ensure it has a topic |
| `/create <session> [dir]` | create a session and ensure it has a topic |
| `/mute` · `/unmute` | stop / resume forwarding replies into the current topic |
| `/type <text>` | raw-inject text into the session's tmux pane, bypassing steering |
| `/keys <key> [key...]` | send raw key names (e.g. `Enter`, `C-c`, `Tab`, `Up`, `Down`, `Escape`), bypassing steering |

Anything else prints a short help. A plain (non-command) message inside a session's
topic is injected into that session.

**`/type` and `/keys` bypass turn-boundary steering on purpose.** Every other
inbound message (and command) is delivered through `/api/chat`, which queues at
the next turn boundary — safe, but it never arrives while a session is sitting
at a tool-approval prompt, a login/dialog picker, or otherwise "waiting", since
those states never reach a turn boundary on their own. `/type` and `/keys` call
the session's `send`/`keys` endpoints directly with immediate delivery, so they
land right away — including while a turn is live. Use them only when steering
genuinely can't reach the target (dialogs, logins); a real Claude Code auth
prompt is exactly the case. Note: `/type` always submits with Enter after
typing (the server's send path has no "type without submitting" mode) — the
follow-up `/keys Enter` in the recipe below is a safety net for any additional
prompt, not required to submit the typed text itself.

### Remote re-login (expired Claude session)

When a session's Claude Code login has expired and it's stuck at an OAuth
prompt, drive the re-login from your phone:

1. `/peek 30` — see the pending OAuth URL in the last 30 lines.
2. Open the URL in a browser, sign in, and copy the code Claude gives you.
3. `/type <code>` — types the code into the session's pane (and submits it).
4. `/keys Enter` — in case a follow-up confirmation is waiting.

## State files (`~/.amux/`, all 0600, never in git)

- `telegram.env` — your config (above).
- `telegram-topics.json` — session ⇄ topic map + muted set.
- `telegram-offset` — inbound long-poll offset (advanced only after a durable amux ack).
- `telegram-outbound.json` — per-session outbound cursor + forwarded stable-ids (dedup).

## Notes

- **amux restarts are normal.** The server self-restarts (`os.execv`) on file save;
  the sidecar treats dropped connections as expected and reconnects with backoff.
- **Read/write auth.** `GET /api/chat` is read-loose; the sidecar's writes carry the
  `X-Amux-Write-Token` from `~/.amux/write_token` (Scope A). If writes 401, confirm
  that file exists and the server is current.
- **Governance.** The sidecar is a standalone file upstream doesn't track (no in-file
  delta, no sentinel, no registry row) — see `.claude/rules/extend-via-sidecar.md`.
