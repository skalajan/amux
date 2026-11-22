//! Fork: one Telegram forum topic per worker (owner, 2026-11-22). Off unless
//! `AMUX_TELEGRAM_FORUM_CHAT=<forum supergroup id>` is set in server.env.
//!
//! - A topic is a chat bridge link: conversation `<chat>:<thread>` to `w:<worker>`
//!   in `bridge_links`, so delivery, history and replies are the bridge's own.
//! - Inbound (`handle_update`, called first by `telegram_poll`): only the owner
//!   (`AMUX_BRIDGE_OWNERS`) is heard in the forum; a topic message goes to its
//!   worker as a Messages post, `/link <worker>` in a topic repoints it.
//! - Outbound: posts in `w:<worker>` reach the topic through the bridge;
//!   `on_turn` mirrors a turn that did not come from Messages (silently), and
//!   `notify` copies the worker's push notifications into its topic.
//! - Lifecycle (`spawn`): every minute, a worker without a topic gets one.
//!
//! Verdicts: tg_topics_forum_seen, tg_topics_not_owner, tg_topics_general_ignored, tg_topics_inbound,
//! tg_topics_unlinked_topic, tg_topic_created, tg_topic_create_failed,
//! tg_topics_tick, tg_topics_mirrored, tg_topics_send_failed.

use crate::api::AppState;
use crate::bridge::{self, Inbound, Outcome};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const PLATFORM: &str = "telegram";
const JOB: &str = "telegram-topics";
const TICK_SECS: u64 = 60;
/// Telegram throttles a group at about 20 messages a minute; topic creation counts.
const CREATE_PER_TICK: usize = 5;
const CHUNK_CHARS: usize = 3900;
const MAX_CHUNKS: usize = 4;
const SEEN_TURNS_CAP: usize = 500;

pub fn forum_chat() -> Option<i64> {
    bridge::setting("AMUX_TELEGRAM_FORUM_CHAT")?.parse().ok()
}

pub fn topic_key(chat: i64, thread: i64) -> String {
    format!("{chat}:{thread}")
}

/// A bridge conversation key that names a forum topic, as (chat, thread).
pub fn split_key(conversation: &str) -> Option<(i64, i64)> {
    let (c, t) = conversation.split_once(':')?;
    Some((c.parse().ok()?, t.parse().ok()?))
}

/// The bridge's Telegram send: a topic key becomes chat_id plus message_thread_id.
pub fn target(payload: &mut Value, conversation: &str) {
    if let Some((chat, thread)) = split_key(conversation) {
        payload["chat_id"] = json!(chat);
        payload["message_thread_id"] = json!(thread);
    }
}

/// `/link@botname proceram` (what Telegram sends from a group's command menu) to `/link proceram`.
fn strip_bot_suffix(text: &str) -> String {
    if !text.starts_with('/') {
        return text.to_string();
    }
    let (cmd, rest) = text.split_once(' ').map(|(c, r)| (c, Some(r))).unwrap_or((text, None));
    let cmd = cmd.split('@').next().unwrap_or(cmd);
    match rest {
        Some(r) => format!("{cmd} {r}"),
        None => cmd.to_string(),
    }
}

#[derive(Debug)]
enum Route {
    NotForum,
    NotOwner(String),
    Ignored(&'static str),
    Topic(Inbound),
}

fn route(update: &Value, forum: i64, is_owner: impl Fn(&str) -> bool) -> Route {
    let Some(msg) = update.get("message") else { return Route::NotForum };
    if msg["chat"]["id"].as_i64() != Some(forum) {
        return Route::NotForum;
    }
    let sender = msg["from"]["id"].as_i64().map(|i| i.to_string()).unwrap_or_default();
    if !is_owner(&sender) {
        return Route::NotOwner(sender);
    }
    let text = msg["text"].as_str().unwrap_or("").trim();
    if text.is_empty() {
        return Route::Ignored("no text");
    }
    let thread = if msg["is_topic_message"] == true { msg["message_thread_id"].as_i64() } else { None };
    let Some(thread) = thread else { return Route::Ignored("general topic") };
    let from = &msg["from"];
    Route::Topic(Inbound {
        platform: PLATFORM.into(),
        conversation: topic_key(forum, thread),
        is_group: true,
        sender_id: sender,
        sender_name: from["username"].as_str().or(from["first_name"].as_str()).unwrap_or("").to_string(),
        text: strip_bot_suffix(text),
        platform_msg_id: format!("{forum}:{}", msg["message_id"].as_i64().unwrap_or(0)),
    })
}

/// True when the update belongs to the forum chat and was handled here, so the
/// one-chat `/link` flow in `telegram_poll` never sees it.
pub async fn handle_update(state: &AppState, update: &Value) -> bool {
    let chat = &update["message"]["chat"];
    let forum = forum_chat();
    if chat["is_forum"] == true && chat["id"].as_i64() != forum {
        tracing::info!(chat_id = chat["id"].as_i64().unwrap_or(0), title = chat["title"].as_str().unwrap_or(""),
            verdict = "tg_topics_forum_seen", "telegram topics: the bot is in a forum that is not AMUX_TELEGRAM_FORUM_CHAT");
    }
    let Some(forum) = forum else { return false };
    match route(update, forum, |id| bridge::is_owner(PLATFORM, id)) {
        Route::NotForum => false,
        Route::NotOwner(sender) => {
            tracing::warn!(sender, measured = true, n_considered = 1, verdict = "tg_topics_not_owner",
                "telegram topics: a message from someone other than the owner was ignored");
            true
        }
        Route::Ignored(why) => {
            tracing::debug!(why, verdict = "tg_topics_general_ignored", "telegram topics: nothing to deliver");
            true
        }
        Route::Topic(m) => {
            let conversation = m.conversation.clone();
            let unlink = m.text == "/unlink";
            match bridge::inbound(state, m).await {
                Outcome::Unlinked if !unlink => {
                    tracing::info!(conversation, measured = true, n_considered = 1, verdict = "tg_topics_unlinked_topic",
                        "telegram topics: a topic not linked to a worker");
                    bridge::adapters::send(PLATFORM, &conversation, true,
                        "This topic is not linked to a worker. Send /link <worker> here.").await.ok();
                }
                outcome => tracing::info!(conversation, outcome = ?outcome, measured = true, n_considered = 1,
                    verdict = "tg_topics_inbound", "telegram topics: an owner message in a topic"),
            }
            true
        }
    }
}

/// Worker name to thread id, for the topics of `chat`.
fn topics(state: &AppState, chat: i64) -> HashMap<String, i64> {
    let Ok(conn) = state.store.read() else { return HashMap::new() };
    let Ok(mut stmt) = conn.prepare(
        "SELECT conversation, conv FROM bridge_links WHERE platform = ?1 AND conversation LIKE ?2 AND conv LIKE 'w:%'",
    ) else {
        return HashMap::new();
    };
    let rows = stmt.query_map(rusqlite::params![PLATFORM, format!("{chat}:%")], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    });
    let mut out = HashMap::new();
    for (conversation, conv) in rows.into_iter().flatten().flatten() {
        if let (Some((c, thread)), Some(worker)) = (split_key(&conversation), conv.strip_prefix("w:")) {
            if c == chat {
                out.insert(worker.to_string(), thread);
            }
        }
    }
    out
}

fn base() -> String {
    bridge::setting("AMUX_BRIDGE_TELEGRAM_BASE")
        .unwrap_or_else(|| "https://api.telegram.org".into())
        .trim_end_matches('/')
        .to_string()
}

async fn call(method: &str, body: &Value) -> Result<Value, String> {
    let token = bridge::setting("TELEGRAM_BOT_TOKEN").ok_or("TELEGRAM_BOT_TOKEN is not set")?;
    let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    // without_url: the bot token is part of the URL and must not reach a log line.
    let r = client
        .post(format!("{}/bot{token}/{method}", base()))
        .json(body)
        .send()
        .await
        .map_err(|e| format!("{method}: {}", e.without_url()))?;
    let status = r.status().as_u16();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if v["ok"] == true {
        Ok(v["result"].clone())
    } else {
        Err(format!("{method} answered {status}: {}", v["description"].as_str().unwrap_or("no description")))
    }
}

/// Splits on line ends where it can; past MAX_CHUNKS the last chunk says so.
fn chunks(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        let mut line = line;
        while cur.chars().count() + line.chars().count() > CHUNK_CHARS {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            let cut = line.char_indices().nth(CHUNK_CHARS).map(|(i, _)| i).unwrap_or(line.len());
            out.push(line[..cut].to_string());
            line = &line[cut..];
        }
        cur.push_str(line);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    if out.len() > MAX_CHUNKS {
        let dropped = out.len() - MAX_CHUNKS;
        out.truncate(MAX_CHUNKS);
        if let Some(last) = out.last_mut() {
            last.push_str(&format!("\n\n[{dropped} more part(s) not sent; the full text is in amux]"));
        }
    }
    out
}

async fn send_to_topic(chat: i64, thread: i64, text: &str, silent: bool) -> Result<(), String> {
    for part in chunks(text) {
        call("sendMessage", &json!({
            "chat_id": chat, "message_thread_id": thread, "text": part, "disable_notification": silent,
        }))
        .await?;
    }
    Ok(())
}

fn first_time(worker: &str, turn: &str) -> bool {
    static SEEN: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    let key = format!("{worker}\u{1f}{turn}");
    let mut seen = SEEN.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
    if seen.contains(&key) {
        return false;
    }
    if seen.len() >= SEEN_TURNS_CAP {
        seen.pop_front();
    }
    seen.push_back(key);
    true
}

/// A finished turn. One that answered a Messages post is already carried by
/// `relay_turn` and the bridge; anything else goes to the topic without ringing.
pub async fn on_turn(state: &AppState, worker: &str, turn: &str, prompt: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() || prompt.contains("amux Messages") {
        return;
    }
    let Some(chat) = forum_chat() else { return };
    let Some(thread) = topics(state, chat).get(worker).copied() else { return };
    if !first_time(worker, turn) {
        return;
    }
    match send_to_topic(chat, thread, text, true).await {
        Ok(()) => tracing::info!(worker, thread, measured = true, n_considered = 1, verdict = "tg_topics_mirrored",
            "telegram topics: a turn was mirrored into the worker's topic"),
        Err(e) => tracing::warn!(worker, thread, error = %e, verdict = "tg_topics_send_failed",
            "telegram topics: a turn could not be sent to the worker's topic"),
    }
}

/// A push notification about `session`, copied into its topic (it rings).
pub fn notify(state: &AppState, session: &str, title: &str, body: &str) {
    if session.is_empty() {
        return;
    }
    let Some(chat) = forum_chat() else { return };
    let Some(thread) = topics(state, chat).get(session).copied() else { return };
    let (session, text) = (session.to_string(), format!("🔔 {title}\n{body}"));
    tokio::spawn(async move {
        if let Err(e) = send_to_topic(chat, thread, &text, false).await {
            tracing::warn!(worker = %session, thread, error = %e, verdict = "tg_topics_send_failed",
                "telegram topics: a notification could not be sent to the worker's topic");
        }
    });
}

async fn tick(state: &AppState, chat: i64) {
    let names = crate::api::session_verbs::all_lane_names();
    let have = topics(state, chat);
    let missing: Vec<&String> = names.iter().filter(|n| !have.contains_key(n.as_str())).collect();
    let mut created = 0usize;
    for name in missing.iter().take(CREATE_PER_TICK) {
        let topic_name: String = name.chars().take(128).collect();
        match call("createForumTopic", &json!({"chat_id": chat, "name": topic_name})).await {
            Ok(r) => {
                let Some(thread) = r["message_thread_id"].as_i64() else {
                    tracing::warn!(worker = %name, verdict = "tg_topic_create_failed", "telegram topics: createForumTopic returned no thread id");
                    break;
                };
                if let Err(e) = bridge::save_link(state, PLATFORM, &topic_key(chat, thread), true, &format!("w:{name}"), "fork_telegram_topics").await {
                    tracing::warn!(worker = %name, thread, error = %e, verdict = "tg_topic_create_failed",
                        "telegram topics: a topic was created but its link was not saved; the next tick creates another");
                    break;
                }
                created += 1;
                tracing::info!(worker = %name, thread, measured = true, n_considered = 1, verdict = "tg_topic_created",
                    "telegram topics: a worker got its forum topic");
            }
            Err(e) => {
                tracing::warn!(worker = %name, error = %e, verdict = "tg_topic_create_failed",
                    "telegram topics: a topic could not be created (is the bot an admin with Manage Topics?)");
                break;
            }
        }
    }
    if !missing.is_empty() {
        tracing::info!(workers = names.len(), with_topic = have.len(), missing = missing.len(), created, measured = true,
            n_considered = names.len(), verdict = "tg_topics_tick", "telegram topics: workers without a topic");
    }
}

/// Registered on /api/system-jobs as `telegram-topics` (off with AMUX_TELEGRAM_TOPICS_SECS=0 or AMUX_ISOLATED=1).
pub fn spawn(state: AppState) -> crate::runtime_jobs::PeriodicTask {
    crate::runtime_jobs::spawn_periodic(JOB, TICK_SECS, move || {
        let state = state.clone();
        async move {
            if let Some(chat) = forum_chat() {
                if bridge::setting("TELEGRAM_BOT_TOKEN").is_some() {
                    tick(&state, chat).await;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORUM: i64 = -1001234;

    fn upd(chat: i64, from: i64, text: &str, thread: Option<i64>) -> Value {
        let mut m = json!({"message_id": 7, "chat": {"id": chat, "type": "supergroup"}, "from": {"id": from, "username": "jan"}, "text": text});
        if let Some(t) = thread {
            m["message_thread_id"] = json!(t);
            m["is_topic_message"] = json!(true);
        }
        json!({"update_id": 1, "message": m})
    }

    #[test]
    fn only_the_forum_chat_is_taken() {
        assert!(matches!(route(&upd(-999, 1, "hi", Some(5)), FORUM, |_| true), Route::NotForum));
    }

    #[test]
    fn anyone_but_the_owner_is_ignored() {
        assert!(matches!(route(&upd(FORUM, 2, "hi", Some(5)), FORUM, |id| id == "1"), Route::NotOwner(s) if s == "2"));
    }

    #[test]
    fn the_general_topic_delivers_nothing() {
        assert!(matches!(route(&upd(FORUM, 1, "hi", None), FORUM, |_| true), Route::Ignored("general topic")));
        let mut reply_in_general = upd(FORUM, 1, "hi", None);
        reply_in_general["message"]["message_thread_id"] = json!(5);
        assert!(matches!(route(&reply_in_general, FORUM, |_| true), Route::Ignored("general topic")));
    }

    #[test]
    fn a_topic_message_is_a_bridge_message_for_that_topic() {
        let Route::Topic(m) = route(&upd(FORUM, 1, "/link@amux_bot proceram", Some(5)), FORUM, |_| true) else { panic!("not a topic") };
        assert_eq!(m.conversation, "-1001234:5");
        assert_eq!(m.text, "/link proceram");
        assert_eq!(m.platform_msg_id, "-1001234:7");
        assert!(m.is_group);
    }

    #[test]
    fn a_topic_key_sends_into_the_thread_and_a_plain_chat_does_not() {
        let mut p = json!({"chat_id": "-1001234:5", "text": "x"});
        target(&mut p, "-1001234:5");
        assert_eq!(p["chat_id"], json!(-1001234));
        assert_eq!(p["message_thread_id"], json!(5));
        let mut q = json!({"chat_id": "42", "text": "x"});
        target(&mut q, "42");
        assert_eq!(q, json!({"chat_id": "42", "text": "x"}));
    }

    #[test]
    fn long_text_is_split_and_capped() {
        let line = format!("{}\n", "a".repeat(1000));
        let parts = chunks(&line.repeat(30));
        assert_eq!(parts.len(), MAX_CHUNKS);
        assert!(parts.iter().all(|p| p.chars().count() <= CHUNK_CHARS + 80));
        assert!(parts.last().unwrap().contains("more part(s) not sent"));
        assert_eq!(chunks("short"), vec!["short".to_string()]);
        let one_long_line = "b".repeat(CHUNK_CHARS * 2 + 10);
        assert_eq!(chunks(&one_long_line).len(), 3);
    }

    #[test]
    fn a_turn_is_mirrored_once() {
        assert!(first_time("w", "turn-1"));
        assert!(!first_time("w", "turn-1"));
        assert!(first_time("w2", "turn-1"));
    }
}
