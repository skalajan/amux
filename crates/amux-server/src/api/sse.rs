//! `/api/events` — SSE (RR-0023/RR-0042, Invariants 35/26).
//!
//! Speaks BOTH dialects on one stream:
//! - Modern: revisioned StateEvents (`{"type":"state",...}`) + gap
//!   detection (`lagged`) + `hello` carrying the current rev.
//! - Legacy: the Python dashboard's own event vocabulary —
//!   `{"type":"board","payload":[...]}` and `{"type":"sessions",
//!   "payload":[...]}` full-list pushes, coalesced. Without these the SPA
//!   CONNECTS (so its polling fallback disarms) and then understands
//!   nothing — strictly worse than no SSE at all, which is exactly what
//!   the browser-golden run demonstrated. The 10s `{"type":"ping"}`
//!   keep-alive serves both dialects (the SPA's 18s staleness detector
//!   feeds on it).

use super::AppState;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use futures::StreamExt;
use std::convert::Infallible;
use std::time::Duration;

pub async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.store.subscribe();
    let current = state.store.current_rev().map(|r| r.0).unwrap_or(0);
    let store = state.store.clone();

    let stream = async_stream(current, move |yielder: tokio::sync::mpsc::Sender<Event>| async move {
        // Initial full pushes so a fresh client renders without a poll.
        push_legacy(&store, &yielder, true, true).await;
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let payload = serde_json::json!({
                        "type": "state",
                        "payload": ev,
                    });
                    if yielder
                        .send(Event::default().data(payload.to_string()))
                        .await
                        .is_err()
                    {
                        break; // client went away
                    }
                    // Legacy dialect: coalesce this event plus everything
                    // already queued (a burst of N writes = one list push).
                    let mut board_dirty = matches!(
                        ev.entity_type,
                        amux_core::revision::EntityType::Task
                    );
                    let mut sessions_dirty = matches!(
                        ev.entity_type,
                        amux_core::revision::EntityType::Worker
                            | amux_core::revision::EntityType::Session
                    );
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    while let Ok(more) = rx.try_recv() {
                        board_dirty |= matches!(
                            more.entity_type,
                            amux_core::revision::EntityType::Task
                        );
                        sessions_dirty |= matches!(
                            more.entity_type,
                            amux_core::revision::EntityType::Worker
                                | amux_core::revision::EntityType::Session
                        );
                    }
                    if (board_dirty || sessions_dirty)
                        && !push_legacy(&store, &yielder, board_dirty, sessions_dirty).await
                    {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    // Backpressure (Invariant 26): a slow client that missed
                    // events is TOLD so, with the count — it must delta-sync,
                    // not assume continuity. Legacy clients get fresh full
                    // lists, which IS their recovery.
                    let payload = serde_json::json!({
                        "type": "lagged",
                        "missed": missed,
                    });
                    if yielder
                        .send(Event::default().data(payload.to_string()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    if !push_legacy(&store, &yielder, true, true).await {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    Sse::new(stream).keep_alive(
        // The keep-alive IS the ping contract: 10s cadence, real data event.
        // `v` = the APP_VER of the app.js THIS binary embeds (Python parity,
        // amux-server.py:65292): the SPA's ping handler self-reloads on
        // mismatch (rate-limited, SW-nudged), which is how clients adopt a
        // new build without a human clicking anything — the server restarts
        // on deploy, the next ping carries the new version, every open
        // window follows. Ethan 2026-08-09: clients restart like the Python
        // server's, not behind a banner.
        // .event(data), NOT .text(): KeepAlive::text() is literally
        // Event::default().comment(t), and an SSE COMMENT never reaches
        // EventSource.onmessage — so the ping parsed right on the wire and
        // was invisible to the client: self-reload unreachable, _lastDataTime
        // starved, and the SPA's 18s zombie detector reconnect-looped on any
        // quiet fleet (browser re-verify, 2026-08-09: 2 comment-pings, 0
        // data-pings in 32s of raw wire). Axum keep-alives fire only after
        // `interval` of SILENCE, unlike Python's unconditional 10s ping —
        // acceptable, because flowing data events feed _lastDataTime
        // themselves; the ping covers exactly the quiet gaps.
        KeepAlive::new()
            .interval(Duration::from_secs(10))
            .event(Event::default().data(ping_payload())),
    )
}

/// `{"type":"ping","v":"<APP_VER>"}` with the version parsed ONCE from the
/// embedded app.js — the same file this server serves, so client and ping can
/// never disagree about what "current" means. Falls back to a bare ping if
/// the constant ever moves (a missing `v` disables self-reload, it does not
/// break liveness).
fn ping_payload() -> &'static str {
    static PAYLOAD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PAYLOAD.get_or_init(|| {
        let ver = amux_dashboard::DashboardAssets::get("app.js")
            .and_then(|f| {
                let s = String::from_utf8_lossy(&f.data).into_owned();
                s.split("const APP_VER = '")
                    .nth(1)
                    .and_then(|rest| rest.split('\'').next().map(String::from))
            })
            .unwrap_or_default();
        if ver.is_empty() {
            "{\"type\":\"ping\"}".to_string()
        } else {
            format!("{{\"type\":\"ping\",\"v\":\"{ver}\"}}")
        }
    })
}

/// Push the legacy full-list events. Returns false when the client is gone.
async fn push_legacy(
    store: &crate::db::SharedStore,
    yielder: &tokio::sync::mpsc::Sender<Event>,
    board: bool,
    sessions: bool,
) -> bool {
    if board {
        let payload = {
            let s = store.clone();
            tokio::task::spawn_blocking(move || legacy_board_json(&s))
                .await
                .ok()
                .flatten()
        };
        if let Some(json) = payload {
            let ev = format!("{{\"type\":\"board\",\"payload\":{json}}}");
            if yielder.send(Event::default().data(ev)).await.is_err() {
                return false;
            }
        }
    }
    if sessions {
        let payload = {
            let s = store.clone();
            tokio::task::spawn_blocking(move || legacy_sessions_json(&s))
                .await
                .ok()
                .flatten()
        };
        if let Some(json) = payload {
            let ev = format!("{{\"type\":\"sessions\",\"payload\":{json}}}");
            if yielder.send(Event::default().data(ev)).await.is_err() {
                return false;
            }
        }
    }
    true
}

fn legacy_board_json(store: &crate::db::SharedStore) -> Option<String> {
    let conn = store.read().ok()?;
    let rows = crate::db::board_store::list_issues(
        &conn,
        &[],
        &[],
        crate::db::board_store::ArchivedFilter::ActiveOnly,
    )
    .ok()?;
    // Python's SSE board channel (amux-server.py:65222-65249): the
    // _load_board(100) TERMINAL QUOTAS (verified gets its own 300-floor so
    // bulk-verifies stay visible), non-archived only, desc slimmed to its
    // first line — NOT the REST path's lumped cap_terminal(100).
    let kept = crate::db::board_store::sse_terminal_quota(rows, 100);
    // Same stale derivation as GET /api/board (a view must share its
    // mechanism's predicate) — skipped when no in-progress card needs it.
    let working = if kept
        .iter()
        .any(|r| matches!(r.status.as_str(), "doing" | "review"))
    {
        crate::api::sessions_legacy::active_python_sessions(&conn)
    } else {
        Default::default()
    };
    let now = chrono::Utc::now().timestamp();
    let items: Vec<serde_json::Value> = kept
        .iter()
        .map(|r| {
            crate::api::board::list_body(
                r,
                true,
                crate::api::board::is_stale(r, now, &working),
            )
        })
        .collect();
    serde_json::to_string(&items).ok()
}

fn legacy_sessions_json(store: &crate::db::SharedStore) -> Option<String> {
    crate::api::sessions_legacy::legacy_sessions_array(store).ok()
}

/// Bridge an async producer into an SSE stream, prefixed with a `hello`
/// event carrying the current revision so the client knows exactly where
/// the live stream begins (and what to pass to /api/sync).
fn async_stream<F, Fut>(
    current_rev: u64,
    producer: F,
) -> impl Stream<Item = Result<Event, Infallible>>
where
    F: FnOnce(tokio::sync::mpsc::Sender<Event>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(256);
    let hello = Event::default().data(
        serde_json::json!({"type": "hello", "rev": current_rev}).to_string(),
    );
    tokio::spawn(producer(tx));
    futures::stream::once(async move { Ok(hello) }).chain(futures::stream::unfold(
        rx,
        |mut rx| async move {
            rx.recv().await.map(|ev| (Ok(ev), rx))
        },
    ))
}

#[cfg(test)]
mod ping_tests {
    #[test]
    fn ping_carries_the_embedded_app_ver() {
        let p = super::ping_payload();
        // The version must be the one inside the EMBEDDED app.js — not a
        // hardcoded copy that drifts on the next client bump.
        let f = amux_dashboard::DashboardAssets::get("app.js").unwrap();
        let s = String::from_utf8_lossy(&f.data).into_owned();
        let ver = s
            .split("const APP_VER = '")
            .nth(1)
            .and_then(|r| r.split('\'').next())
            .expect("APP_VER present in embedded app.js");
        assert_eq!(p, &format!("{{\"type\":\"ping\",\"v\":\"{ver}\"}}"));
    }
}
