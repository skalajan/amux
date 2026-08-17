//! /api/habits — the Habits tab's whole state, one JSON array in
//! `~/.amux/habits.json` (AMUX-2871).
//!
//! Python contract (amux-server.py:66494 at 792ce1f^, `CC_HABITS` at py:68):
//! GET returns the array (`[]` when the file is absent or unparseable), PUT
//! replaces it wholesale and rejects anything that is not an array.
//!
//! This one was losing data, not merely failing to render. `_habitsSave()`
//! PUTs and never checks `r.ok`, so with the route unmounted every tick, add
//! and rename since the Rust cutover was discarded with no error anywhere. The
//! file on this machine was last written 2026-07-04 — before the cutover —
//! which is what the silence looks like from outside.
//!
//! The write is atomic (tmp + rename) because the whole tab is ONE file: a
//! partial write is not a lost edit, it is every habit and every day of history
//! gone at once.

use super::AppState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/habits", get(load).put(save))
}

fn habits_path() -> PathBuf {
    crate::api::session_verbs::home().join("habits.json")
}

async fn load() -> Response {
    let value = std::fs::read_to_string(habits_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([]));
    Json(value).into_response()
}

async fn save(Json(body): Json<Value>) -> Response {
    if !body.is_array() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "expected array"})),
        )
            .into_response();
    }
    let path = habits_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = match serde_json::to_string(&body) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let tmp = path.with_extension("json.tmp");
    let write = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path));
    match write {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            // Name the path. "Permission denied" alone sent a reader looking at
            // the server rather than at ~/.amux.
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("could not write {}: {e}", path.display())})),
            )
                .into_response()
        }
    }
}
