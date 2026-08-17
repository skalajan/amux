//! /api/proxies — the Proxies tab's CRUD (AMUX-2887).
//!
//! Python contract: `_proxies_list` py:77772, the handlers at py:65636.
//!
//! **Proxies and the tunnel are ONE subsystem, and this card is only half of
//! it.** A "proxy" is a saved public-tunnel target; `/start` and `/stop` drive
//! the tunnel CLIENT (`_tunnel_start`/`_tunnel_loop`, py:77931/77848) — a
//! long-poll relay against the cloud gateway with rate limiting, generation
//! tracking and a security refusal. That client is AMUX-2888 and is not ported.
//!
//! So this module deliberately ships CRUD plus an honest 501 on start/stop
//! rather than either faking them or leaving them 404. The reasoning, since
//! "port or delete" invited the opposite answer:
//! - the Proxies tab is DEFAULT-VISIBLE (`ALL_TABS`, absent from the hidden
//!   set) and its list call 404'd, so the tab reads "Failed to load";
//! - the table has real user data on this machine (PRX-3, "Flask demo");
//! - CRUD that works, with a Start button that says exactly why it cannot
//!   start yet, is strictly better than a dead tab — and unlike a fake, it
//!   cannot be mistaken for a working tunnel.
//!
//! A 501 is the right code here, not 404: the route exists and the capability
//! is real, it is this build that lacks it. `/api/browser/agent` already uses
//! that shape.
//!
//! `live`/`url`/`requests`/`dropped` are reported as the not-running values
//! because no tunnel can be running without the client. When AMUX-2888 lands it
//! fills them from real client state; nothing here should start guessing.

use super::AppState;
use crate::db::board_store as bs;
use crate::db::WriteOutcome;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", axum::routing::patch(patch).delete(remove))
        .route("/{id}/start", post(start))
        .route("/{id}/stop", post(stop))
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

async fn list(State(state): State<AppState>) -> Response {
    let conn = match state.store.read() {
        Ok(c) => c,
        Err(e) => {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": e.to_string()})))
                .into_response()
        }
    };
    let rows: Vec<Value> = conn
        .prepare(
            "SELECT id, name, port, scheme, created_at, last_started \
             FROM proxies ORDER BY created_at ASC",
        )
        .and_then(|mut s| {
            let it = s.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "port": r.get::<_, i64>(2)?,
                    "scheme": r.get::<_, String>(3)?,
                    "created_at": r.get::<_, i64>(4)?,
                    "last_started": r.get::<_, Option<i64>>(5)?,
                    // Not-running values, stated rather than omitted: the SPA
                    // reads `live` to pick the dot and the Start/Stop button.
                    "live": false,
                }))
            })?;
            Ok(it.flatten().collect())
        })
        .unwrap_or_default();

    Json(json!({
        "proxies": rows,
        "active_id": Value::Null,
        // `configured` gates the tab's "not configured" hint. Report the TOKEN's
        // presence honestly — it is a real precondition — while `tunnel_ported`
        // says the other precondition is what is actually missing here.
        "configured": std::env::var("AMUX_TUNNEL_TOKEN").map(|v| !v.trim().is_empty()).unwrap_or(false),
        "self_port": crate::legacy_port::canonical_port(),
        "rate_per_min": env_i64("AMUX_TUNNEL_RATE_PER_MIN", 180),
        "max_concurrent": env_i64("AMUX_TUNNEL_MAX_CONCURRENT", 8),
        "tunnel_ported": false,
        "note": "CRUD is native; the tunnel client that makes a proxy LIVE is not ported yet (AMUX-2888)",
    }))
    .into_response()
}

/// Python validated name + `1 <= port <= 65535` and silently coerced an unknown
/// scheme to http (py:65643). Same rules, because the SPA relies on the coercion
/// rather than sending a scheme on every write.
fn validate(name: &str, port: i64) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("name and a valid port (1-65535) are required");
    }
    if !(1..=65535).contains(&port) {
        return Some("name and a valid port (1-65535) are required");
    }
    None
}

fn scheme_of(v: Option<&str>, fallback: &str) -> String {
    match v.map(|s| s.trim().to_lowercase()) {
        Some(s) if s == "http" || s == "https" => s,
        _ => fallback.to_string(),
    }
}

async fn create(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let name = body["name"].as_str().unwrap_or("").trim().to_string();
    // Python's int(body.get("port")) accepts "5055" as well as 5055; the SPA
    // sends a string from an <input>, so a number-only parse would reject every
    // create the UI makes.
    let port = body["port"]
        .as_i64()
        .or_else(|| body["port"].as_str().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(0);
    if let Some(err) = validate(&name, port) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": err}))).into_response();
    }
    let scheme = scheme_of(body["scheme"].as_str(), "http");
    let ts = now();
    let out = state
        .store
        .write_async(move |conn| {
            let id = bs::next_issue_id(conn, "PRX")?;
            conn.execute(
                "INSERT INTO proxies (id, name, port, scheme, created_at) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![id, name, port, scheme, ts],
            )?;
            Ok(WriteOutcome { applied: true, events: vec![] })
        })
        .await;
    match out {
        Ok(_) => {
            // The id is minted inside the write; read back the newest row so the
            // response can carry it (the SPA does not use it today, but a
            // create that cannot name what it created is a poor contract).
            let id = state
                .store
                .read()
                .ok()
                .and_then(|c| {
                    c.query_row(
                        "SELECT id FROM proxies ORDER BY created_at DESC, rowid DESC LIMIT 1",
                        [],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                })
                .unwrap_or_default();
            (StatusCode::OK, Json(json!({"ok": true, "id": id}))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
            .into_response(),
    }
}

async fn patch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let existing = {
        let Ok(conn) = state.store.read() else {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "store unavailable"})))
                .into_response();
        };
        conn.query_row(
            "SELECT name, port, scheme FROM proxies WHERE id=?1",
            [&id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)),
        )
        .ok()
    };
    let Some((cur_name, cur_port, cur_scheme)) = existing else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    };

    // Every field falls back to its CURRENT value — a PATCH that omits a field
    // must not blank it (Python py:65660 does the same).
    let name = match body["name"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => n.to_string(),
        None => cur_name,
    };
    let port = body["port"]
        .as_i64()
        .or_else(|| body["port"].as_str().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(cur_port);
    if !(1..=65535).contains(&port) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid port"}))).into_response();
    }
    let scheme = scheme_of(body["scheme"].as_str(), &cur_scheme);

    match state
        .store
        .write_async(move |conn| {
            conn.execute(
                "UPDATE proxies SET name=?1, port=?2, scheme=?3 WHERE id=?4",
                rusqlite::params![name, port, scheme, id],
            )?;
            Ok(WriteOutcome { applied: true, events: vec![] })
        })
        .await
    {
        // Python restarted a LIVE proxy whose port changed. Nothing can be live
        // without the tunnel client, so there is nothing to restart — and
        // pretending otherwise is the fake this module refuses to ship.
        Ok(_) => Json(json!({"ok": true})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
            .into_response(),
    }
}

async fn remove(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state
        .store
        .write_async(move |conn| {
            let n = conn.execute("DELETE FROM proxies WHERE id=?1", [&id])?;
            Ok(WriteOutcome { applied: n > 0, events: vec![] })
        })
        .await
    {
        Ok(_) => Json(json!({"ok": true})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
            .into_response(),
    }
}

/// 501, not 404 and not a lie. The route exists, the capability is real, and
/// THIS build is what lacks it — which is exactly what 501 means. The body says
/// which card carries the missing half so nobody has to go and find out whether
/// proxies are broken or unfinished.
fn tunnel_not_ported(verb: &str) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "ok": false,
            "error": format!("cannot {verb} a proxy: the tunnel client is not ported to the Rust server yet"),
            "card": "AMUX-2888",
            "why": "a proxy's start/stop drives the cloud tunnel relay (py:77931 _tunnel_start / py:77848 _tunnel_loop), \
                    not just a database row. Proxy CRUD is native; the relay is not.",
            "security_note": "when it is ported it MUST keep python's refusal to tunnel amux's own port — the local \
                              control plane is unauthenticated, so exposing it publicly is unauthenticated RCE on \
                              YOLO sessions (py:77943, override only via AMUX_TUNNEL_ALLOW_SELF=1).",
        })),
    )
        .into_response()
}

async fn start(Path(_id): Path<String>) -> Response {
    tunnel_not_ported("start")
}
async fn stop(Path(_id): Path<String>) -> Response {
    tunnel_not_ported("stop")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_validation_matches_python_bounds() {
        assert!(validate("api", 1).is_none());
        assert!(validate("api", 65535).is_none());
        // The controls: each of these is a distinct way the UI can send junk.
        assert!(validate("api", 0).is_some(), "0 is not a port");
        assert!(validate("api", 65536).is_some(), "above the u16 range");
        assert!(validate("api", -1).is_some(), "negative");
        assert!(validate("", 8080).is_some(), "a nameless proxy is unusable in the list");
        assert!(validate("   ", 8080).is_some(), "whitespace is not a name");
    }

    #[test]
    fn scheme_coerces_unknown_values_rather_than_rejecting_them() {
        assert_eq!(scheme_of(Some("https"), "http"), "https");
        assert_eq!(scheme_of(Some("HTTPS"), "http"), "https");
        // Python coerces silently and the SPA depends on it — it omits `scheme`
        // on most writes, so rejecting would break every edit from the UI.
        assert_eq!(scheme_of(Some("ftp"), "http"), "http");
        assert_eq!(scheme_of(None, "https"), "https", "absent keeps the CURRENT scheme");
        assert_eq!(scheme_of(Some(""), "https"), "https");
    }
}
