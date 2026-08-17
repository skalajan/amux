//! /api/sql — the SQL tab: schema browse, row browse, and query execution.
//!
//! Never ported; the tab is visible by default, so clicking it showed a dead
//! panel (route census, AMUX-2871).
//!
//! THE READ/WRITE SPLIT IS ENFORCED BY SQLITE, NOT BY READING THE SQL.
//! The client sends `{sql, write}` and a naive port would gate on a prefix
//! check — but `SELECT 1; DROP TABLE issues` passes any prefix check, and a
//! blocklist of keywords is a guessing game whose failure mode is silent data
//! loss on the live board. Instead a non-write query runs on a connection
//! opened SQLITE_OPEN_READ_ONLY: the engine refuses the write, so the guard
//! cannot be out-thought by a query shape nobody predicted. That is the
//! structurally-absent signal the ethos file prefers over a tuned parameter.

use super::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::types::ValueRef;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::post(run_sql))
        .route("/schema", get(schema))
        .route("/rows", get(rows))
}

fn db_path() -> std::path::PathBuf {
    std::env::var("AMUX_DB").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        crate::api::session_verbs::home().join("amux.db")
    })
}

/// Cell -> JSON. NULL stays null rather than becoming "" — a browser that
/// cannot tell an empty string from a NULL is lying about the data it exists
/// to show.
fn cell(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => json!(format!("<{} bytes>", b.len())),
    }
}

fn rows_to_json(stmt: &mut rusqlite::Statement<'_>) -> rusqlite::Result<(Vec<String>, Vec<Value>)> {
    let cols: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
    let mut out = Vec::new();
    let mut q = stmt.query([])?;
    while let Some(r) = q.next()? {
        let mut obj = serde_json::Map::new();
        for (i, name) in cols.iter().enumerate() {
            obj.insert(name.clone(), cell(r.get_ref(i)?));
        }
        out.push(Value::Object(obj));
    }
    Ok((cols, out))
}

async fn schema(State(state): State<AppState>) -> Response {
    let conn = match state.store.read() {
        Ok(c) => c,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    };
    let mut tables: BTreeMap<String, Value> = BTreeMap::new();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(0)).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    for t in names {
        let cols: Vec<Value> = conn
            .prepare(&format!("PRAGMA table_info(\"{}\")", t.replace('"', "\"\"")))
            .and_then(|mut s| {
                s.query_map([], |r| {
                    Ok(json!({
                        "name": r.get::<_, String>(1)?,
                        "type": r.get::<_, String>(2)?,
                        "notnull": r.get::<_, i64>(3)? == 1,
                        "pk": r.get::<_, i64>(5)? > 0,
                    }))
                })
                .map(|r| r.flatten().collect())
            })
            .unwrap_or_default();
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM \"{}\"", t.replace('"', "\"\"")), [], |r| r.get(0))
            .unwrap_or(-1);
        tables.insert(t, json!({ "columns": cols, "rows": count }));
    }
    Json(json!({ "tables": tables, "path": db_path().to_string_lossy() })).into_response()
}

#[derive(serde::Deserialize)]
pub struct RowsParams {
    table: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn rows(State(state): State<AppState>, Query(p): Query<RowsParams>) -> Response {
    let Some(table) = p.table.filter(|t| !t.is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "table required" }))).into_response();
    };
    let conn = match state.store.read() {
        Ok(c) => c,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    };
    // The table name cannot be bound as a parameter, so it is verified against
    // sqlite_master rather than escaped-and-hoped. An identifier that is not a
    // real table never reaches a query string.
    let known: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [&table],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !known {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no such table: {table}") })))
            .into_response();
    }
    let limit = p.limit.unwrap_or(100).min(1000);
    let offset = p.offset.unwrap_or(0);
    let sql = format!("SELECT * FROM \"{}\" LIMIT {} OFFSET {}", table.replace('"', "\"\""), limit, offset);
    match conn.prepare(&sql).and_then(|mut s| rows_to_json(&mut s)) {
        Ok((columns, rows)) => Json(json!({ "columns": columns, "rows": rows, "table": table })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn run_sql(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let sql = body.get("sql").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if sql.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "sql required" }))).into_response();
    }
    let write = body.get("write").and_then(Value::as_bool).unwrap_or(false);

    if !write {
        // READ-ONLY BY CONSTRUCTION. Not a keyword check — the engine refuses.
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI;
        let conn = match rusqlite::Connection::open_with_flags(db_path(), flags) {
            Ok(c) => c,
            Err(e) => {
                return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": e.to_string() })))
                    .into_response()
            }
        };
        return match conn.prepare(&sql).and_then(|mut s| rows_to_json(&mut s)) {
            Ok((columns, rows)) => {
                Json(json!({ "columns": columns, "rows": rows, "readonly": true })).into_response()
            }
            // SQLITE_READONLY surfaces here with its own message; pass it
            // through rather than rewriting it, so a refused write says it was
            // refused instead of looking like a syntax error.
            Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response(),
        };
    }

    // write: true — the user asked for it explicitly via the UI toggle. Goes
    // through the store's writer so it serialises with every other mutation
    // rather than racing them on a second connection.
    let sql2 = sql.clone();
    let changed = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let cw = changed.clone();
    let res = state
        .store
        .write_async(move |conn| {
            let n = conn.execute_batch(&sql2).map(|_| conn.changes() as usize)?;
            *cw.lock().expect("slot") = n;
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        })
        .await;
    match res {
        Ok(_) => {
            let n = *changed.lock().expect("slot");
            Json(json!({ "ok": true, "changed": n, "readonly": false })).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}
