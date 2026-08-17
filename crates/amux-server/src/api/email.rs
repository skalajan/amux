//! Email API (RR-0088): `/api/email/send|reply|inbox|search|log`, request
//! and response field names IDENTICAL to the Python handlers (the CLAUDE.md
//! email contract: to/subject/body/from/cc; message_id for replies;
//! X-Amux-Session attribution recorded in the send-audit ledger,
//! AMUX-1897).
//!
//! Scope note (honest deviation, named): the Python handlers fall back to
//! Mail.app/AppleScript for accounts without a Gmail token. That path is
//! deliberately NOT ported — this server answers 501 with the way out
//! (connect the account) instead of silently doing nothing. Everything on
//! the Gmail-API path is ported: validation messages, the new-thread guard
//! (AMUX-1739), signature handling, threading proof fields, the send-audit
//! ledger, and the AMUX-1886 inbox window semantics.
//!
//! Every real Gmail outcome feeds the IntegrationRegistry (RR-0073): a
//! successful call proves `email: available`; an auth-shaped failure
//! (invalid_grant / not_connected) proves `unavailable`; anything else is
//! `degraded`. That is the "state from real outcomes, not token files"
//! contract.

use super::AppState;
use crate::integrations::email::{
    email_log, read_email_log, GmailClient, OUR_DOMAINS,
};
use crate::integrations::{self, IntegrationRegistry, IntegrationState};
use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Handler context: the Gmail client (mockable transport) + the registry
/// its outcomes feed.
pub struct EmailCtx {
    pub client: Arc<GmailClient>,
    pub registry: Arc<IntegrationRegistry>,
}

pub fn routes() -> Router<AppState> {
    routes_with(Arc::new(EmailCtx {
        client: Arc::new(GmailClient::new_default()),
        registry: integrations::global_registry().clone(),
    }))
}

pub fn routes_with(ctx: Arc<EmailCtx>) -> Router<AppState> {
    Router::new()
        .route("/send", post(send))
        .route("/reply", post(reply))
        .route("/inbox", get(inbox))
        .route("/search", get(search))
        .route("/log", get(send_log))
        .layer(Extension(ctx))
}

// ---- shared helpers -------------------------------------------------------

fn err(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// Python `_hdr_worker`: X-Amux-Worker is canonical, X-Amux-Session still
/// works. `None` (not "") when unattributed so the ledger's `session` field
/// round-trips as Python's `null`.
fn hdr_worker(headers: &HeaderMap) -> Option<String> {
    for name in ["x-amux-worker", "x-amux-session"] {
        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim) {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Which Gmail accounts this lane is scoped to, or `None` for "unrestricted"
/// (AMUX-3103).
///
/// Resolved through `scoped_setting_in`, so `AMUX_GMAIL_ACCOUNTS` obeys the same
/// global -> group -> worker precedence as every other scoped setting: set it once
/// on the `mixpeek` group, override it on one worker, and the worker wins. That
/// is the whole connector-scoping mechanism — a connector is a composition of
/// env + scope, not a new subsystem (docs/design/connectors.md).
///
/// ABSENT MEANS UNRESTRICTED, deliberately. A connector that denied by default
/// would break every lane that sends mail today the moment this shipped, and a
/// guard that has to be rolled out atomically with its policy is a guard people
/// disable. Presence of configuration is the switch, per the single-codebase
/// rule — no build flag, no IS_SCOPED branch.
fn gmail_scope_allowed(home: &std::path::Path, lane: &str) -> Option<Vec<String>> {
    let raw = crate::api::session_verbs::scoped_setting_in(home, lane, "AMUX_GMAIL_ACCOUNTS")?;
    let list: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    (!list.is_empty()).then_some(list)
}

/// The UNHAPPY PATH Ethan named as the acceptance test: a lane asking to send as
/// an account it is not scoped to must be DENIED, and told what it may use.
///
/// Returns the refusal body, or `None` to allow. Also resolves the empty-`from`
/// case: an unscoped lane keeps the historical default, a scoped lane defaults to
/// its FIRST allowed account rather than to a global default it may not hold —
/// otherwise "scoped to the personal account" would still send as ethan@ whenever
/// `from` was omitted, which is the same bug wearing a default.
fn gmail_scope_check(home: &std::path::Path, lane: &str, from: &str) -> Result<Option<String>, Value> {
    let Some(allowed) = gmail_scope_allowed(home, lane) else {
        return Ok(None); // unrestricted: caller's `from` stands
    };
    if from.is_empty() {
        return Ok(Some(allowed[0].clone()));
    }
    if allowed.iter().any(|a| a.eq_ignore_ascii_case(from)) {
        return Ok(None);
    }
    Err(json!({
        "error": format!(
            "worker '{lane}' is not scoped to send as {from} — allowed: {}",
            allowed.join(", ")
        ),
        "blocked": "connector_scope",
        "connector": "gmail",
        "worker": lane,
        "requested": from,
        "allowed": allowed,
        "how_to_change": "set AMUX_GMAIL_ACCOUNTS at global/group/worker scope (worker wins)",
    }))
}

fn body_str(body: &Value, k: &str) -> String {
    body.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// Feed a real Gmail outcome into the registry (RR-0073). Auth-shaped
/// failures are `Unavailable` (re-auth needed / no token); transient ones
/// `Degraded`; success is the only path to `Available`.
fn report_outcome(reg: &IntegrationRegistry, result: &Result<(), String>) {
    match result {
        Ok(()) => reg.set("email", IntegrationState::Available),
        Err(e) if e.contains("invalid_grant") || e.contains("not_connected") => {
            reg.set("email", IntegrationState::Unavailable { reason: e.clone() })
        }
        Err(e) => reg.set("email", IntegrationState::Degraded { reason: e.clone() }),
    }
}

/// The Mail.app/AppleScript fallback is not ported (module docs): refuse
/// honestly with the way out, never pretend to send.
fn applescript_not_ported(account: &str) -> Response {
    err(
        StatusCode::NOT_IMPLEMENTED,
        json!({
            "error": format!(
                "'{account}' is not a connected Gmail account and the Mail.app/AppleScript \
                 fallback is not ported to this server — connect it \
                 (GET /api/gmail/auth?account={account}) or use a connected account"
            ),
            "connected_hint": "GET /api/gmail/accounts lists connected accounts",
        }),
    )
}

/// Refuse a SEND/REPLY whose from-address is not a connected Gmail account, AND
/// record the attempt where a human already looks (GE-621, prevention-a + its
/// observability half). Refusing was already correct — the Rust server never had
/// the silent Mail.app fallback that sent 3 replies from Ethan's personal account
/// off-ledger. But the refusal wrote NOTHING to /api/email/log or the server log,
/// so an off-account attempt left no trace: the API did the right thing invisibly.
/// Now every refused send lands in the audited log (`via: "refused"`, `refused:
/// true`) and a WARN, so a sweep of /api/email/log catches "someone keeps trying
/// to send from a non-connected account" — the early-warning signal for the whole
/// class (two-fixes rule). Read endpoints (inbox/search) keep the quiet refusal:
/// a read from an unconnected account is benign and frequent, not an incident.
fn refuse_send(ctx: &EmailCtx, headers: &HeaderMap, endpoint: &str, account: &str) -> Response {
    tracing::warn!(
        endpoint, from = %account,
        "email {endpoint} REFUSED: from-address is not a connected Gmail account \
         (no silent Mail.app fallback) — recorded to /api/email/log (GE-621)"
    );
    email_log(
        ctx.client.home(),
        json!({
            "endpoint": endpoint,
            "via": "refused",
            "refused": true,
            "from": account,
            "reason": "from-address is not a connected Gmail account; \
                       Mail.app/AppleScript fallback is not ported",
            "session": hdr_worker(headers),
        }),
    );
    applescript_not_ported(account)
}

const ADDR_RE: &str = r"^[^@\s]+@[^@\s]+\.[^@\s]+$";

fn bad_addrs(list: &str) -> Vec<String> {
    let re = regex::Regex::new(ADDR_RE).expect("static regex");
    list.split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty() && !re.is_match(a))
        .map(String::from)
        .collect()
}

// ---- POST /api/email/send -------------------------------------------------

pub async fn send(
    Extension(ctx): Extension<Arc<EmailCtx>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // Reject threading headers — Python's exact refusal (threaded replies
    // belong on /reply, where In-Reply-To/References are derived correctly).
    for forbidden_key in ["in_reply_to", "references", "inReplyTo"] {
        if body.get(forbidden_key).map(|v| !v.is_null() && v != &json!("")).unwrap_or(false) {
            return err(
                StatusCode::BAD_REQUEST,
                json!({
                    "error": format!(
                        "'{forbidden_key}' is not supported on /api/email/send — \
                         Mail.app cannot set custom headers on outgoing messages. \
                         Use POST /api/email/reply to reply in-thread."
                    )
                }),
            );
        }
    }
    let to = body_str(&body, "to");
    let subject = body_str(&body, "subject");
    let message = body_str(&body, "body");
    let cc = body_str(&body, "cc");
    let mut from_acct = body_str(&body, "from");
    // CONNECTOR SCOPE (AMUX-3103): enforced before anything is sent, and keyed on
    // the SERVER-VERIFIED caller header rather than anything in the body — a lane
    // must not be able to widen its own scope by claiming a different worker
    // (AMUX-1768 is the same principle for message provenance).
    if let Some(lane) = hdr_worker(&headers) {
        match gmail_scope_check(&crate::api::session_verbs::home(), &lane, &from_acct) {
            Err(denial) => return err(StatusCode::FORBIDDEN, denial),
            Ok(Some(defaulted)) => from_acct = defaulted,
            Ok(None) => {}
        }
    }
    if to.is_empty() || subject.is_empty() || message.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            json!({ "error": "to, subject, and body are required" }),
        );
    }
    // to/cc may be comma-separated lists; validate each part (Python
    // parity, including the error strings).
    let bad_to = bad_addrs(&to);
    if !bad_to.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            json!({ "error": format!("invalid email address: {}", bad_to.join(", ")) }),
        );
    }
    let bad_cc = bad_addrs(&cc);
    if !bad_cc.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            json!({ "error": format!("invalid cc address: {}", bad_cc.join(", ")) }),
        );
    }

    let connected = ctx.client.connected_accounts();

    // NEW-THREAD GUARD (AMUX-1739): a /send to an EXTERNAL recipient with an
    // active thread fragments a customer conversation. Block with the
    // candidate so replying stays the DEFAULT; a new thread requires an
    // explicit force_new_thread. Own-domain/connected recipients exempt;
    // fails open on Gmail API errors (latest_matching -> None).
    if !body.get("force_new_thread").map(truthy).unwrap_or(false) {
        let conn_lc: Vec<String> = connected.iter().map(|a| a.to_lowercase()).collect();
        let ext_rcpts: Vec<String> = to
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .filter(|a| {
                let al = a.to_lowercase();
                !conn_lc.contains(&al)
                    && !OUR_DOMAINS.iter().any(|d| al.ends_with(&format!("@{d}")))
            })
            .map(String::from)
            .collect();
        let win = body
            .get("thread_window_days")
            .and_then(Value::as_i64)
            .unwrap_or(14)
            .clamp(1, 90);
        for r in &ext_rcpts {
            let mut cand = None;
            for acct in &connected {
                cand = ctx.client.latest_matching(acct, "", r, "", win).await;
                if cand.is_some() {
                    break;
                }
            }
            if let Some(cand) = cand {
                tracing::info!(recipient = %r, "new-thread guard: blocked /send (active thread)");
                return err(
                    StatusCode::CONFLICT,
                    json!({
                        "error": format!(
                            "{r} has an active thread in the last {win} days — \
                             replying in-thread is the default; opening a new thread \
                             must be explicit"
                        ),
                        "blocked": true,
                        "recipient": r,
                        "candidate_thread": cand,
                        "reply_instead": {
                            "endpoint": "POST /api/email/reply",
                            "body": {
                                "message_id": cand.get("message_id").cloned().unwrap_or(json!("")),
                                "body": "...",
                                "from": if from_acct.is_empty() { "ethan@mixpeek.com".to_string() } else { from_acct.clone() },
                            },
                        },
                        "or_force": {
                            "force_new_thread": true,
                            "note": "resend with this flag to deliberately start a new thread",
                        },
                    }),
                );
            }
        }
    }

    // Python: `body.get("signature", True) is not False` — only a literal
    // false disables the signature.
    let include_sig = body.get("signature") != Some(&Value::Bool(false));
    if !from_acct.is_empty() && connected.contains(&from_acct) {
        let res = ctx
            .client
            .compose_send(&from_acct, &to, &subject, &message, &cc, "", "", "", include_sig)
            .await;
        report_outcome(&ctx.registry, &res.as_ref().map(|_| ()).map_err(Clone::clone));
        return match res {
            Err(e) => err(StatusCode::BAD_GATEWAY, json!({ "error": e })),
            Ok(res) => {
                email_log(
                    ctx.client.home(),
                    json!({
                        "endpoint": "send", "via": "gmail", "from": from_acct,
                        "to": to, "cc": if cc.is_empty() { Value::Null } else { json!(cc) },
                        "subject": subject,
                        "body_chars": message.chars().count(),
                        "body_preview": message.chars().take(240).collect::<String>(),
                        "id": res.get("id").cloned().unwrap_or(Value::Null),
                        "thread_id": res.get("thread_id").cloned().unwrap_or(Value::Null),
                        "session": hdr_worker(&headers),
                    }),
                );
                Json(json!({
                    "ok": true, "to": to, "subject": subject, "from": from_acct,
                    "cc": if cc.is_empty() { Value::Null } else { json!(cc) },
                    "via": "gmail",
                    "id": res.get("id").cloned().unwrap_or(Value::Null),
                    "thread_id": res.get("thread_id").cloned().unwrap_or(Value::Null),
                    "signature_included": res.get("signature_included").cloned().unwrap_or(Value::Null),
                }))
                .into_response()
            }
        };
    }
    refuse_send(&ctx, &headers, "send", &from_acct)
}

use super::py_truthy as truthy;

// ---- POST /api/email/reply ------------------------------------------------

pub async fn reply(
    Extension(ctx): Extension<Arc<EmailCtx>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let mut message_id = body_str(&body, "message_id");
    let reply_body = body_str(&body, "body");
    let reply_all = body.get("reply_all").map(truthy).unwrap_or(false);
    let from_acct = body_str(&body, "from");
    let dry_run = body.get("dry_run").map(truthy).unwrap_or(false);
    let connected = ctx.client.connected_accounts();

    // REPLY-BY-SELECTOR (AMUX-1739): resolve "the latest message from X"
    // server-side; dry_run returns the target WITHOUT sending.
    let mut resolved: Option<Value> = None;
    if message_id.is_empty() {
        let sel_from = body_str(&body, "reply_to_latest_from");
        if !sel_from.is_empty() {
            let subj_c = body_str(&body, "subject_contains");
            let pref = body_str(&body, "from");
            let mut accts: Vec<String> = Vec::new();
            if connected.contains(&pref) {
                accts.push(pref.clone());
            }
            accts.extend(connected.iter().filter(|a| **a != pref).cloned());
            for a in &accts {
                resolved = ctx.client.latest_matching(a, &sel_from, "", &subj_c, 0).await;
                if resolved.is_some() {
                    break;
                }
            }
            match &resolved {
                Some(r) => {
                    message_id =
                        r.get("message_id").and_then(Value::as_str).unwrap_or("").to_string()
                }
                None => {
                    let mut msg = format!("no message from {sel_from}");
                    if !subj_c.is_empty() {
                        msg.push_str(&format!(" with subject containing {subj_c:?}"));
                    }
                    msg.push_str(" found in any connected account");
                    return err(StatusCode::NOT_FOUND, json!({ "error": msg }));
                }
            }
        }
    }
    if message_id.is_empty() || (reply_body.is_empty() && !dry_run) {
        return err(
            StatusCode::BAD_REQUEST,
            json!({ "error": "message_id (or reply_to_latest_from) and body are required" }),
        );
    }
    if dry_run {
        return Json(json!({
            "ok": true, "dry_run": true, "would_reply_to": message_id,
            "resolved": resolved,
            "note": "no email sent — repeat without dry_run to send",
        }))
        .into_response();
    }
    let gmail_from =
        if from_acct.is_empty() { body_str(&body, "account") } else { from_acct.clone() };
    let include_sig = body.get("signature") != Some(&Value::Bool(false));
    if !gmail_from.is_empty() && connected.contains(&gmail_from) {
        let allow_self = body.get("allow_self").map(truthy).unwrap_or(false);
        let res = ctx
            .client
            .reply_send(&gmail_from, &message_id, &reply_body, include_sig, reply_all, allow_self)
            .await;
        report_outcome(&ctx.registry, &res.as_ref().map(|_| ()).map_err(Clone::clone));
        return match res {
            Err(e) => err(StatusCode::BAD_GATEWAY, json!({ "error": e })),
            Ok(res) => {
                email_log(
                    ctx.client.home(),
                    json!({
                        "endpoint": "reply", "via": "gmail", "from": gmail_from,
                        "in_reply_to": message_id, "reply_all": reply_all,
                        "body_chars": reply_body.chars().count(),
                        "body_preview": reply_body.chars().take(240).collect::<String>(),
                        "id": res.get("id").cloned().unwrap_or(Value::Null),
                        "thread_id": res.get("thread_id").cloned().unwrap_or(Value::Null),
                        "session": hdr_worker(&headers),
                    }),
                );
                Json(json!({
                    "ok": true, "message_id": message_id, "reply_all": reply_all,
                    "from": gmail_from, "via": "gmail",
                    "id": res.get("id").cloned().unwrap_or(Value::Null),
                    "thread_id": res.get("thread_id").cloned().unwrap_or(Value::Null),
                    "orig_thread_id": res.get("orig_thread_id").cloned().unwrap_or(Value::Null),
                    "threaded": res.get("threaded").cloned().unwrap_or(Value::Null),
                    "signature_included": res.get("signature_included").cloned().unwrap_or(Value::Null),
                }))
                .into_response()
            }
        };
    }
    refuse_send(&ctx, &headers, "reply", &gmail_from)
}

// ---- GET /api/email/inbox -------------------------------------------------

pub async fn inbox(
    Extension(ctx): Extension<Arc<EmailCtx>>,
    Query(qs): Query<HashMap<String, String>>,
) -> Response {
    let account_filter = qs.get("account").cloned().unwrap_or_default();
    let count: usize =
        qs.get("count").and_then(|v| v.parse().ok()).unwrap_or(20).min(500);
    let lookback_days: f64 = qs.get("days").and_then(|v| v.parse().ok()).unwrap_or(7.0);
    let envelope = matches!(
        qs.get("envelope").map(String::as_str),
        Some("1") | Some("true") | Some("yes")
    );
    let reply_shape = |msgs: Vec<Value>, truncated: bool| -> Response {
        if envelope {
            Json(json!({
                "messages": msgs, "returned": msgs.len(),
                "truncated": truncated, "window_days": lookback_days,
            }))
            .into_response()
        } else {
            Json(Value::Array(msgs)).into_response()
        }
    };
    let connected = ctx.client.connected_accounts();
    if !account_filter.is_empty() && connected.contains(&account_filter) {
        let res = ctx.client.inbox_messages(&account_filter, count, "", lookback_days).await;
        report_outcome(&ctx.registry, &res.as_ref().map(|_| ()).map_err(Clone::clone));
        return match res {
            Err(e) => err(StatusCode::BAD_GATEWAY, json!({ "error": e })),
            Ok(v) => reply_shape(
                v.get("messages").and_then(Value::as_array).cloned().unwrap_or_default(),
                v.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            ),
        };
    }
    if account_filter.is_empty() && !connected.is_empty() {
        // Parallel fan-out over connected accounts (the Python 504 fix):
        // each bounded so one wedged token can't hang the unified inbox.
        let futs = connected.iter().map(|a| {
            let client = ctx.client.clone();
            let a = a.clone();
            async move {
                tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    client.inbox_messages(&a, count, "", lookback_days),
                )
                .await
                .ok()
                .and_then(Result::ok)
            }
        });
        let results = futures::future::join_all(futs).await;
        let mut msgs: Vec<Value> = Vec::new();
        let mut any_trunc = false;
        let mut any_ok = false;
        for r in results.into_iter().flatten() {
            any_ok = true;
            if let Some(m) = r.get("messages").and_then(Value::as_array) {
                msgs.extend(m.iter().cloned());
            }
            if r.get("truncated").and_then(Value::as_bool).unwrap_or(false) {
                any_trunc = true;
            }
        }
        if any_ok {
            ctx.registry.set("email", IntegrationState::Available);
        }
        // Newest first; an unparseable date sinks, never errors.
        msgs.sort_by(|a, b| recv_ts(b).partial_cmp(&recv_ts(a)).unwrap_or(std::cmp::Ordering::Equal));
        if msgs.len() > count {
            any_trunc = true;
        }
        msgs.truncate(count);
        return reply_shape(msgs, any_trunc);
    }
    applescript_not_ported(&account_filter)
}

fn recv_ts(m: &Value) -> f64 {
    m.get("date")
        .and_then(Value::as_str)
        .and_then(|d| chrono::DateTime::parse_from_rfc2822(d).ok())
        .map(|dt| dt.timestamp() as f64)
        .unwrap_or(0.0)
}

// ---- GET /api/email/search ------------------------------------------------

pub async fn search(
    Extension(ctx): Extension<Arc<EmailCtx>>,
    Query(qs): Query<HashMap<String, String>>,
) -> Response {
    let q = qs.get("q").map(|s| s.trim().to_string()).unwrap_or_default();
    if q.is_empty() {
        return err(StatusCode::BAD_REQUEST, json!({ "error": "q parameter is required" }));
    }
    let account = qs.get("account").map(|s| s.trim().to_string()).unwrap_or_default();
    let limit: usize = qs.get("limit").and_then(|v| v.parse().ok()).unwrap_or(20).min(100);
    let days: i64 = qs.get("days").and_then(|v| v.parse().ok()).unwrap_or(30);
    let mailbox = qs.get("mailbox").map(|s| s.trim().to_string()).unwrap_or_default();

    // Python `_gmail_query`: mailbox maps to a Gmail operator; `days` is a
    // real filter (it was silently ignored once — kept fixed).
    let mut gq = q.clone();
    let mb = mailbox.to_lowercase();
    if mb == "sent" || mb == "sent mail" {
        gq.push_str(" in:sent");
    } else if mb == "inbox" {
        gq.push_str(" in:inbox");
    } else if !mb.is_empty() && mb != "all" {
        gq.push_str(&format!(" label:{mailbox}"));
    }
    if days > 0 {
        gq.push_str(&format!(" newer_than:{days}d"));
    }
    let gq = gq.trim().to_string();

    let connected = ctx.client.connected_accounts();
    if !account.is_empty() && connected.contains(&account) {
        let res = ctx.client.inbox_messages(&account, limit, &gq, 0.0).await;
        report_outcome(&ctx.registry, &res.as_ref().map(|_| ()).map_err(Clone::clone));
        return match res {
            Err(e) => err(StatusCode::BAD_GATEWAY, json!({ "error": e })),
            Ok(v) => Json(v.get("messages").cloned().unwrap_or(json!([]))).into_response(),
        };
    }
    if account.is_empty() && !connected.is_empty() {
        let futs = connected.iter().map(|a| {
            let client = ctx.client.clone();
            let (a, gq) = (a.clone(), gq.clone());
            async move { client.inbox_messages(&a, limit, &gq, 0.0).await.ok() }
        });
        let results = futures::future::join_all(futs).await;
        let mut merged: Vec<Value> = Vec::new();
        for r in results.into_iter().flatten() {
            if let Some(m) = r.get("messages").and_then(Value::as_array) {
                merged.extend(m.iter().cloned());
            }
        }
        merged.sort_by(|a, b| {
            recv_ts(b).partial_cmp(&recv_ts(a)).unwrap_or(std::cmp::Ordering::Equal)
        });
        merged.truncate(limit);
        return Json(Value::Array(merged)).into_response();
    }
    applescript_not_ported(&account)
}

// ---- GET /api/email/log ---------------------------------------------------

/// The send-audit ledger (AMUX-1897): one call answers "who sent X and
/// when". `session=unattributed` returns records sent without the header.
pub async fn send_log(
    Extension(ctx): Extension<Arc<EmailCtx>>,
    Query(qs): Query<HashMap<String, String>>,
) -> Response {
    let days: i64 = qs.get("days").and_then(|v| v.parse().ok()).unwrap_or(7);
    let limit: usize = qs.get("limit").and_then(|v| v.parse().ok()).unwrap_or(50).min(500);
    let session = qs.get("session").map(|s| s.trim().to_string()).unwrap_or_default();
    Json(read_email_log(ctx.client.home(), days, limit, &session)).into_response()
}

// ---------------------------------------------------------------------------
// Tests — mocked transport + temp homes only. No network, no live token
// files, no credential values.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // ---- AMUX-3103: the connector-scope eval -----------------------------
    //
    // Ethan's acceptance for the whole connectors block is "verify/validate
    // access in UNHAPPY paths", so these are written as the acceptance test and
    // not as an afterthought. Each one is a path that must DENY, plus the happy
    // path beside it — a deny-everything bug and a deny-nothing bug look
    // identical if you only test one side.
    fn scope_home(layers: &[(&str, &str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("env")).unwrap();
        std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
        for (level, name, body) in layers {
            let p = match *level {
                "global" => dir.path().join("amux.env"),
                "group" => dir.path().join("env").join(format!("{name}.env")),
                _ => dir.path().join("sessions").join(format!("{name}.env")),
            };
            std::fs::write(p, body).unwrap();
        }
        dir
    }

    #[test]
    fn unscoped_lane_is_unrestricted_so_nothing_breaks_on_rollout() {
        let d = scope_home(&[("worker", "w", "CC_TAGS=gtm\n")]);
        // No AMUX_GMAIL_ACCOUNTS anywhere: the caller's `from` stands untouched.
        assert!(matches!(gmail_scope_check(d.path(), "w", "anyone@example.com"), Ok(None)));
    }

    #[test]
    fn wrong_account_is_denied_and_told_what_it_may_use() {
        let d = scope_home(&[("worker", "w", "AMUX_GMAIL_ACCOUNTS=personal@gmail.com\n")]);
        let denial = gmail_scope_check(d.path(), "w", "ethan@mixpeek.com")
            .expect_err("a lane scoped to personal MUST NOT send as ethan@mixpeek.com");
        assert_eq!(denial["blocked"], "connector_scope");
        assert_eq!(denial["requested"], "ethan@mixpeek.com");
        // The refusal must be actionable: naming the allowed set is what stops
        // the caller retrying the same thing (the AMUX-2325 lesson).
        assert!(denial["error"].as_str().unwrap().contains("personal@gmail.com"));
        assert!(denial["how_to_change"].as_str().unwrap().contains("AMUX_GMAIL_ACCOUNTS"));
    }

    #[test]
    fn right_account_is_allowed_case_insensitively() {
        let d = scope_home(&[("worker", "w", "AMUX_GMAIL_ACCOUNTS=info@mixpeek.com,ethan@mixpeek.com\n")]);
        assert!(matches!(gmail_scope_check(d.path(), "w", "ethan@mixpeek.com"), Ok(None)));
        // Addresses are case-insensitive; a scope that rejected Ethan@ would be a
        // deny-the-right-account bug, which is the failure users report as broken.
        assert!(matches!(gmail_scope_check(d.path(), "w", "Ethan@Mixpeek.com"), Ok(None)));
    }

    #[test]
    fn omitted_from_defaults_into_scope_not_to_the_global_default() {
        // The subtle one. Without this, a lane scoped to the personal account
        // still sends as ethan@mixpeek.com whenever `from` is omitted, because
        // the handler's historical default fires — scoped in name only.
        let d = scope_home(&[("worker", "w", "AMUX_GMAIL_ACCOUNTS=personal@gmail.com\n")]);
        match gmail_scope_check(d.path(), "w", "") {
            Ok(Some(defaulted)) => assert_eq!(defaulted, "personal@gmail.com"),
            other => panic!("expected the scoped default, got {other:?}"),
        }
    }

    #[test]
    fn worker_scope_overrides_group_scope() {
        // Ethan's actual layout: the mixpeek GROUP gets the work accounts, one
        // worker is moved to the personal account. This is the end-to-end proof
        // that connector scope rides the global->group->worker layers rather
        // than being a second mechanism.
        let d = scope_home(&[
            ("global", "", "AMUX_GMAIL_ACCOUNTS=info@mixpeek.com\n"),
            ("group", "mixpeek", "AMUX_GMAIL_ACCOUNTS=info@mixpeek.com,ethan@mixpeek.com\n"),
            ("worker", "refresh-house", "CC_TAGS=mixpeek\nAMUX_GMAIL_ACCOUNTS=personal@gmail.com\n"),
            ("worker", "backend", "CC_TAGS=mixpeek\n"),
        ]);
        // worker layer wins for the overridden lane
        assert!(gmail_scope_check(d.path(), "refresh-house", "ethan@mixpeek.com").is_err());
        assert!(matches!(gmail_scope_check(d.path(), "refresh-house", "personal@gmail.com"), Ok(None)));
        // a lane with no worker override inherits the GROUP layer, not global
        assert!(matches!(gmail_scope_check(d.path(), "backend", "ethan@mixpeek.com"), Ok(None)));
        assert!(gmail_scope_check(d.path(), "backend", "personal@gmail.com").is_err());
    }

    use super::*;
    use crate::db::Store;
    use crate::integrations::email::HttpTransport;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Mutex;
    use tower::ServiceExt;

    /// Scripted transport (same shape as the integrations::email mock):
    /// first (method, url-substring) match wins and pops.
    struct MockHttp {
        calls: Mutex<Vec<(String, String, Option<Value>)>>,
        script: Mutex<Vec<(String, String, u16, Value)>>,
    }
    impl MockHttp {
        fn new(script: Vec<(&str, &str, u16, Value)>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                script: Mutex::new(
                    script
                        .into_iter()
                        .map(|(m, u, s, v)| (m.to_string(), u.to_string(), s, v))
                        .collect(),
                ),
            })
        }
        fn answer(&self, method: &str, url: &str, body: Option<&Value>) -> Result<(u16, Value), String> {
            self.calls.lock().unwrap().push((method.into(), url.into(), body.cloned()));
            let mut script = self.script.lock().unwrap();
            if let Some(pos) =
                script.iter().position(|(m, sub, _, _)| m == method && url.contains(sub.as_str()))
            {
                let (_, _, status, v) = script.remove(pos);
                return Ok((status, v));
            }
            Err(format!("mock has no answer for {method} {url}"))
        }
    }
    #[async_trait]
    impl HttpTransport for MockHttp {
        async fn get(&self, url: &str, _b: Option<&str>) -> Result<(u16, Value), String> {
            self.answer("GET", url, None)
        }
        async fn post_json(
            &self,
            url: &str,
            _b: Option<&str>,
            body: &Value,
        ) -> Result<(u16, Value), String> {
            self.answer("POST", url, Some(body))
        }
        async fn post_form(
            &self,
            url: &str,
            form: &[(String, String)],
        ) -> Result<(u16, Value), String> {
            let v = Value::Object(form.iter().map(|(k, val)| (k.clone(), json!(val))).collect());
            self.answer("FORM", url, Some(&v))
        }
    }

    const ACCT: &str = "acct@example.com";

    /// Temp amux home with one connected account (PLACEHOLDER token values
    /// only — never real credentials).
    fn temp_home() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("gmail-tokens");
        std::fs::create_dir_all(&tokens).unwrap();
        std::fs::write(
            tokens.join(format!("{ACCT}.json")),
            json!({
                "token": "PLACEHOLDER_ACCESS",
                "refresh_token": "PLACEHOLDER_REFRESH",
                "token_uri": "https://oauth2.googleapis.com/token",
                "client_id": "PLACEHOLDER_ID",
                "client_secret": "PLACEHOLDER_SECRET",
            })
            .to_string(),
        )
        .unwrap();
        dir
    }

    fn app_with(
        http: Arc<MockHttp>,
        home: &std::path::Path,
    ) -> (axum::Router, tempfile::TempDir, Arc<IntegrationRegistry>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("email-api-test.db")).unwrap();
        let state = AppState {
            store: Arc::new(store),
            started: std::time::Instant::now(),
            build_hash: "test".into(),
            auth_token: None,
        };
        let registry = Arc::new(IntegrationRegistry::new());
        let ctx = Arc::new(EmailCtx {
            client: Arc::new(GmailClient::new(http, home.to_path_buf())),
            registry: registry.clone(),
        });
        let router = Router::new().nest("/api/email", routes_with(ctx)).with_state(state);
        (router, dir, registry)
    }

    async fn send_req(
        app: &axum::Router,
        method: &str,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, v)
    }

    #[tokio::test]
    async fn send_validations_match_python() {
        let home = temp_home();
        let (app, _d, _r) = app_with(MockHttp::new(vec![]), home.path());
        // Threading headers rejected with Python's message.
        let (st, e) = send_req(
            &app,
            "POST",
            "/api/email/send",
            Some(json!({ "to": "x@y.co", "subject": "s", "body": "b", "in_reply_to": "<m@x>" })),
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(e["error"].as_str().unwrap().contains("'in_reply_to' is not supported"));
        // Required fields.
        let (st, e) =
            send_req(&app, "POST", "/api/email/send", Some(json!({ "to": "x@y.co" })), &[]).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(e["error"], json!("to, subject, and body are required"));
        // Address validation, comma lists validated per part.
        let (st, e) = send_req(
            &app,
            "POST",
            "/api/email/send",
            Some(json!({ "to": "good@x.co, not-an-addr", "subject": "s", "body": "b" })),
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(e["error"], json!("invalid email address: not-an-addr"));
    }

    #[tokio::test]
    async fn send_from_unconnected_account_is_honest_501() {
        let home = temp_home();
        let (app, _d, _r) = app_with(MockHttp::new(vec![]), home.path());
        let (st, e) = send_req(
            &app,
            "POST",
            "/api/email/send",
            Some(json!({ "to": "x@y.co", "subject": "s", "body": "b",
                         "from": "other@nowhere.com", "force_new_thread": true })),
            &[("x-amux-session", "gtm-lane")],
        )
        .await;
        assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
        assert!(e["error"].as_str().unwrap().contains("not ported"), "{e}");

        // GE-621: the refusal must be VISIBLE in the audited log — an off-account
        // send attempt that leaves no trace is the incident's invisibility. The
        // refusal now writes a `via:"refused"` entry a sweep of /api/email/log can
        // find, so "someone keeps trying to send from a non-connected account"
        // self-announces (two-fixes rule).
        let log = crate::integrations::email::read_email_log(home.path(), 1, 50, "");
        let entries = log["log"].as_array().cloned().unwrap_or_default();
        let refused = entries
            .iter()
            .find(|x| x.get("refused") == Some(&json!(true)))
            .expect("the refused send must be recorded in the audited email log");
        assert_eq!(refused["from"], json!("other@nowhere.com"));
        assert_eq!(refused["endpoint"], json!("send"));
        assert_eq!(refused["via"], json!("refused"));
        assert_eq!(refused["session"], json!("gtm-lane"));
        // A refusal must never be recorded as a successful gmail send.
        assert!(
            entries.iter().all(|x| x.get("via") != Some(&json!("gmail"))),
            "a refused send must not appear as via:gmail"
        );
    }

    #[tokio::test]
    async fn send_happy_path_writes_attributed_audit_and_updates_registry() {
        let home = temp_home();
        let http = MockHttp::new(vec![
            // Guard probe finds no active thread.
            ("GET", "/messages?q=", 200, json!({ "messages": [] })),
            ("GET", "/settings/sendAs", 200, json!({ "sendAs": [] })),
            ("POST", "/messages/send", 200, json!({ "id": "m1", "threadId": "t1" })),
        ]);
        let (app, _d, reg) = app_with(http, home.path());
        let (st, res) = send_req(
            &app,
            "POST",
            "/api/email/send",
            Some(json!({ "to": "x@customer.com", "subject": "Hi", "body": "hello",
                         "from": ACCT, "cc": "" })),
            &[("x-amux-session", "tester-session")],
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{res}");
        assert_eq!(res["ok"], json!(true));
        assert_eq!(res["via"], json!("gmail"));
        assert_eq!(res["from"], json!(ACCT));
        assert_eq!(res["cc"], Value::Null); // Python: cc or None
        assert_eq!(res["id"], json!("m1"));
        assert_eq!(res["thread_id"], json!("t1"));
        // The send-audit ledger recorded the attributed send (AMUX-1897)...
        let (st, log) = send_req(&app, "GET", "/api/email/log?days=1", None, &[]).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(log["count"], json!(1));
        assert_eq!(log["log"][0]["session"], json!("tester-session"));
        assert_eq!(log["log"][0]["endpoint"], json!("send"));
        assert_eq!(log["log"][0]["to"], json!("x@customer.com"));
        // ...and the filter works both ways.
        let (_, none) =
            send_req(&app, "GET", "/api/email/log?session=unattributed", None, &[]).await;
        assert_eq!(none["count"], json!(0));
        // A real successful call is what proves email available (RR-0073).
        assert_eq!(reg.get("email"), Some(IntegrationState::Available));
    }

    #[tokio::test]
    async fn new_thread_guard_blocks_with_candidate_and_force_bypasses() {
        let home = temp_home();
        let meta = json!({
            "id": "g9", "threadId": "T7",
            "payload": { "headers": [
                { "name": "From", "value": "ceo@customer.com" },
                { "name": "Subject", "value": "Pilot" },
                { "name": "Message-ID", "value": "<pilot@x>" },
                { "name": "Date", "value": "Sun, 09 Aug 2026 10:00:00 -0400" },
            ]},
        });
        let http = MockHttp::new(vec![
            ("GET", "/messages?q=", 200, json!({ "messages": [{ "id": "g9" }] })),
            ("GET", "/messages/g9", 200, meta),
        ]);
        let (app, _d, _r) = app_with(http, home.path());
        let (st, e) = send_req(
            &app,
            "POST",
            "/api/email/send",
            Some(json!({ "to": "ceo@customer.com", "subject": "s", "body": "b", "from": ACCT })),
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT, "{e}");
        assert_eq!(e["blocked"], json!(true));
        assert_eq!(e["recipient"], json!("ceo@customer.com"));
        assert_eq!(e["candidate_thread"]["message_id"], json!("<pilot@x>"));
        assert_eq!(e["reply_instead"]["endpoint"], json!("POST /api/email/reply"));
        assert_eq!(e["reply_instead"]["body"]["message_id"], json!("<pilot@x>"));
        assert_eq!(e["or_force"]["force_new_thread"], json!(true));

        // force_new_thread skips the guard and sends.
        let http2 = MockHttp::new(vec![
            ("GET", "/settings/sendAs", 200, json!({ "sendAs": [] })),
            ("POST", "/messages/send", 200, json!({ "id": "m2", "threadId": "t2" })),
        ]);
        let (app2, _d2, _r2) = app_with(http2, home.path());
        let (st, res) = send_req(
            &app2,
            "POST",
            "/api/email/send",
            Some(json!({ "to": "ceo@customer.com", "subject": "s", "body": "b",
                         "from": ACCT, "force_new_thread": true })),
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{res}");
        assert_eq!(res["id"], json!("m2"));
    }

    #[tokio::test]
    async fn reply_dry_run_and_validation() {
        let home = temp_home();
        let (app, _d, _r) = app_with(MockHttp::new(vec![]), home.path());
        // Missing everything -> Python's message.
        let (st, e) = send_req(&app, "POST", "/api/email/reply", Some(json!({})), &[]).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(
            e["error"],
            json!("message_id (or reply_to_latest_from) and body are required")
        );
        // dry_run answers without sending (no mock entries were consumed).
        let (st, res) = send_req(
            &app,
            "POST",
            "/api/email/reply",
            Some(json!({ "message_id": "<m@x>", "dry_run": true })),
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(res["dry_run"], json!(true));
        assert_eq!(res["would_reply_to"], json!("<m@x>"));
        assert_eq!(res["note"], json!("no email sent — repeat without dry_run to send"));
    }

    #[tokio::test]
    async fn reply_happy_path_reports_threading_proof() {
        let home = temp_home();
        let orig = json!({
            "id": "g1", "threadId": "T1",
            "payload": { "headers": [
                { "name": "Message-ID", "value": "<orig@ext>" },
                { "name": "Subject", "value": "Deal" },
                { "name": "From", "value": "p@customer.com" },
                { "name": "To", "value": ACCT },
            ]},
        });
        let http = MockHttp::new(vec![
            ("GET", "q=rfc822msgid", 200, json!({ "messages": [{ "id": "g1" }] })),
            ("GET", "/messages/g1", 200, orig),
            ("GET", "/settings/sendAs", 200, json!({ "sendAs": [] })),
            ("POST", "/messages/send", 200, json!({ "id": "m3", "threadId": "T1" })),
        ]);
        let (app, _d, _reg) = app_with(http, home.path());
        let (st, res) = send_req(
            &app,
            "POST",
            "/api/email/reply",
            Some(json!({ "message_id": "<orig@ext>", "body": "thanks", "from": ACCT })),
            &[("x-amux-worker", "w1")],
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{res}");
        assert_eq!(res["ok"], json!(true));
        assert_eq!(res["message_id"], json!("<orig@ext>"));
        assert_eq!(res["via"], json!("gmail"));
        assert_eq!(res["threaded"], json!(true));
        assert_eq!(res["orig_thread_id"], json!("T1"));
        // Audit line for the reply, attributed via X-Amux-Worker.
        let (_, log) = send_req(&app, "GET", "/api/email/log", None, &[]).await;
        assert_eq!(log["log"][0]["endpoint"], json!("reply"));
        assert_eq!(log["log"][0]["session"], json!("w1"));
        assert_eq!(log["log"][0]["in_reply_to"], json!("<orig@ext>"));
    }

    #[tokio::test]
    async fn search_requires_q_and_inbox_envelope_shape() {
        let home = temp_home();
        let meta = json!({
            "id": "g1", "threadId": "T", "snippet": "snip", "labelIds": ["INBOX"],
            "payload": { "headers": [
                { "name": "From", "value": "a@ext.com" },
                { "name": "To", "value": ACCT },
                { "name": "Subject", "value": "s" },
                { "name": "Date", "value": "Sun, 09 Aug 2026 10:00:00 -0400" },
                { "name": "Message-ID", "value": "<m1@x>" },
            ]},
        });
        let http = MockHttp::new(vec![
            ("GET", "/messages?q=", 200, json!({ "messages": [{ "id": "g1" }] })),
            ("GET", "/messages/g1", 200, meta),
        ]);
        let (app, _d, _r) = app_with(http, home.path());
        let (st, e) = send_req(&app, "GET", "/api/email/search", None, &[]).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(e["error"], json!("q parameter is required"));

        let (st, res) = send_req(
            &app,
            "GET",
            &format!("/api/email/inbox?account={ACCT}&count=5&days=3&envelope=1"),
            None,
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{res}");
        assert_eq!(res["returned"], json!(1));
        assert_eq!(res["truncated"], json!(false));
        assert_eq!(res["window_days"], json!(3.0));
        assert_eq!(res["messages"][0]["message_id"], json!("<m1@x>"));
        assert_eq!(res["messages"][0]["account"], json!(ACCT));
    }

    #[tokio::test]
    async fn auth_failure_marks_email_unavailable_in_registry() {
        let home = temp_home();
        let http = MockHttp::new(vec![
            // Guard probe fails open (no answer -> latest_matching None).
            ("GET", "/messages?q=", 500, json!({ "error": "x" })),
            // Signature fetch fails silently, send hits a revoked token.
            ("GET", "/settings/sendAs", 200, json!({ "sendAs": [] })),
            ("POST", "/messages/send", 401, json!({ "error": "auth" })),
            (
                "FORM",
                "oauth2.googleapis.com/token",
                400,
                json!({ "error": "invalid_grant", "error_description": "revoked" }),
            ),
        ]);
        let (app, _d, reg) = app_with(http, home.path());
        let (st, e) = send_req(
            &app,
            "POST",
            "/api/email/send",
            Some(json!({ "to": "x@customer.com", "subject": "s", "body": "b", "from": ACCT })),
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::BAD_GATEWAY, "{e}");
        match reg.get("email") {
            Some(IntegrationState::Unavailable { reason }) => {
                assert!(reason.contains("invalid_grant"), "{reason}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }
}
