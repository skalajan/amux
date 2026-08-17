//! `/api/sync?since_rev=N` — delta sync (RR-0024, Invariant 35).
//!
//! Returns every StateEvent after revision N, bounded. When the journal no
//! longer holds that window, `full_sync_required: true` — a delta that
//! cannot be complete must say so, never silently omit (Invariant 40).

use super::AppState;
use amux_core::revision::{StateEvent, StateRevision};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

/// Response cap: bounded payloads by design. The Python board's 6MB
/// unbounded responses are the L1 lesson; every list endpoint here is
/// bounded and announces truncation.
const MAX_EVENTS: usize = 2000;

#[derive(Deserialize)]
pub struct SyncParams {
    #[serde(default)]
    pub since_rev: u64,
}

#[derive(Serialize)]
pub struct SyncResponse {
    pub rev: u64,
    pub events: Vec<StateEvent>,
    pub full_sync_required: bool,
    /// True when MAX_EVENTS truncated the answer — the client calls again
    /// with the last event's rev.
    pub more: bool,
}

pub async fn delta_sync(
    State(state): State<AppState>,
    Query(params): Query<SyncParams>,
) -> Result<Json<SyncResponse>, StatusCode> {
    let store = state.store.clone();
    let since = StateRevision(params.since_rev);
    let result = tokio::task::spawn_blocking(move || {
        let current = store.current_rev()?;
        let (events, full_sync_required) = store.events_since(since, MAX_EVENTS + 1)?;
        anyhow::Ok((current, events, full_sync_required))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (current, mut events, full_sync_required) = result;
    let more = events.len() > MAX_EVENTS;
    events.truncate(MAX_EVENTS);
    Ok(Json(SyncResponse {
        rev: current.0,
        events,
        full_sync_required,
        more,
    }))
}
