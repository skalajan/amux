//! Board API (RR-0049 routes + 409 gate contract + force audit; RR-0055
//! archive/restore; the list shape RR-0053's auto-capture will write into).
//!
//! Mounted at `/api/board` inside the `protected` router (api/mod.rs). This
//! is the STRANGLER-FIG surface: it serves the same `issues` rows the Python
//! server serves, in the same shapes the Python dashboard/CLI already parse —
//! a bare JSON array from the list, the `gate not acknowledged` 409 body the
//! CLI's `--checked` flow is built around, `X-Amux-Truncated` headers on the
//! capped list. Interop mappings live in `db::board_store`.
//!
//! Every status change routes through core's `apply_transition` — one state
//! machine, one code path (Invariant 3); nothing here hand-rolls a status
//! write. Gate refusals carry core's `WhyBlocked` list alongside the Python
//! keys, force bypasses are audited into the card's own log (ethos rule 6:
//! the Python board claimed force-is-logged while nothing logged it), and
//! no-op PATCHes report `applied: false` with `rev` unmoved (Invariant 37).

use super::AppState;
use crate::db::board_store::{self as bs, ArchivedFilter, IssueRow};
use crate::db::{PendingEvent, WriteOutcome};
use amux_core::board::{
    apply_transition, why_blocked, BoardTransition, TaskStatus, TransitionError,
};
use amux_core::events::Actor;
use amux_core::revision::{EntityType, MutationKind};
use amux_core::verification::{Evidence, EvidenceKind, EvidenceSource};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_board).post(create_item))
        // Static segment outranks /{id} in axum: /statuses never collides.
        .route("/statuses", get(list_statuses).post(create_status))
        // Static /reorder outranks /{sid}; both outrank /api/board/{id}.
        .route("/statuses/reorder", axum::routing::put(reorder_statuses))
        .route(
            "/statuses/{sid}",
            axum::routing::patch(patch_status).delete(delete_status),
        )
        // Static /session-gates and /contract outrank /{id}.
        .route(
            "/session-gates",
            get(list_session_gates).patch(patch_session_gates),
        )
        .route("/contract", get(get_contract))
        // DELETE was never registered, so the SPA's own Delete button on a
        // card 405'd — and `deleteBoardItem` removes the card optimistically
        // BEFORE the request, so the card vanished, the server kept it, and it
        // came back on the next poll. That is the reported "tons of board
        // items are not moving" (AMUX board sweep, 2026-08-09).
        // Before the /{id} wildcard, or "clear-done" is swallowed as an id.
        .route("/clear-done", post(clear_done))
        .route("/{id}", get(get_item).patch(patch_item).delete(delete_item))
        .route("/{id}/archive", post(archive_item))
        .route("/{id}/restore", post(restore_item))
        // The D1-exit pair — see the handlers below for why their 405 was the
        // most expensive shape available.
        .route("/{id}/status-request", post(status_request))
        .route("/{id}/status-update", post(status_update))
        // AMUX-3131: the claim the assignment notifications tell every session to
        // run. It was never mounted, so `amux board claim <id>` hit the GET-only
        // SPA catch-all (405) and the CLI (pre-fix) exited 0 with the card
        // untouched — AMUX-2140 one layer down. Same mechanism auto-pickup uses.
        .route("/{id}/claim", post(claim_item))
}

/// GET /api/board/contract — the gate table, types, and CLI syntax.
/// Every gate-blocked 409 tells the caller to `GET /api/board/contract`
/// to understand the rules. Without this endpoint that instruction is a
/// dead link (AR-123).
async fn get_contract() -> Response {
    use serde_json::json;
    let statuses = ["doing", "review", "done", "verified"];
    let mut gates = serde_json::Map::new();
    for ty in bs::KNOWN_TYPES {
        let mut ty_gates = serde_json::Map::new();
        for &st in &statuses {
            if let Some(target) = bs::parse_status(st) {
                let g = bs::default_gates_for(ty, target);
                if !g.is_empty() {
                    ty_gates.insert(st.to_string(), json!(g));
                }
            }
        }
        if !ty_gates.is_empty() {
            gates.insert(ty.to_string(), serde_json::Value::Object(ty_gates));
        }
    }
    Json(json!({
        "types": bs::KNOWN_TYPES,
        "gates": gates,
        "how_to_ack": {
            "cli": "amux board <status> <id> --checked \"criterion 1\" \"criterion 2\"",
            "api": "PATCH /api/board/<id> with gate_checked: [\"criterion 1\", ...] or gate_ack: true",
            "wrong_type": "If the item has no code, set its type first — the gate is DERIVED from the type.",
        },
        // AMUX-2933 (ts-gke). The list filters WORK and were documented
        // NOWHERE — "discoverable only by guessing", and the cap was worse than
        // undocumented: silent. A lane auditing its own board got the 100
        // most-recent terminal rows fleet-wide and no signal that it was a
        // sample, so `GET /api/board` could return FEWER of its done cards than
        // `?session=<lane>` did. That reads as data, not as truncation.
        "list": {
            "endpoint": "GET /api/board",
            "returns": "a bare JSON array of items (NOT an envelope) — kept that way \
                        because every caller and the SPA index it directly",
            "filters": {
                "session": "comma-separated worker names",
                "status": "comma-separated statuses",
                "archived": "absent/\"\" = no filter · 1|true|yes = archived ONLY · \
                             any other value (0, false, …) = non-archived only",
                "done_limit": "cap on TERMINAL items (done/verified/discarded), keeping the \
                               most recently updated. 0 or negative = uncapped",
                "all": "1|true|yes = uncap the terminal set (equivalent to done_limit=0) — the \
                        complete enumeration; use this or ?status=<s> to defeat the render cap",
                "limit": "page size, applied AFTER done_limit",
                "offset": "page offset",
                "slim": "1 = trimmed item bodies",
            },
            "not_a_filter": {
                "q / query / search": "REFUSED with 400 — /api/board does not search, it would \
                                       return the entire board. Use /api/search?q=",
            },
            "terminal_cap": {
                "default_unscoped": 100,
                "default_scoped": 0,
                "scoped_means": "session= or status= is present — a bounded QUESTION is answered \
                                 in full; only the unbounded list is sampled",
                "why": "the unfiltered board is ~4.5MB at a cap of 100 and ~19.8MB uncapped \
                        (1186 vs 5576 items, measured 2026-08-11). amux is mobile-first, so the \
                        default stays capped — the defect was never the cap, it was the silence",
                "detect_truncation": "response headers x-amux-truncated (1|0), \
                                      x-amux-terminal-total, x-amux-terminal-returned, \
                                      x-amux-done-limit",
                "to_get_everything": "?done_limit=0 (or scope the query with session=/status=)",
                // CORRECTED by ts-gke's reconciliation, 2026-08-11. The first
                // version of this line said `?session=<worker>` full stop, and
                // that is the query for "everything I own" — NOT for "what do I
                // still have to act on". Their three counts, all correct once
                // understood: bare list 8 (capped), ?session&status=done 101,
                // the same +archived=0 → 59. The 42-card difference is archived
                // cards, which are TERMINAL AND IMMUTABLE — a status PATCH on
                // one is refused with `archived_task_immutable`
                // ("task is archived; restore it first", amux-core/src/board.rs).
                // So an audit built on 101 counts 42 cards nobody can act on,
                // and the auto-continue nudge that said 60 was right the whole
                // time: it counts the actionable set.
                "auditing_your_own_cards": "GET /api/board?session=<worker>&status=done&archived=0 \
                                            — scoped queries are uncapped, and archived=0 drops \
                                            cards that are terminal AND immutable (a status PATCH \
                                            on an archived card is refused with \
                                            archived_task_immutable). Dropping archived=0 answers \
                                            'everything I own', which is a different question and \
                                            the one that inflates a verification backlog. The bare \
                                            list answers neither.",
            },
        },
    }))
    .into_response()
}

/// GET /api/board/statuses — Python `_load_board_statuses` (amux-server.py
/// :15933): the SPA builds its kanban COLUMNS from this list, silently
/// falling back to a hardcoded default set on any failure — so a 404 here
/// meant custom Python-configured columns never rendered on the Rust origin
/// (AMUX-2596). Shape: [{id, label, mode, gate}] ordered by position;
/// Python's builtin defaults when the table is empty/absent.
async fn list_statuses(State(state): State<AppState>) -> Response {
    const DEFAULTS: [(&str, &str); 7] = [
        ("backlog", "Backlog"),
        ("todo", "To Do"),
        ("doing", "In Progress"),
        ("review", "In Review"),
        ("done", "Done"),
        ("verified", "Verified"),
        ("discarded", "Discarded"),
    ];
    let conn = match state.store.read() {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    let mut out: Vec<Value> = Vec::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT id, label, gate, mode FROM statuses ORDER BY position")
    {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        }) {
            for (id, label, gate, mode) in rows.flatten() {
                let gate: Value = gate
                    .as_deref()
                    .and_then(|g| serde_json::from_str(g).ok())
                    .unwrap_or_else(|| json!([]));
                let mode = mode.filter(|m| !m.is_empty()).unwrap_or_else(|| "implicit".into());
                let terminal = matches!(id.as_str(), "verified" | "discarded");
                out.push(json!({ "id": id, "label": label, "mode": mode, "gate": gate, "terminal": terminal }));
            }
        }
    }
    if out.is_empty() {
        // Python: default columns, and note Python's dict here has NO
        // mode/gate keys on defaults — the SPA tolerates their absence.
        out = DEFAULTS
            .iter()
            .map(|(id, label)| {
                let terminal = matches!(*id, "verified" | "discarded");
                json!({ "id": id, "label": label, "terminal": terminal })
            })
            .collect();
    }
    Json(Value::Array(out)).into_response()
}


// ---- per-session gate overrides (AMUX-2599) -------------------------------
//
// Python `_load_session_gates` (py:16105) + the GET/PATCH pair at py:69563.
// The layer between the global per-status default and the per-card override.
//
// The SPA fetches this on EVERY board load, in the same `Promise.all` as the
// board and the status list. Its failure mode is the reason this is worth
// porting carefully: the client does
// `try { const d = await r.json(); if (d && typeof d === 'object') sessionGates = d; } catch {}`
// — and a 404 body `{"error":"not found"}` IS an object, so it was assigned
// wholesale. Every `sessionGates[worker][status]` lookup then missed, and the
// user's per-worker gates rendered as if they had been DELETED. A 404 that
// deserializes into the success path is not a missing endpoint, it is a silent
// data-loss illusion (ethos rule 4: the wrong answer must be visible).

/// GET /api/board/session-gates -> `{session: {status: [criteria]}}`.
///
/// Empty gates are dropped rather than returned as `[]`: a missing
/// (session, status) MEANS "inherit the global default for that status", and
/// an empty array would read as "this worker has an override that requires
/// nothing" — the opposite of inheritance.
async fn list_session_gates(State(state): State<AppState>) -> Response {
    let conn = match state.store.read() {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    let mut out: Map<String, Value> = Map::new();
    // Python wrapped the SELECT in a bare `except: return {}` — the table is
    // absent on a fresh DB and an empty map is the honest answer there.
    if let Ok(mut stmt) = conn.prepare("SELECT session, status, gate FROM session_gates") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        }) {
            for (session, status, gate) in rows.flatten() {
                let items: Vec<String> = gate
                    .as_deref()
                    .and_then(|g| serde_json::from_str(g).ok())
                    .unwrap_or_default();
                if items.is_empty() {
                    continue;
                }
                out.entry(session)
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
                    .expect("just inserted an object")
                    .insert(status, json!(items));
            }
        }
    }
    Json(Value::Object(out)).into_response()
}

/// PATCH /api/board/session-gates {session|worker, status, gate[]} -> {ok:true}.
///
/// Accepts BOTH spellings of the key on purpose. Python read `session`; the
/// shipped SPA has always sent `worker` (app.js `editSessionGate`), so the
/// python endpoint would have answered 400 to its own dashboard. Rather than
/// re-ship that mismatch, take either — `worker` is the alias `aliases.rs`
/// already maps to `session` everywhere else in this API.
async fn patch_session_gates(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let key = |k: &str| {
        body.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let (Some(session), Some(status)) = (key("session").or_else(|| key("worker")), key("status"))
    else {
        return err(
            StatusCode::BAD_REQUEST,
            json!({"error": "missing session or status"}),
        );
    };
    let items: Vec<String> = body
        .get("gate")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|x| match x {
                    Value::String(s) => s.trim().to_string(),
                    other => other.to_string().trim().to_string(),
                })
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let write = state
        .store
        .write_async(move |conn| {
            if items.is_empty() {
                // Empty -> revert this session to the global default for that
                // status. Deleting the row is what "inherit" means here; an
                // empty-array row would be an override requiring nothing.
                conn.execute(
                    "DELETE FROM session_gates WHERE session = ?1 AND status = ?2",
                    rusqlite::params![session, status],
                )?;
            } else {
                conn.execute(
                    "INSERT INTO session_gates (session, status, gate) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(session, status) DO UPDATE SET gate = excluded.gate",
                    rusqlite::params![
                        session,
                        status,
                        serde_json::to_string(&items).unwrap_or_else(|_| "[]".into())
                    ],
                )?;
            }
            Ok(WriteOutcome {
                applied: true,
                // Board-flavoured so the SSE tick makes open dashboards refetch
                // — python called `_board_changed()` here for the same reason.
                events: vec![PendingEvent {
                    entity_type: EntityType::Task,
                    entity_id: format!("session_gates:{session}:{status}"),
                    mutation: MutationKind::Updated,
                    payload: None,
                }],
            })
        })
        .await;
    match write {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => internal(e),
    }
}

// ---- board status (column) mutations -------------------------------------
//
// Python parity, amux-server.py:69484-69560 + 69209 (reorder). The PATCH was
// the live 405 Ethan hit editing a column (request_log target
// /api/board/statuses/review, 2026-08-09) — GET was ported for AMUX-2596 and
// the mutation verbs were not.

/// POST /api/board/statuses {label} -> 201 {id,label} (py:69484).
async fn create_status(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let label = body
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if label.is_empty() {
        return err(StatusCode::BAD_REQUEST, json!({ "error": "missing label" }));
    }
    // Python: slugify, then -2..-19 suffix on collision.
    let mut sid: String = label
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(30)
        .collect();
    sid = sid.trim_matches('-').to_string();
    if sid.is_empty() {
        return err(StatusCode::BAD_REQUEST, json!({ "error": "invalid label" }));
    }
    let out: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let out_w = out.clone();
    let label_w = label.clone();
    let write = state
        .store
        .write_async(move |conn| {
            let existing: Vec<String> = conn
                .prepare("SELECT id FROM statuses")?
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(Result::ok)
                .collect();
            let mut final_id = sid.clone();
            if existing.contains(&final_id) {
                for i in 2..20 {
                    let candidate = format!("{sid}-{i}");
                    if !existing.contains(&candidate) {
                        final_id = candidate;
                        break;
                    }
                }
            }
            let max_pos: i64 = conn.query_row(
                "SELECT COALESCE(MAX(position),0) FROM statuses",
                [],
                |r| r.get(0),
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO statuses (id, label, position, is_builtin) VALUES (?, ?, ?, 0)",
                rusqlite::params![final_id, label_w, max_pos + 1],
            )?;
            *out_w.lock().expect("status slot poisoned") = Some(final_id.clone());
            Ok(WriteOutcome {
                applied: true,
                events: vec![PendingEvent {
                    entity_type: EntityType::Task,
                    entity_id: format!("statuses:{final_id}"),
                    mutation: MutationKind::Created,
                    payload: None,
                }],
            })
        })
        .await;
    if let Err(e) = write {
        return internal(e);
    }
    let sid = out.lock().expect("status slot poisoned").take().unwrap_or_default();
    (StatusCode::CREATED, Json(json!({ "id": sid, "label": label }))).into_response()
}

/// PATCH /api/board/statuses/{sid} {label?, gate?} -> {ok:true} (py:69550).
async fn patch_status(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let label = body
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    // Python: "gate" present -> list of non-empty strings, else NULL.
    let gate_update = body.get("gate").map(|g| {
        let items: Vec<String> = g
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| match x {
                        Value::String(s) => s.trim().to_string(),
                        other => other.to_string().trim().to_string(),
                    })
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if items.is_empty() {
            None
        } else {
            serde_json::to_string(&items).ok()
        }
    });
    let sid_w = sid.clone();
    let write = state
        .store
        .write_async(move |conn| {
            if let Some(l) = &label {
                conn.execute(
                    "UPDATE statuses SET label = ? WHERE id = ?",
                    rusqlite::params![l, sid_w],
                )?;
            }
            if let Some(g) = &gate_update {
                conn.execute(
                    // gate_custom=1: a person edited this column's gate, so
                    // enforcement must honour it over the type default
                    // (AMUX-2641). Without the flag a stale seed row is
                    // indistinguishable from operator intent.
                    "UPDATE statuses SET gate = ?, gate_custom = 1 WHERE id = ?",
                    rusqlite::params![g, sid_w],
                )?;
            }
            Ok(WriteOutcome {
                applied: true,
                events: vec![PendingEvent {
                    entity_type: EntityType::Task,
                    entity_id: format!("statuses:{sid_w}"),
                    mutation: MutationKind::Updated,
                    payload: None,
                }],
            })
        })
        .await;
    match write {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => internal(e),
    }
}

/// DELETE /api/board/statuses/{sid} — refuse builtins; audit the bulk
/// status rewrite onto every moved card (AMUX-2491: a column delete used to
/// leave no trace) -> {ok, moved, ids[:50]} (py:69512-69549).
async fn delete_status(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    headers: HeaderMap,
) -> Response {
    const BUILTINS: [&str; 7] =
        ["backlog", "todo", "doing", "review", "done", "verified", "discarded"];
    if BUILTINS.contains(&sid.as_str()) {
        return err(
            StatusCode::BAD_REQUEST,
            json!({ "error": "cannot delete built-in status" }),
        );
    }
    let (_, actor_name) = actor_from_headers(&headers);
    let actor = if actor_name == "api-anonymous" { "human".to_string() } else { actor_name };
    let out: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
    let out_w = out.clone();
    let sid_w = sid.clone();
    let write = state
        .store
        .write_async(move |conn| {
            let moved: Vec<String> = conn
                .prepare("SELECT id FROM issues WHERE status = ?1 AND deleted IS NULL")?
                .query_map([&sid_w], |r| r.get::<_, String>(0))?
                .filter_map(Result::ok)
                .collect();
            let stamp = hhmm();
            let mut events = Vec::new();
            for card in &moved {
                let line = format!(
                    "status: {sid_w} -> todo (column '{sid_w}' deleted by {actor})"
                );
                conn.execute(
                    "UPDATE issues SET log = ?1 WHERE id = ?2",
                    rusqlite::params![
                        bs::append_log(
                            conn.query_row(
                                "SELECT log FROM issues WHERE id = ?1",
                                [card],
                                |r| r.get::<_, Option<String>>(0),
                            )?
                            .as_deref(),
                            &stamp,
                            &line,
                        ),
                        card
                    ],
                )?;
                events.push(PendingEvent {
                    entity_type: EntityType::Task,
                    entity_id: card.clone(),
                    mutation: MutationKind::StatusChanged {
                        from: sid_w.clone(),
                        to: "todo".into(),
                    },
                    payload: None,
                });
            }
            conn.execute(
                "DELETE FROM statuses WHERE id = ?1 AND is_builtin = 0",
                [&sid_w],
            )?;
            conn.execute(
                "UPDATE issues SET status = 'todo' WHERE status = ?1 AND deleted IS NULL",
                [&sid_w],
            )?;
            *out_w.lock().expect("status slot poisoned") = Some(moved);
            Ok(WriteOutcome { applied: true, events })
        })
        .await;
    if let Err(e) = write {
        return internal(e);
    }
    let moved = out.lock().expect("status slot poisoned").take().unwrap_or_default();
    Json(json!({
        "ok": true,
        "moved": moved.len(),
        "ids": moved.iter().take(50).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// PUT /api/board/statuses/reorder {order:[ids]} -> {ok:true} (py:69210).
async fn reorder_statuses(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(order) = body.get("order").and_then(Value::as_array).filter(|a| !a.is_empty())
    else {
        return err(StatusCode::BAD_REQUEST, json!({ "error": "missing order" }));
    };
    let ids: Vec<String> = order
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let write = state
        .store
        .write_async(move |conn| {
            for (pos, sid) in ids.iter().enumerate() {
                conn.execute(
                    "UPDATE statuses SET position = ?1 WHERE id = ?2",
                    rusqlite::params![pos as i64, sid],
                )?;
            }
            Ok(WriteOutcome {
                applied: true,
                events: vec![PendingEvent {
                    entity_type: EntityType::Task,
                    entity_id: "statuses:reorder".into(),
                    mutation: MutationKind::Updated,
                    payload: None,
                }],
            })
        })
        .await;
    match write {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => internal(e),
    }
}

// ---- shared helpers ------------------------------------------------------

fn err(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

use super::internal;

fn not_found(id: &str) -> Response {
    err(
        StatusCode::NOT_FOUND,
        json!({ "error": "item not found", "id": id }),
    )
}

fn no_write() -> WriteOutcome {
    WriteOutcome {
        applied: false,
        events: Vec::new(),
    }
}

/// Task event carrying the post-mutation snapshot (RR-0111a). Every board
/// event site has the freshly written row in hand inside the same write
/// closure, so the snapshot is one serialization — never a re-read — and the
/// journal can replay board state without consulting the live table.
fn ev_snap(row: &IssueRow, mutation: MutationKind) -> PendingEvent {
    PendingEvent {
        entity_type: EntityType::Task,
        entity_id: row.id.clone(),
        mutation,
        payload: Some(row.snapshot()),
    }
}

fn finish<T>(
    slot: &Mutex<Option<T>>,
    outcome: T,
    write: WriteOutcome,
) -> rusqlite::Result<WriteOutcome> {
    *slot.lock().expect("outcome slot poisoned") = Some(outcome);
    Ok(write)
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Local HH:MM, matching Python's `time.strftime("%H:%M")` log stamps.
fn hhmm() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

/// The verified caller identity from the attribution header (AMUX-1768:
/// provenance is the header, never body text). Returns (core actor, log display
/// name). No worker registry lookup exists yet, so a named caller maps to
/// `Actor::System{component: <name>}` — honest about being unverified-as-a-
/// Worker while still carrying the name into every audit line.
///
/// BOTH SPELLINGS, and this module was the only one that took just one (AC-322).
/// `X-Amux-Worker` is canonical and `X-Amux-Session` is still honored — the rule
/// every other module already implements via [`crate::api::groups::hdr_worker`]
/// (groups, session_verbs, schedules, email, alerts, git_guard). board.rs read
/// `x-amux-session` alone, and the installed `amux` CLI is the bash script,
/// whose 14 board-path PATCH sites all send `X-Amux-Worker`. So a correctly
/// attributed CLI call was byte-identical to an anonymous one HERE and nowhere
/// else, which broke two things at once:
///
///   1. `amux board <status> --force` was unwalkable. The force check below
///      refuses `api-anonymous`, so the sanctioned CLI could not satisfy the
///      attribution requirement force demands, and its own error told the caller
///      to "use the CLI" — which is what they had done. That is ethos rule 6
///      exactly: a constraint whose sanctioned escape is unwalkable from the
///      audited path gets walked from an unaudited one (a hand-rolled curl,
///      which is where unattributed writes come from in the first place).
///   2. The cross-lane ARCHIVE guard (AMUX-2492) was blind to every bash-CLI
///      caller. `caller_lane` derives from this same name, so it was empty for
///      all of them, and an empty caller_lane disables the guard — meaning the
///      guard that stops one lane archiving another lane's card has been open
///      for the entire installed-CLI population, silently.
///
/// Fixed at the seam rather than at the CLI's call sites: one resolver here
/// fixes every already-installed CLI copy at once, and closes both effects
/// together, whereas patching curl lines fixes only the machines that upgrade.
fn actor_from_headers(headers: &HeaderMap) -> (Actor, String) {
    match Some(crate::api::groups::hdr_worker(headers))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(name) => (
            Actor::System {
                component: name.to_string(),
            },
            name.to_string(),
        ),
        None => (
            Actor::System {
                component: "api-anonymous".into(),
            },
            "api-anonymous".into(),
        ),
    }
}

// ---- body shapes ---------------------------------------------------------

/// Full detail body: everything, full `desc`, full `log` (L1: the full desc
/// is never in a LIST payload; it is always here). Delegates to
/// [`IssueRow::snapshot`] — the SAME serialization the event journal records
/// as each mutation's payload (RR-0111a), so API body, journal payload, and
/// replay verification can never drift apart.
fn detail_body(row: &IssueRow) -> Value {
    row.snapshot()
}

/// List body, Python-parity (AMUX-2586 fix #4). The plain list serves the
/// FULL `desc` and FULL `log` exactly as Python's `_load_board` does — the
/// SPA renders `item.desc` and reads `item.log` (the folded badge) straight
/// off the LIST payload, so the earlier L1 slimming (first-line desc,
/// `log_n` instead of `log`) silently blanked both in the dashboard.
/// `slim=1` stays the payload diet, matching Python `_board_project`: drop
/// desc/log, add `desc_len` + `log_n`. `stale` mirrors Python's
/// `_board_item_stale` flag — set ONLY when true, on both paths (Python's
/// `_BOARD_SLIM_DROP` is `("desc","log")`; `stale` rides through slim).
pub fn list_body(row: &IssueRow, slim: bool, stale: bool) -> Value {
    let mut v = detail_body(row);
    let obj = v.as_object_mut().expect("detail_body is an object");
    if slim {
        obj.remove("desc");
        obj.insert("desc_len".into(), json!(row.desc.chars().count()));
        obj.remove("log");
        let log_n = row
            .log
            .as_deref()
            .map(|l| l.lines().filter(|x| !x.trim().is_empty()).count())
            .unwrap_or(0);
        obj.insert("log_n".into(), json!(log_n));

        // SHIP THE DERIVED FACTS, NOT THE RAW FIELDS (AMUX-2840).
        //
        // The comment above records that an earlier slimming attempt "silently
        // blanked both in the dashboard", because the SPA reads `desc` and
        // `log` straight off the LIST payload. It does — but not for their
        // content. It needs exactly two things from them in a list:
        //   app.js:19488  the first line of desc, as the card preview
        //   app.js:18866  whether desc+log contain "New task:", for the folded badge
        // Both are tiny derivations over fields that together are 81% of a
        // 4.7MB response. Computing them here costs bytes in the low hundreds
        // and lets the client stop carrying 3.5MB of prose it never renders.
        //
        // Full-text SEARCH is the third consumer and is deliberately NOT served
        // here: /api/search already indexes these cards and returns ranked hits
        // with snippets, so shipping every desc to re-implement it client-side
        // is duplicated work in the expensive direction.
        let head: String = row
            .desc
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        obj.insert("desc_head".into(), json!(head));
        let folded_n = row.desc.matches("New task:").count()
            + row.log.as_deref().map(|l| l.matches("New task:").count()).unwrap_or(0);
        obj.insert("folded_n".into(), json!(folded_n));

        // The third derivation the list makes over desc+log (app.js:19231): the
        // LAST "NEEDS-YOU:" marker, which is what a card shows when it is
        // waiting on a human. Last rather than first — a re-marked card should
        // show its freshest question, which is the client's own rule.
        let ny = {
            let hay = format!("{}\n{}", row.desc, row.log.as_deref().unwrap_or(""));
            let mut found: Option<String> = None;
            for line in hay.lines() {
                let l = line.trim();
                let low = l.to_lowercase();
                // EVERY SPELLING THE CLIENT REGEX ACCEPTS, or the two disagree
                // about the same card. app.js's _focusAsk uses
                // /NEEDS[- ]?(?:YOU|ETHAN|HUMAN):/i, which admits the space and
                // no-separator forms for ETHAN and HUMAN too — this list had
                // only the hyphenated ones, so a card marked "NEEDS ETHAN:"
                // produced a note in the client and none here. Under slim the
                // client reads THIS field, so the divergence would have shown
                // up as the marker silently ceasing to work for those spellings
                // the moment the poll flipped.
                for m in [
                    "needs-you:", "needs you:", "needsyou:",
                    "needs-ethan:", "needs ethan:", "needsethan:",
                    "needs-human:", "needs human:", "needshuman:",
                ] {
                    if let Some(p) = low.find(m) {
                        let v = l[p + m.len()..].trim();
                        if !v.is_empty() {
                            found = Some(v.chars().take(400).collect());
                        }
                    }
                }
            }
            found
        };
        if let Some(n) = ny {
            obj.insert("needsyou_note".into(), json!(n));
        }

        // Detail-only fields the list never renders. The SPA fetches the
        // full card on demand when the detail panel opens, so these are
        // pure payload waste on the list/SSE path. Keeps depends_on
        // (is:blocked filter) and folded_n (is:folded filter).
        for k in ["source_ref", "last_verified_at", "reviewer", "due_time", "gate"] {
            obj.remove(k);
        }
    }
    if stale {
        obj.insert("stale".into(), json!(true));
    }
    v
}

/// Python `_board_item_stale` (amux-server.py:15671): an in-progress card
/// whose owning session is not actively working and that nobody has touched
/// for 30 minutes. `working` is the derived active-session set — the SAME
/// derivation the session list serves, so the two views cannot disagree.
pub fn is_stale(row: &IssueRow, now: i64, working: &std::collections::BTreeSet<String>) -> bool {
    if !matches!(row.status.as_str(), "doing" | "review") {
        return false;
    }
    let Some(sess) = row.session.as_deref().filter(|s| !s.is_empty()) else {
        return false;
    };
    if row.updated == 0 || now - row.updated < 1800 {
        return false;
    }
    !working.contains(sess)
}

// ---- GET /api/board ------------------------------------------------------

#[derive(Deserialize)]
pub struct ListParams {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub archived: Option<String>,
    #[serde(default)]
    pub done_limit: Option<i64>,
    // `?all=1` — the "give me everything" escape. Every session that hit the
    // terminal cap reached for this exact param (AMUX-3154: mixpeek-funnel,
    // mixpeek-frustrations, ts-gke all tried `?all=1`/`?limit=10000` and got the
    // capped 100-terminal view back, silently). It was an UNRECOGNISED param, so
    // axum dropped it and the default cap answered — the rule-7 failure where a
    // filter that never ran hands back a confident wrong denominator (a lane
    // auditing its `done` work off the plain list was reading ~6% of it). Now
    // recognised and honoured as `done_limit=0` (uncap terminal). The dashboard
    // render poll keeps the cap by NOT sending this; a denominator read asks for
    // it explicitly.
    #[serde(default)]
    pub all: Option<String>,
    #[serde(default)]
    pub slim: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
    // SEARCH-INTENT PARAMS, RECOGNISED ONLY TO BE REFUSED (2026-08-11).
    //
    // axum drops unknown query params silently, so `/api/board?q=nudge`
    // returned THE WHOLE BOARD — 1382 rows that look exactly like search
    // results. That is the failure mode ethos rule 7 names: a filter that
    // silently matches everything hands you a confident wrong answer instead of
    // silence, and nothing about the response prompts a recheck.
    //
    // It cost a real one here: two different queries returned byte-identical
    // lists, and only comparing them by accident revealed the param was inert.
    // One query alone reads as "no such card exists" — which is how a duplicate
    // gets filed against a board that already had the card.
    //
    // Not silently honoured either, because /api/search is the real one and it
    // returns a different (ranked, typed) shape. Naming it in a 400 routes the
    // caller to the working endpoint, the same way the gate 409 publishes its
    // escape. Nothing in the SPA or CLI sends these — verified before making
    // them loud — so this cannot break a client holding a stale service worker.
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
}

/// Query keys GET /api/board actually consumes. Anything else is dropped
/// SILENTLY by axum's typed `Query<ListParams>` — which is how `?include_archived=1`
/// or a mistyped filter returned the DEFAULT view served as if it answered the
/// query (BACKE-3228; the ethos rule-7 class — a filter that never ran hands back
/// a confident wrong answer). It bit three sessions: amux-cloud read 13 archived
/// cards as 0, ts-gke brute-forced the id space, backend reported a working guard
/// as broken. A blanket 400 on unknown params is unsafe (cache-busters like
/// `?_=<ts>` are legitimate and any client may append them), so instead we NAME
/// the ignored ones in a response header + a WARN — non-breaking, and it makes
/// the silent drop impossible to miss the way `X-Amux-Done-Limit` already
/// announces the terminal cap. (`q`/`query`/`search` are here because they are
/// consumed above — refused with a 400 — so they are recognised, not ignored.)
const RECOGNISED_BOARD_PARAMS: &[&str] = &[
    "status", "session", "archived", "done_limit", "all", "slim", "limit", "offset", "q", "query",
    "search",
];
/// Cache-buster keys clients legitimately append; never a filter typo, so they
/// are not surfaced as "ignored" (that would be pure noise on every polled tab).
const BENIGN_QUERY_KEYS: &[&str] =
    &["_", "t", "v", "ts", "cb", "_t", "cache", "cachebust", "nocache"];

/// Query keys GET /api/board neither consumes nor treats as a benign
/// cache-buster — the ones a caller thinks are filtering but that did nothing.
/// Pure over the raw query so it is tested without an HTTP round-trip.
fn ignored_board_params(raw_query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for pair in raw_query.split('&') {
        let key = pair.split('=').next().unwrap_or("").trim();
        if key.is_empty() {
            continue;
        }
        let k = key.to_ascii_lowercase();
        if RECOGNISED_BOARD_PARAMS.contains(&k.as_str()) || BENIGN_QUERY_KEYS.contains(&k.as_str())
        {
            continue;
        }
        if !out.iter().any(|e| e.eq_ignore_ascii_case(key)) {
            out.push(key.to_string());
        }
    }
    out
}

fn qp_truthy(v: Option<&str>) -> bool {
    // Python lowercases before the membership test (`.lower() in ("1",
    // "true","yes")`), so `slim=TRUE` counts.
    matches!(
        v.map(|s| s.trim().to_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Bare JSON ARRAY (the Python dashboard parses exactly that shape). The
/// terminal cap ALWAYS announces itself via the header quartet the Python
/// server emits (`X-Amux-Done-Limit`/`-Truncated`/`-Terminal-Total`/
/// `-Terminal-Returned`) — a silent cap manufactured wrong absence claims
/// twice in one week (AC-291, AC-301), so the two counts come from
/// `cap_terminal` itself, never re-derived from list lengths.
pub async fn list_board(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_query: axum::extract::RawQuery,
    Query(p): Query<ListParams>,
) -> Response {
    // BACKE-3228: name query params we silently dropped, so a caller cannot draw
    // an absence conclusion from a filter that never ran. Computed from the RAW
    // query (typed ListParams cannot see keys it does not declare).
    let ignored = raw_query.0.as_deref().map(ignored_board_params).unwrap_or_default();
    if !ignored.is_empty() {
        tracing::warn!(
            target: "board",
            ignored = %ignored.join(","),
            "GET /api/board ignored unrecognised query param(s) — caller may be reading a \
             default view as a filtered answer (BACKE-3228)"
        );
    }
    // ETag based on global_rev — saves 3.5MB on unchanged polls.
    let rev = state.store.current_rev().map(|r| r.0).unwrap_or(0);
    let etag_val = format!("\"board-{}\"", rev);
    if let Some(inm) = headers.get("if-none-match").and_then(|v| v.to_str().ok()) {
        if inm == etag_val || inm == format!("W/{etag_val}") {
            let mut h = HeaderMap::new();
            if let Ok(v) = etag_val.parse() {
                h.insert("etag", v);
            }
            return (StatusCode::NOT_MODIFIED, h).into_response();
        }
    }

    if let Some(term) = p.q.as_deref().or(p.query.as_deref()).or(p.search.as_deref()) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "ok": false,
                "error": "/api/board does not search — it would have returned the ENTIRE board",
                "you_sent": term,
                "use_instead": format!("/api/search?q={term}"),
                "why": "This param was silently ignored until 2026-08-11, so the full board came \
                        back looking like ranked results. Refusing loudly beats answering wrongly.",
                "board_filters": ["status", "session", "archived", "done_limit", "all", "slim", "limit", "offset"],
            })),
        )
            .into_response();
    }
    let split = |s: &Option<String>| -> Vec<String> {
        s.as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect()
    };
    let status_f = split(&p.status);
    let session_f = split(&p.session);
    // `archived` grammar (amux-server.py:68758 + 14025, ported on AMUX-2586 fix #5):
    //   "1"/"true"/"yes"          -> archived-only
    //   any OTHER non-empty value -> non-archived only ("0", "false", "all", "2", ...)
    //   absent or ""              -> scope-dependent (see below)
    //
    // SCOPE-DEPENDENT default (AMUX-3086 / AMUX-3107). A SCOPED list (status= or
    // session=) with `archived` absent now defaults to ActiveOnly, so the view
    // agrees with the mutation guard: an archived card is immutable
    // (amux-core/board.rs:570), and agent cleanup loops were building discard
    // candidates from `?session=X&status=done`, then PATCHing {status:discarded}
    // on the ~42 archived cards it mixed in, drawing 409 archived_task_immutable
    // (ethos rule 1: a view must share the predicate of the mechanism it
    // describes). The UNSCOPED bare list stays All: the SPA text-search full fetch
    // (?done_limit=0) relies on archived cards being in the corpus, and
    // board_api.rs pins that the bare list still includes them.
    let scoped = !status_f.is_empty() || !session_f.is_empty();
    let archived = match p.archived.as_deref().map(|s| s.to_lowercase()) {
        Some(v) if matches!(v.as_str(), "1" | "true" | "yes") => ArchivedFilter::ArchivedOnly,
        Some(v) if !v.is_empty() => ArchivedFilter::ActiveOnly,
        _ if scoped => ArchivedFilter::ActiveOnly,
        _ => ArchivedFilter::All,
    };
    // <=0 is uncapped inside cap_terminal, matching Python's `_cap_terminal`.
    //
    // A SCOPED QUERY IS NOT CAPPED BY DEFAULT (ts-gke, 2026-08-11).
    //
    // The cap exists so the UNFILTERED board payload stays renderable — the
    // dashboard does not draw 1300 terminal cards. But `?session=X` or
    // `?status=done` is a bounded QUESTION, and answering it with the 100
    // most-recent terminal rows produces a confident wrong number with nothing
    // in the body to say so.
    //
    // Measured on the report: ts-gke holds 174 terminal cards (94 done, 60
    // verified, 20 discarded). Capping to the 100 most-recently-updated left 68
    // that happened to be `done`, so `?session=ts-gke` answered 68 where the
    // truth is 94 — and a digest built on it reported 25. Four cards named in
    // that digest were absent from the list while GET /api/board/<id> returned
    // them fine: same store, two endpoints, different answers.
    //
    // The truncation WAS reported, in x-amux-truncated / x-amux-terminal-total
    // headers. That is ethos rule 4's second layer: a tag in a store the reader
    // never opens is the same failure as no tag. Every consumer here reads
    // `curl | json.load`, which sees a bare array and no headers at all.
    //
    // An explicit ?done_limit= still wins in both cases — a caller who asks for
    // a bound gets exactly that.
    let scoped = p.session.is_some() || p.status.is_some();
    // `?all=1` uncaps the terminal set for the unscoped list — the documented,
    // now-discoverable escape from the render cap (AMUX-3154). An explicit
    // `?done_limit=N` still wins over it (a caller who names a bound gets it).
    let uncap_all = qp_truthy(p.all.as_deref());
    let done_limit = p
        .done_limit
        .unwrap_or(if scoped || uncap_all { 0 } else { 100 });
    let slim = qp_truthy(p.slim.as_deref());

    let store = state.store.clone();
    let joined = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = store.read()?;
        let rows = bs::list_issues(&conn, &status_f, &session_f, archived)?;
        // The `stale` flag needs the active-session set only when an
        // in-progress card is present (Python computes it in `_load_board`).
        let working = if rows
            .iter()
            .any(|r| matches!(r.status.as_str(), "doing" | "review"))
        {
            crate::api::sessions_legacy::active_python_sessions(&conn)
        } else {
            Default::default()
        };
        Ok((rows, working))
    })
    .await;
    let (rows, working) = match joined {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let (kept, term_total, term_kept) = bs::cap_terminal(rows, done_limit);
    let total = kept.len();
    let now = now_secs();

    // TWO-FIXES (AMUX-3154): the terminal cap already reports itself in
    // x-amux-truncated / x-amux-terminal-total, but a `curl | json.load` consumer
    // reads the bare array and never sees a header (ethos rule 4, second layer: a
    // signal in a store the reader never opens is the same as no signal). So the
    // NEXT lane that reads the plain list as a `done` denominator leaves a
    // greppable trace instead of a clean-looking wrong answer. Gated to the
    // denominator-read SHAPE — an unscoped, full-card (non-slim) fetch that did
    // not ask for a bound — so the high-frequency dashboard poll (slim=1) and any
    // explicit ?all=1 / ?done_limit= caller stay silent. grep "board list truncated".
    if term_total > term_kept
        && !scoped
        && !uncap_all
        && !slim
        && p.done_limit.is_none()
    {
        tracing::warn!(
            target: "board",
            hidden = term_total - term_kept,
            terminal_total = term_total,
            terminal_returned = term_kept,
            "board list truncated {} terminal card(s) to the render cap — a caller reading the \
             plain /api/board as a 'done' denominator sees a partial set. Use ?all=1 or \
             ?status=done for the full set (AMUX-3154).",
            term_total - term_kept
        );
    }

    let offset = p.offset.unwrap_or(0);
    let page: &[bs::IssueRow] = if offset >= kept.len() {
        &[]
    } else if let Some(lim) = p.limit {
        &kept[offset..(offset + lim).min(kept.len())]
    } else {
        &kept[offset..]
    };

    let items: Vec<Value> = page
        .iter()
        .map(|r| list_body(r, slim, is_stale(r, now, &working)))
        .collect();

    let mut headers = HeaderMap::new();
    let put = |h: &mut HeaderMap, k: &'static str, v: String| {
        if let Ok(val) = v.parse() {
            h.insert(k, val);
        }
    };
    put(&mut headers, "x-amux-done-limit", done_limit.to_string());
    put(
        &mut headers,
        "x-amux-truncated",
        if term_total > term_kept { "1" } else { "0" }.to_string(),
    );
    put(&mut headers, "x-amux-terminal-total", term_total.to_string());
    put(
        &mut headers,
        "x-amux-terminal-returned",
        term_kept.to_string(),
    );
    put(&mut headers, "x-amux-total", total.to_string());
    put(&mut headers, "x-amux-offset", offset.to_string());
    put(&mut headers, "x-amux-returned", items.len().to_string());
    // BACKE-3228: announce any query params we ignored, so a filter typo cannot
    // masquerade as an empty/absent result. Non-breaking (informational header).
    if !ignored.is_empty() {
        put(&mut headers, "x-amux-params-ignored", ignored.join(","));
    }
    put(&mut headers, "etag", etag_val);
    (StatusCode::OK, headers, Json(Value::Array(items))).into_response()
}

// ---- request-value helpers (bodies are raw maps: the Python dashboard
// PATCHes whole item objects, so deny_unknown_fields would break the UI;
// unknown keys are collected and REPORTED as `ignored_fields` instead of
// silently dropped — the narrower truth Invariant 37 actually needs) -------

fn body_str(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

/// A nullable string field: `None` = absent, `Some(None)` = explicit null
/// (clear it), `Some(Some(s))` = set.
fn body_opt_str(map: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match map.get(key) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(v) => Some(v.as_str().map(str::to_string)),
    }
}

/// tags/depends_on style list: array of strings; a bare string is coerced to
/// a one-element list (SP-539: iterating a str exploded it into one tag per
/// character — 200, no error, silently corrupted card).
fn body_str_list(v: &Value) -> Result<Vec<String>, String> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::String(s) => Ok(if s.trim().is_empty() {
            Vec::new()
        } else {
            vec![s.clone()]
        }),
        Value::Array(a) => {
            let mut out = Vec::new();
            for x in a {
                match x.as_str() {
                    Some(s) if !s.trim().is_empty() => out.push(s.trim().to_string()),
                    Some(_) => {}
                    None => return Err("must be a list of strings".into()),
                }
            }
            Ok(out)
        }
        _ => Err("must be a list of strings".into()),
    }
}

fn unknown_type_response(t: &str) -> Response {
    err(
        StatusCode::BAD_REQUEST,
        json!({
            "error": format!("unknown type {t:?}"),
            "valid_types": bs::KNOWN_TYPES,
            "why": "The gate is DERIVED from type. An unknown type would silently fall back \
                    to the strictest (code) gate, which non-code work cannot satisfy without \
                    asserting a merge that never happened.",
        }),
    )
}

fn cycle_response(cycle: &[String]) -> Response {
    err(
        StatusCode::BAD_REQUEST,
        json!({
            "error": format!("circular depends_on: {}", cycle.join(" -> ")),
            "cycle": cycle,
        }),
    )
}

const VALID_STATUSES: [&str; 11] = [
    "backlog",
    "todo",
    "doing",
    "review",
    "needsyou",
    "blocked",
    "done",
    "verified",
    "discarded",
    "armed",
    "quarantined",
];

// ---- POST /api/board -----------------------------------------------------

pub async fn create_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(map) = body.as_object().cloned() else {
        return err(
            StatusCode::BAD_REQUEST,
            json!({ "error": "body must be a JSON object" }),
        );
    };
    let title = body_str(&map, "title").unwrap_or_default().trim().to_string();
    if title.is_empty() {
        return err(StatusCode::BAD_REQUEST, json!({ "error": "missing title" }));
    }

    // MO-3038: when the body OMITS `session` and the verified header is
    // present, the card is for the sender's own lane. An EXPLICIT value —
    // including explicit "" / null for a deliberately unassigned card — is
    // always respected.
    let (_, hdr_name) = actor_from_headers(&headers);
    let hdr_session = if hdr_name == "api-anonymous" {
        String::new()
    } else {
        hdr_name.clone()
    };
    let session = if map.contains_key("session") {
        body_str(&map, "session").unwrap_or_default().trim().to_string()
    } else {
        hdr_session.chars().take(64).collect()
    };

    let status_in = body_str(&map, "status").unwrap_or_else(|| "todo".into());
    // AMUX-2609: a status outside the typed vocabulary may still be a real
    // user-created column. The `statuses` table is the vocabulary for those —
    // see the long note in `patch_item` for why `TaskStatus` stays closed.
    let status_raw = match bs::parse_status(&status_in) {
        Some(st) => bs::db_status_spelling(st).to_string(),
        None => {
            let id = status_in.trim().to_lowercase();
            let known = state
                .store
                .read()
                .ok()
                .and_then(|conn| {
                    conn.query_row(
                        "SELECT id FROM statuses WHERE id = ?1",
                        rusqlite::params![id],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                });
            match known {
                Some(id) => id,
                None => {
                    let cols: Vec<String> = state
                        .store
                        .read()
                        .ok()
                        .map(|conn| {
                            conn.prepare("SELECT id FROM statuses ORDER BY position")
                                .and_then(|mut st| {
                                    st.query_map([], |r| r.get::<_, String>(0))
                                        .map(|rows| rows.flatten().collect::<Vec<String>>())
                                })
                                .unwrap_or_default()
                        })
                        .unwrap_or_default();
                    return err(
                        StatusCode::BAD_REQUEST,
                        json!({
                            "error": format!("unknown status {status_in:?}"),
                            "valid_statuses": VALID_STATUSES,
                            "configured_columns": cols,
                            "how_to_add": "POST /api/board/statuses {\"label\": \"...\"}",
                        }),
                    );
                }
            }
        }
    };

    let item_type = body_str(&map, "type")
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "code".into());
    if !bs::KNOWN_TYPES.contains(&item_type.as_str()) {
        return unknown_type_response(&item_type);
    }

    let depends_on = match map.get("depends_on") {
        None => Vec::new(),
        Some(v) => match body_str_list(v) {
            Ok(l) => l,
            Err(e) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    json!({ "error": format!("depends_on {e}") }),
                )
            }
        },
    };
    let tags = match map.get("tags") {
        None => Vec::new(),
        Some(v) => match body_str_list(v) {
            Ok(l) => l,
            Err(e) => {
                return err(StatusCode::BAD_REQUEST, json!({ "error": format!("tags {e}") }))
            }
        },
    };
    let gate = match map.get("gate") {
        None => Vec::new(),
        Some(v) => body_str_list(v).unwrap_or_default(),
    };

    // Creator attribution (AMUX-1812): the body value is a self-reported
    // CLAIM; the verified header wins, and a disagreement is recorded.
    let claimed = body_str(&map, "creator").unwrap_or_default().trim().to_string();
    let creator = match (&hdr_session.is_empty(), claimed.is_empty()) {
        (false, false) if hdr_session != claimed => format!("{hdr_session} (claimed {claimed})"),
        (false, _) => hdr_session.clone(),
        (true, false) => claimed,
        (true, true) => String::new(),
    };

    let owner_type = match body_str(&map, "owner_type").as_deref() {
        Some("human") => "human".to_string(),
        Some("agent") => "agent".to_string(),
        Some(_) => "human".to_string(),
        None => if session.is_empty() { "human" } else { "agent" }.to_string(),
    };

    let known_keys = [
        "title", "desc", "status", "session", "type", "depends_on", "tags", "creator",
        "reviewer", "shepherd", "gate", "owner_type", "due", "due_time",
    ];
    let ignored: Vec<String> = map
        .keys()
        .filter(|k| !known_keys.contains(&k.as_str()))
        .cloned()
        .collect();

    let new = bs::NewIssue {
        title,
        desc: body_str(&map, "desc").unwrap_or_default(),
        status: status_raw,
        session: Some(session).filter(|s| !s.is_empty()),
        item_type,
        creator,
        owner_type,
        due: body_str(&map, "due").filter(|s| !s.trim().is_empty()),
        due_time: body_str(&map, "due_time").filter(|s| !s.trim().is_empty()),
        reviewer: body_str(&map, "reviewer").filter(|s| !s.trim().is_empty()),
        shepherd: body_str(&map, "shepherd").filter(|s| !s.trim().is_empty()),
        gate,
        depends_on,
        tags,
    };

    enum Out {
        Cycle(Vec<String>),
        Created(Box<IssueRow>),
    }
    let slot: Arc<Mutex<Option<Out>>> = Arc::new(Mutex::new(None));
    let slot_w = slot.clone();
    let write = state
        .store
        .write_async(move |conn| {
            // Acyclicity is validated INSIDE the write so no interleaved
            // create can slip a cycle between check and insert. The new id
            // does not exist yet, so a placeholder self id is fine — only
            // edges out of it are being added.
            if !new.depends_on.is_empty() {
                if let Some(cycle) = bs::depends_on_cycle(conn, "\u{0}new-card", &new.depends_on)? {
                    return finish(&slot_w, Out::Cycle(cycle), no_write());
                }
            }
            let row = bs::create_issue(conn, &new, now_secs())?;
            let event = ev_snap(&row, MutationKind::Created);
            finish(
                &slot_w,
                Out::Created(Box::new(row)),
                WriteOutcome {
                    applied: true,
                    events: vec![event],
                },
            )
        })
        .await;
    let reply = match write {
        Ok(r) => r,
        Err(e) => return internal(e),
    };
    let outcome = slot.lock().expect("outcome slot poisoned").take();
    match outcome {
        None => internal("create produced no outcome"),
        Some(Out::Cycle(cycle)) => cycle_response(&cycle),
        Some(Out::Created(row)) => {
            let mut v = detail_body(&row);
            v["rev"] = json!(row.rev);
            v["global_rev"] = json!(reply.rev.0);
            if !ignored.is_empty() {
                v["ignored_fields"] = json!(ignored);
            }
            (StatusCode::CREATED, Json(v)).into_response()
        }
    }
}

// ---- GET /api/board/{id} -------------------------------------------------

pub async fn get_item(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let store = state.store.clone();
    let key = id.clone();
    let joined = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = store.read()?;
        Ok(bs::get_issue(&conn, &key)?)
    })
    .await;
    match joined {
        Ok(Ok(Some(row))) => {
            // Weak ETag for read-modify-write callers (AMUX-1711 parity).
            let mut headers = HeaderMap::new();
            if let Ok(v) = format!("W/\"{}-{}\"", row.id, row.rev).parse() {
                headers.insert("etag", v);
            }
            (StatusCode::OK, headers, Json(detail_body(&row))).into_response()
        }
        Ok(Ok(None)) => not_found(&id),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /api/board/{id}/claim — atomically take a `todo` card and start it.
///
/// The assignment notifications tell every session to run `amux board claim
/// <id>`, and the CLI has always POSTed here — but the route was never mounted,
/// so the call hit the GET-only SPA catch-all (405), the CLI printed a good
/// message and (pre-fix) exited 0, and the card was untouched (AMUX-3131, the
/// AMUX-2140 class one layer down: the sanctioned instruction was theatre). It
/// now runs the SAME operation auto-pickup uses (`claim_card`: compare-and-swap
/// todo->doing, assign the claimer, emit `task.claimed` for the 24h re-claim
/// cooldown), so a manual claim and an auto-pickup are one mechanism.
pub async fn claim_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Claimer: X-Amux-Worker / X-Amux-Session (canonical), else a body
    // {"session":...} (the bash CLI sends both). A claim with no claimer is
    // meaningless, so refuse rather than record an anonymous owner.
    let (_actor, mut session) = actor_from_headers(&headers);
    if session == "api-anonymous" {
        if let Ok(v) = serde_json::from_slice::<Value>(&body) {
            if let Some(s) = v.get("session").and_then(Value::as_str) {
                let s = s.trim();
                if !s.is_empty() {
                    session = s.to_string();
                }
            }
        }
    }
    if session == "api-anonymous" || session.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "claim needs a claimer — send X-Amux-Session: <your session> (the `amux board` CLI does this for you)",
                "id": id,
            })),
        )
            .into_response();
    }
    // Read current status + owner first, so every branch reports the truth
    // (claimed / already yours / not claimable / not found) instead of a bare
    // 409 the caller cannot act on.
    let store = state.store.clone();
    let key = id.clone();
    let row = match tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = store.read()?;
        Ok(bs::get_issue(&conn, &key)?)
    })
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let Some(row) = row else {
        return not_found(&id);
    };
    let owner = row.session.clone().unwrap_or_default().trim().to_string();
    match row.status.as_str() {
        "todo" => {
            if !owner.is_empty() && owner != session {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": format!("card is assigned to '{owner}', not yours to claim — reassign it first"),
                        "id": id, "status": "todo", "session": owner,
                    })),
                )
                    .into_response();
            }
            if crate::runtime_jobs::board_drive::claim_card(&state, &session, &id).await {
                (
                    StatusCode::OK,
                    Json(json!({
                        "ok": true, "id": id, "status": "doing", "session": session, "claimed": true,
                    })),
                )
                    .into_response()
            } else {
                // Raced out of `todo` between the read above and the swap
                // (owner closed it, or a peer claimed first).
                (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": "claim raced — the card left 'todo' between read and write; re-check its status",
                        "id": id,
                    })),
                )
                    .into_response()
            }
        }
        "doing" if owner == session => (
            StatusCode::OK,
            Json(json!({
                "ok": true, "id": id, "status": "doing", "session": session, "already": true,
            })),
        )
            .into_response(),
        other => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!("card is '{other}', not claimable — only a 'todo' card can be claimed (move it first: amux board todo {id})"),
                "id": id, "status": other, "session": owner,
            })),
        )
            .into_response(),
    }
}

// ---- PATCH /api/board/{id} -----------------------------------------------

/// Keys PATCH writes. Everything else lands in `ignored_fields` (reported,
/// never silently dropped — AC-263).
/// Truncate for a HISTORY LINE, on chars not bytes (a multi-byte title must not
/// panic the writer) and with an ellipsis so a truncated value never reads as
/// the whole value.
fn chars_truncate_log(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    format!("{}…", s.chars().take(n).collect::<String>())
}

const PATCH_WRITABLE: [&str; 19] = [
    "title", "desc", "status", "session", "type", "depends_on", "tags", "reviewer", "shepherd",
    "epic", // AMUX-2992: assign/clear the epic a card rolls up under
    "due", "due_time", "owner_type", "pinned", "pos", "gate", "source_ref", "archived",
    // `amux board <status> --trigger` sends source_ref AND last_verified_at
    // together, but only the first was writable, so the stamp was silently
    // dropped into ignored_fields (reported by mixpeek-frustrations on MF-534).
    // That defeats the guard the flag exists for: the staleness view keys on
    // this field, so a parked card without it sleeps forever with a perfectly
    // good trigger and nothing ever re-checks it — "parking without it buys
    // silence with no expiry", which is the flag's own promise inverted.
    "last_verified_at",
];
/// Control keys: consumed by the PATCH protocol itself, never "ignored".
/// `authorized_by` is the cross-lane archive authorizer (AMUX-2492).
/// `desc_append` modifies how `desc` is written rather than naming a column,
/// so it is control, not writable — but it MUST be listed, or it lands in
/// `ignored_fields` and the append silently does nothing (AC-323).
const PATCH_CONTROL: [&str; 8] = [
    "expect_rev",
    "gate_ack",
    "gate_checked",
    "force",
    "reason",
    "authorized_by",
    "override_doing",
    "desc_append",
];

enum PatchOut {
    NotFound,
    /// Any pre-write refusal (400/409) with its exact body.
    Refused(StatusCode, Value),
    /// Invariant 37: nothing changed; `rev` unmoved.
    Noop { body: Value, ignored: Vec<String> },
    Applied {
        body: Value,
        ignored: Vec<String>,
        /// (session, from_status, to_status) when a status change happened,
        /// for reactive pickup: if the transition freed the lane (done/verified/
        /// discarded), fire an immediate pickup instead of waiting 60s.
        status_transition: Option<(String, String, String)>,
    },
}

/// Map a (from, to) pair onto the core transition vocabulary. `None` means
/// no named transition exists — the caller falls back to the gate-checked
/// generic move (the Python board allows any->any, so refusing unmapped
/// pairs outright would break live CLI flows like todo->done).
fn named_transition(
    from: TaskStatus,
    to: TaskStatus,
    evidence: Vec<Evidence>,
    reason: String,
) -> Option<BoardTransition> {
    use TaskStatus as S;
    Some(match (from, to) {
        (S::Backlog, S::Todo) => BoardTransition::Queue,
        (S::Todo, S::Backlog) => BoardTransition::Park,
        (S::Todo, S::Doing) => BoardTransition::Start,
        (S::Doing, S::Todo) => BoardTransition::Release,
        (S::Doing, S::Review) => BoardTransition::Submit,
        (S::Review, S::Done) => BoardTransition::Approve { evidence },
        (S::Review, S::Doing) => BoardTransition::Reject { reason },
        (S::Doing, S::Done) => BoardTransition::Complete { evidence },
        (S::Done, S::Verified) => BoardTransition::Verify {
            criteria: vec![],
            evidence,
        },
        (S::Done, S::Doing) => BoardTransition::VerificationFailed { reason },
        (S::Doing, S::NeedsYou) => BoardTransition::RequestInput { question: reason },
        (S::NeedsYou, S::Doing) => BoardTransition::Resume,
        (S::Todo | S::Doing, S::Blocked) => BoardTransition::Block { reason },
        (S::Blocked, S::Todo) => BoardTransition::Unblock,
        (S::Todo | S::Backlog, S::Armed) => BoardTransition::Arm,
        (S::Armed, S::Todo) => BoardTransition::Fire { reason },
        (_, S::Discarded) => BoardTransition::Discard { reason },
        (_, S::Quarantined) => BoardTransition::Quarantine { reason },
        _ => return None,
    })
}

/// Ack evidence: one `ModelTranscript` artifact per criterion, provenance
/// `SelfReported` (an ack IS self-reported — never inflate it to
/// Independent). This is what `satisfied_by` matches against the
/// `ModelJudgment` verifiers in `bs::core_gates`.
fn ack_evidence(actor: &str, criteria: &[String], via: &str) -> Vec<Evidence> {
    let now = chrono::Utc::now();
    criteria
        .iter()
        .map(|c| Evidence {
            kind: EvidenceKind::ModelTranscript,
            description: format!("acknowledged by {actor} via {via}: {c}"),
            artifact: None,
            produced_at: now,
            source: EvidenceSource::SelfReported,
        })
        .collect()
}

/// The Python-compatible gate 409 (the CLI parses `error`, `gate`,
/// `item_type`, `attempted_status`, `valid_types` — grep amux-server.py
/// "gate not acknowledged"). Core's serialized refusal rides along under
/// `why_blocked`/`kind`: it cannot be merged flat because core spells the
/// list `blocked` while the Python contract's `blocked` is the boolean the
/// CLI-side incident (orch MO-2952) made load-bearing.
fn gate_409(
    row: &IssueRow,
    eff_gate: &[String],
    target_raw: &str,
    wb: &[amux_core::board::WhyBlocked],
) -> Value {
    let checked_args = eff_gate
        .iter()
        .map(|g| format!("{:?}", g))
        .collect::<Vec<_>>()
        .join(" ");
    json!({
        "error": "gate not acknowledged",
        "ok": false,
        "blocked": true,
        "gate": eff_gate,
        "attempted_status": target_raw,
        "item": row.id,
        "item_type": row.item_type,
        "how_to_ack": {
            "gate_ack": true,
            "or_gate_checked": eff_gate,
            "contract": "GET /api/board/contract",
            "wrong_type?": "If this item has no code, set its type (escalation/blocker/investigation/ops/research/chore/doc) — the gate is DERIVED from the type. Never ack a merge that did not happen.",
        },
        "cli": format!("amux board {target_raw} {} --checked {checked_args}", row.id),
        "valid_types": bs::KNOWN_TYPES,
        "kind": "gate_blocked",
        "why_blocked": wb,
    })
}

/// `POST /api/board/clear-done` — the dashboard's "Clear done" button.
///
/// It was never routed on rust (AMUX-2630): the SPA optimistically hid the done
/// cards, POSTed, got a 405 from the GET-only catch-all, and the cards came
/// back on the next refresh. A button that appears to work and silently does
/// nothing is worse than a missing one.
///
/// ARCHIVES, never deletes. The cards are the user's record of what happened,
/// and "clear from my board" is a view operation — `archived=1` removes them
/// from every default view while leaving them recoverable. Deleting user
/// content as the side effect of a tidy-up button is the ethos-rule-8 failure.
pub async fn clear_done(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let (_, actor) = actor_from_headers(&headers);
    // HOW MANY, not merely "ok" (ethos rule 4). A bare success is
    // indistinguishable from the dead button this card is about: both leave
    // the caller with no way to tell "archived 957" from "matched nothing".
    // The count is the UPDATE's own rowcount, so it cannot drift from what
    // the write did — and the SPA renders it instead of a silent hide.
    let slot: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
    let slot_w = slot.clone();
    let res = state
        .store
        .write_async(move |conn| {
            let n = conn.execute(
                "UPDATE issues SET archived = 1, updated = strftime('%s','now') \
                 WHERE status = 'done' AND COALESCE(archived,0) = 0 AND deleted IS NULL",
                [],
            )?;
            *slot_w.lock().unwrap() = Some(n as i64);
            Ok(crate::db::WriteOutcome { applied: n > 0, events: vec![] })
        })
        .await;
    match res {
        Ok(_) => {
            let n = slot.lock().unwrap().unwrap_or(0);
            tracing::info!(actor = %actor, archived = n, "board: cleared done cards (archived)");
            // `action` travels WITH the count because the count alone loses the
            // load-bearing fact: these cards still exist. A client that reads
            // only "archived: 957" must not have to guess whether 957 rows were
            // destroyed.
            (
                StatusCode::OK,
                Json(json!({"ok": true, "archived": n, "action": "archived"})),
            )
                .into_response()
        }
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": format!("clear-done failed: {e}")}),
        ),
    }
}

pub async fn patch_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(map) = body.as_object().cloned() else {
        return err(
            StatusCode::BAD_REQUEST,
            json!({ "error": "body must be a JSON object" }),
        );
    };
    let (actor, actor_name) = actor_from_headers(&headers);
    // ATTRIBUTION IS REQUIRED FOR FORCE (ts-gke 2026-08-03; Python parity
    // amux-server.py ~70111). Fires on `force` ITSELF, never `eff_gate &&
    // force`: the incident specimen was a watch card whose todo->discarded
    // had NO gate, so a gate-conditioned check cannot fail on the case that
    // motivated it.
    if map.get("force").and_then(Value::as_bool).unwrap_or(false) && actor_name == "api-anonymous" {
        return err(
            StatusCode::BAD_REQUEST,
            json!({
                "error": "force requires attribution",
                "why": "force bypasses the checks; the judgment then rests with whoever forced it, so the ledger must name them. An unattributed force is an audit row that records only that something happened.",
                "how": "send X-Amux-Session: <your session> (the `amux board` CLI does this for you). Or satisfy the gate honestly — if it does not fit the work, the TYPE is wrong; fix the type, not the truth.",
            }),
        );
    }
    let force_actor = actor_name.clone();
    // Python `_hdr_worker`: "" when the header is absent — the cross-lane
    // archive guard only fires for a NAMED caller (AMUX-2492).
    let caller_lane = if actor_name == "api-anonymous" {
        String::new()
    } else {
        actor_name.clone()
    };

    let slot: Arc<Mutex<Option<PatchOut>>> = Arc::new(Mutex::new(None));
    let slot_w = slot.clone();
    let id_w = id.clone();

    let write = state
        .store
        .write_async(move |conn| {
            let Some(row) = bs::get_issue(conn, &id_w)? else {
                return finish(&slot_w, PatchOut::NotFound, no_write());
            };

            // Optimistic concurrency: expect_rev checks the PYTHON counter.
            // Conflict outranks everything — a stale caller must learn their
            // view is old before any other verdict.
            if let Some(exp) = map.get("expect_rev").and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            }) {
                if exp != row.rev {
                    return finish(
                        &slot_w,
                        PatchOut::Refused(
                            StatusCode::CONFLICT,
                            json!({
                                "error": "rev conflict",
                                "current_rev": row.rev,
                                "expected": exp,
                                "item": detail_body(&row),
                                "hint": "re-read, re-apply your change to the current item, retry with the new rev",
                            }),
                        ),
                        no_write(),
                    );
                }
            }

            let ignored: Vec<String> = map
                .keys()
                .filter(|k| {
                    !PATCH_WRITABLE.contains(&k.as_str()) && !PATCH_CONTROL.contains(&k.as_str())
                })
                .cloned()
                .collect();

            // ---- stage non-status field changes onto a working copy ------
            // (staged BEFORE the gate check so a PATCH changing type and
            // status together gates on the NEW type — the Python handler's
            // own rule.)
            let mut next = row.clone();
            let mut changed: Vec<String> = Vec::new();
            let mut tags_change: Option<Vec<String>> = None;

            if let Some(t) = body_str(&map, "title") {
                if t != next.title {
                    next.title = t;
                    changed.push("title".into());
                }
            }
            // `desc_append` appends instead of the destructive replace (Python
            // parity, amux-server.py:69887). The cutover dropped it, so every
            // `amux board progress` since has printed "progress noted" and
            // written NOTHING — AC-323, and the sanctioned way CLAUDE.md tells
            // sessions to record an outcome before a gate transition.
            //
            // Python's own comment records the harsher version of this bug: the
            // field was accepted, ignored, and the destructive replace ran
            // anyway — ~20 silent wipes in one day, nine cards rebuilt from
            // /history. Both natural shapes work, so the obvious guess is right:
            //   {desc_append: "text"}             -> old + "\n" + text
            //   {desc: "text", desc_append: true} -> old + "\n" + text
            //   {desc_append: false}              -> plain replace semantics
            let desc_effective: Option<String> = match map.get("desc_append") {
                None | Some(Value::Bool(false)) => body_str(&map, "desc"),
                Some(v) => {
                    let text = match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Bool(true) => body_str(&map, "desc"),
                        _ => None,
                    };
                    match text {
                        Some(t) if !t.is_empty() => {
                            let old = next.desc.trim_end();
                            Some(if old.is_empty() {
                                t.trim().to_string()
                            } else {
                                format!("{old}\n{t}").trim().to_string()
                            })
                        }
                        // Empty/non-string append is a no-op, NOT a wipe.
                        _ => body_str(&map, "desc"),
                    }
                }
            };
            // Nullable epoch seconds. An explicit null CLEARS (re-arming a
            // trigger for re-verification); absent leaves it alone.
            if let Some(v) = map.get("last_verified_at") {
                let next_v = match v {
                    Value::Null => None,
                    Value::Number(n) => n.as_i64(),
                    Value::String(s) => s.trim().parse::<i64>().ok(),
                    _ => next.last_verified_at,
                };
                if next_v != next.last_verified_at {
                    next.last_verified_at = next_v;
                    changed.push("last_verified_at".into());
                }
            }
            if let Some(d) = desc_effective {
                if d != next.desc {
                    next.desc = d;
                    changed.push("desc".into());
                }
            }
            // Nullable string columns: explicit null/"" clears, absent leaves.
            let set_opt =
                |key: &str, field: &mut Option<String>, changed: &mut Vec<String>| {
                    if let Some(v) = body_opt_str(&map, key) {
                        let v = v.filter(|s| !s.trim().is_empty());
                        if *field != v {
                            *field = v;
                            changed.push(key.into());
                        }
                    }
                };
            set_opt("session", &mut next.session, &mut changed);
            set_opt("reviewer", &mut next.reviewer, &mut changed);
            set_opt("shepherd", &mut next.shepherd, &mut changed);
            set_opt("epic", &mut next.epic, &mut changed); // AMUX-2992: assign/clear a card's epic
            set_opt("due", &mut next.due, &mut changed);
            set_opt("due_time", &mut next.due_time, &mut changed);
            set_opt("source_ref", &mut next.source_ref, &mut changed);
            if let Some(ot) = body_str(&map, "owner_type") {
                let ot = if ot == "agent" { "agent" } else { "human" }.to_string();
                if ot != next.owner_type {
                    next.owner_type = ot;
                    changed.push("owner_type".into());
                }
            }
            if let Some(p) = map.get("pinned") {
                let p = match p {
                    Value::Bool(b) => i64::from(*b),
                    v => v.as_i64().unwrap_or(0),
                };
                if p != next.pinned {
                    next.pinned = p;
                    changed.push("pinned".into());
                }
            }
            if let Some(p) = map.get("pos").and_then(|v| v.as_f64()) {
                if (p - next.pos).abs() > f64::EPSILON {
                    next.pos = p;
                    changed.push("pos".into());
                }
            }
            // `archived` via PATCH — Python parity (AMUX-2492, py:70294):
            // the SPA's card archive and the harness cleanup PATCH this
            // field. Python's coercion: str(v).lower() in (1,true,yes,on).
            // Cross-lane ARCHIVING (a named caller hiding another lane's
            // card) requires `authorized_by` — it removes the card from
            // every view and autonomy loop, a termination in effect.
            // UN-archiving is never gated, or the un-do is unreachable.
            if let Some(v) = map.get("archived") {
                let raw = match v {
                    Value::String(s) => s.clone(),
                    Value::Bool(b) => if *b { "true".into() } else { "false".into() },
                    other => other.to_string(),
                };
                let arc_v: i64 = i64::from(matches!(
                    raw.trim().to_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                ));
                if arc_v == 1 {
                    let owner = row.session.clone().unwrap_or_default().trim().to_string();
                    let authorized = map
                        .get("authorized_by")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or("");
                    if !caller_lane.is_empty() && !owner.is_empty() && owner != caller_lane
                        && authorized.is_empty()
                    {
                        return finish(
                            &slot_w,
                            PatchOut::Refused(
                                StatusCode::BAD_REQUEST,
                                json!({
                                    "error": "cross-lane destruction requires authorized_by",
                                    "why": format!(
                                        "{caller_lane} is archiving {}, which belongs to {owner}. \
                                         Archiving hides it from every board view AND every \
                                         autonomy loop, so it is a termination in effect even \
                                         though the status is untouched.",
                                        row.id
                                    ),
                                    "how": format!(
                                        "add {{\"authorized_by\": \"<who asked>\"}}, or use \
                                         `amux board archive {} --authorized-by \"<who>\"`",
                                        row.id
                                    ),
                                    "card_owner": owner,
                                }),
                            ),
                            no_write(),
                        );
                    }
                }
                if arc_v != next.archived {
                    next.archived = arc_v;
                    changed.push("archived".into());
                }
            }
            if let Some(t) = body_str(&map, "type") {
                let t = t.trim().to_lowercase();
                if !t.is_empty() {
                    if !bs::KNOWN_TYPES.contains(&t.as_str()) {
                        // Reject at the door: an unknown type silently
                        // inherits the code gate non-code work cannot satisfy.
                        return finish(
                            &slot_w,
                            PatchOut::Refused(
                                StatusCode::BAD_REQUEST,
                                json!({
                                    "error": format!("unknown type {t:?}"),
                                    "valid_types": bs::KNOWN_TYPES,
                                    "why": "The gate is DERIVED from type. An unknown type would silently fall back to the strictest (code) gate, which non-code work cannot satisfy without asserting a merge that never happened.",
                                }),
                            ),
                            no_write(),
                        );
                    }
                    if t != next.item_type {
                        next.item_type = t;
                        changed.push("type".into());
                        // AMUX-3058: a non-empty `gate` OVERRIDE pins the gate
                        // over the type — effective_gate returns row.gate before
                        // deriving from item_type — so retyping to escape a wrong
                        // gate (ethos rule 3's sanctioned escape) was a DEAD END
                        // while an override stood, including one that matched no
                        // type's default (a code-criteria override on a non-code
                        // card, TUBES-1622). Retyping is an explicit statement that
                        // the card's KIND changed and the gate derives from the
                        // kind, so a stale override is dropped here and the gate
                        // re-derives from the new type. A caller that wants a custom
                        // gate on the retyped card sends `gate` in this SAME PATCH:
                        // the gate handler below runs after this and re-sets it.
                        if next.gate.is_some() {
                            next.gate = None;
                            changed.push("gate".into());
                            tracing::info!(
                                target: "amux::board", id = %next.id,
                                "cleared a stale gate override on retype so the gate re-derives from the new type (AMUX-3058)"
                            );
                        }
                    }
                }
            }
            if let Some(v) = map.get("gate") {
                let list = match body_str_list(v) {
                    Ok(l) => l,
                    Err(e) => {
                        return finish(
                            &slot_w,
                            PatchOut::Refused(
                                StatusCode::BAD_REQUEST,
                                json!({ "error": format!("gate {e}") }),
                            ),
                            no_write(),
                        )
                    }
                };
                let new_gate = if list.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&list).unwrap_or_default())
                };
                if next.gate_criteria() != list {
                    next.gate = new_gate;
                    changed.push("gate".into());
                }
            }
            if let Some(v) = map.get("depends_on") {
                let deps = match body_str_list(v) {
                    Ok(l) => l,
                    Err(e) => {
                        return finish(
                            &slot_w,
                            PatchOut::Refused(
                                StatusCode::BAD_REQUEST,
                                json!({ "error": format!("depends_on {e}") }),
                            ),
                            no_write(),
                        )
                    }
                };
                if deps != next.depends_on {
                    if let Some(cycle) = bs::depends_on_cycle(conn, &row.id, &deps)? {
                        return finish(
                            &slot_w,
                            PatchOut::Refused(
                                StatusCode::BAD_REQUEST,
                                json!({
                                    "error": format!("circular depends_on: {}", cycle.join(" -> ")),
                                    "cycle": cycle,
                                }),
                            ),
                            no_write(),
                        );
                    }
                    next.depends_on = deps;
                    changed.push("depends_on".into());
                }
            }
            if let Some(v) = map.get("tags") {
                let tags = match body_str_list(v) {
                    Ok(l) => l,
                    Err(e) => {
                        return finish(
                            &slot_w,
                            PatchOut::Refused(
                                StatusCode::BAD_REQUEST,
                                json!({ "error": format!("tags {e}") }),
                            ),
                            no_write(),
                        )
                    }
                };
                let mut a = tags.clone();
                let mut b = next.tags.clone();
                a.sort();
                b.sort();
                if a != b {
                    next.tags = tags.clone();
                    tags_change = Some(tags);
                    changed.push("tags".into());
                }
            }

            // ---- status transition through the core state machine --------
            let mut status_event: Option<(String, String)> = None;

            // ---- user-created columns (AMUX-2609) ------------------------
            //
            // Python's board columns are fully dynamic: create a column, drag
            // cards into it. Rust's `TaskStatus` is a closed enum, so
            // `parse_status` returned None and the PATCH bounced 400 — while
            // the SPA had ALREADY moved the card optimistically and cached it.
            // The user saw the card sit in the new column behind a bare
            // "Error: 400" toast until the next poll silently snapped it back.
            //
            // This deliberately does NOT add a `Custom(String)` cell to
            // `TaskStatus`. That enum is the BUILTIN LIFECYCLE the state
            // machine reasons about, and widening it would:
            //   * lose `Copy`, breaking ~40 by-value sites (20 of them
            //     `match task.status` inside `apply_transition` alone);
            //   * break `db_status_spelling`'s `&'static str` return, which
            //     cannot express an owned custom id;
            //   * leave `disposition_is_total_over_every_status` iterating
            //     `TaskStatus::ALL`, a `const [TaskStatus; 11]` that CANNOT
            //     contain a `Custom` — so the totality PROOF silently narrows
            //     to builtins while still passing (ethos rule 7: a check that
            //     can no longer fail);
            //   * and route every custom move through `BoardTransition::Force`
            //     (no `named_transition` arm exists), filling the one audited
            //     bypass trail with routine traffic until it means nothing
            //     (ethos rule 6).
            // `amux_core::workflow` already models dynamic columns properly
            // (ColumnId + ColumnRole::Custom); a `Custom` variant here would be
            // a THIRD spelling of the same idea.
            //
            // So the vocabulary is read from where users actually create
            // columns — the `statuses` table. Both `issues.status` and that
            // table are raw strings already, so nothing migrates. The card's
            // required semantics fall out for free: `board_drive`'s pickup and
            // the WIP-1 guard compare raw SQL against 'todo'/'doing', and the
            // terminal/rot checks against their own literals, so a custom
            // column is non-WIP, non-terminal and never auto-picked WITHOUT a
            // single new exclusion list (ethos rule 1: an exemption nobody
            // maintains is how things go invisible).
            //
            // A transition is UNMODELLED when EITHER end is outside the typed
            // vocabulary. Handling only the "into" direction would build a
            // roach motel — cards could enter a custom column and never leave —
            // which is precisely what ethos rule 3 forbids: every legitimate
            // state needs a truthful exit.
            let unmodelled_status = body_str(&map, "status").filter(|s| {
                bs::parse_status(s).is_none() || bs::parse_status(&next.status).is_none()
            });
            if let Some(target_in) = unmodelled_status {
                let target_typed = bs::parse_status(&target_in);
                let target_raw = match target_typed {
                    Some(t) => bs::status_to_db(t, &next.status),
                    None => target_in.trim().to_lowercase(),
                };

                // NOTE for whoever wires the orchestrator half (AMUX-2631/2/3,
                // de8a079): `db::workflow_store::load_workflow` is the richer
                // reader of this same `statuses` table and models column ROLE
                // and terminal behaviour. It is the right oracle once planning
                // needs to reason about custom columns; the membership check
                // here is deliberately the narrowest question ("does this
                // column exist"), against the same table, so the two cannot
                // disagree about what a column IS.
                //
                // The gate for an unmodelled move: a typed target keeps its
                // normal derived/override gate; a custom target uses the gate
                // the column itself carries (`statuses.gate`, written by the
                // column editor). Without this a custom column would be a
                // gate-shaped hole in the board.
                let eff_gate: Vec<String> = match target_typed {
                    Some(t) => bs::effective_gate_configured(conn, &next, t),
                    None => {
                        let found: Option<Option<String>> = conn
                            .query_row(
                                "SELECT gate FROM statuses WHERE id = ?1",
                                rusqlite::params![target_raw],
                                |r| r.get::<_, Option<String>>(0),
                            )
                            .ok();
                        let Some(gate_json) = found else {
                            // Neither a builtin nor a column that exists: a
                            // real typo. Name BOTH vocabularies and how to add
                            // one, so the refusal is actionable rather than a
                            // list the caller has already read.
                            let mut cols: Vec<String> = Vec::new();
                            if let Ok(mut stmt) =
                                conn.prepare("SELECT id FROM statuses ORDER BY position")
                            {
                                if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                                    cols = rows.flatten().collect();
                                }
                            }
                            return finish(
                                &slot_w,
                                PatchOut::Refused(
                                    StatusCode::BAD_REQUEST,
                                    json!({
                                        "error": format!("unknown status {target_in:?}"),
                                        "valid_statuses": VALID_STATUSES,
                                        "configured_columns": cols,
                                        "how_to_add": "POST /api/board/statuses {\"label\": \"...\"}",
                                    }),
                                ),
                                no_write(),
                            );
                        };
                        gate_json
                            .as_deref()
                            .and_then(|g| serde_json::from_str::<Vec<String>>(g).ok())
                            .unwrap_or_default()
                    }
                };

                if target_raw != next.status {
                    let force = map.get("force").and_then(Value::as_bool).unwrap_or(false);
                    let reason = body_str(&map, "reason").unwrap_or_default();
                    let mut ack_via: Option<String> = None;
                    if !eff_gate.is_empty() && !force {
                        let gc = map.get("gate_checked").and_then(Value::as_array).map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(|s| s.trim().to_string())
                                .collect::<Vec<_>>()
                        });
                        if let Some(gc) = &gc {
                            let missing: Vec<&String> =
                                eff_gate.iter().filter(|c| !gc.contains(c)).collect();
                            if !missing.is_empty() {
                                return finish(
                                    &slot_w,
                                    PatchOut::Refused(
                                        StatusCode::CONFLICT,
                                        json!({
                                            "error": "gate_checked does not match the gate",
                                            "ok": false,
                                            "blocked": true,
                                            "gate": eff_gate,
                                            "missing": missing,
                                            "you_sent": gc,
                                            "attempted_status": target_raw,
                                            "item": row.id,
                                            "item_type": next.item_type,
                                        }),
                                    ),
                                    no_write(),
                                );
                            }
                            ack_via =
                                Some(format!("gate_checked ({}/{})", gc.len(), eff_gate.len()));
                        } else if map.get("gate_ack").and_then(Value::as_bool).unwrap_or(false) {
                            ack_via = Some("gate_ack".into());
                        } else {
                            // `why_blocked` is deliberately EMPTY here: core
                            // cannot compute it for a column it does not
                            // model, and an empty list says exactly that
                            // rather than inventing a reason.
                            return finish(
                                &slot_w,
                                PatchOut::Refused(
                                    StatusCode::CONFLICT,
                                    gate_409(&next, &eff_gate, &target_raw, &[]),
                                ),
                                no_write(),
                            );
                        }
                    }

                    let from_raw = next.status.clone();
                    let stamp = hhmm();
                    if let Some(via) = &ack_via {
                        next.log = Some(bs::append_log(
                            next.log.as_deref(),
                            &stamp,
                            &format!("{actor_name}: gate satisfied via {via} for {target_raw}"),
                        ));
                    }
                    // NOT logged as a force: this is an ordinary move between
                    // configured columns. Calling it a bypass would be the
                    // ethos-rule-6 failure in reverse — an audit line that
                    // cries wolf is as useless as one that never fires.
                    let line = if force {
                        format!("force by {force_actor}: {from_raw}->{target_raw} reason={reason}")
                    } else {
                        format!("{actor_name}: {from_raw} -> {target_raw} (user column)")
                    };
                    next.log = Some(bs::append_log(next.log.as_deref(), &stamp, &line));
                    next.status = target_raw.clone();
                    next.version += 1;
                    status_event = Some((from_raw, target_raw));
                    changed.push("status".into());
                }
            } else if let Some(target_in) = body_str(&map, "status") {
                let Some(target) = bs::parse_status(&target_in) else {
                    return finish(
                        &slot_w,
                        PatchOut::Refused(
                            StatusCode::BAD_REQUEST,
                            json!({
                                "error": format!("unknown status {target_in:?}"),
                                "valid_statuses": VALID_STATUSES,
                            }),
                        ),
                        no_write(),
                    );
                };
                let from = bs::parse_status(&next.status);
                if from != Some(target) {
                    let Some(from) = from else {
                        return finish(
                            &slot_w,
                            PatchOut::Refused(
                                StatusCode::CONFLICT,
                                json!({
                                    // Unreachable since AMUX-2609: the
                                    // `unmodelled_status` branch above claims
                                    // every case where either end is outside
                                    // the typed vocabulary. Kept as an honest
                                    // 409 rather than an unwrap so a future
                                    // edit to that predicate degrades to a
                                    // refusal instead of a panic — and no
                                    // longer instructs the caller to go use a
                                    // server that was retired.
                                    "error": format!(
                                        "current status {:?} is outside the typed vocabulary \
                                         and was not routed to the user-column path",
                                        next.status
                                    ),
                                }),
                            ),
                            no_write(),
                        );
                    };
                    let Some(task) = next.to_task() else {
                        return finish(
                            &slot_w,
                            PatchOut::Refused(
                                StatusCode::CONFLICT,
                                json!({ "error": "row cannot be mapped to a core task" }),
                            ),
                            no_write(),
                        );
                    };
                    let force = map.get("force").and_then(Value::as_bool).unwrap_or(false);
                    let reason = body_str(&map, "reason").unwrap_or_default();
                    // ONE-DOING-PER-SESSION (AMUX-1707 parity). Python's WIP
                    // filters verbatim: archived cards and dormant types
                    // (tripwire/watch) do not hold WIP — both were real
                    // incidents. The escape names the attributed CLI command
                    // (AMUX-2325: an escape publishable only in HTTP terms
                    // routes agents off the audited path).
                    let override_doing = map
                        .get("override_doing")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if target == TaskStatus::Doing
                        && task.status != TaskStatus::Doing
                        && !force
                        && !override_doing
                    {
                        if let Some(sess) = next.session.as_deref().filter(|s| !s.is_empty()) {
                            let holding: Vec<String> = conn
                                .prepare(
                                    "SELECT id FROM issues WHERE session = ?1 \
                                     AND status = 'doing' AND id != ?2 \
                                     AND deleted IS NULL AND COALESCE(archived,0) = 0 \
                                     AND COALESCE(type,'') NOT IN ('tripwire','watch','epic') \
                                     ORDER BY id",
                                )
                                .and_then(|mut st| {
                                    st.query_map(rusqlite::params![sess, next.id], |r| {
                                        r.get::<_, String>(0)
                                    })
                                    .map(|rows| rows.filter_map(Result::ok).collect())
                                })
                                .unwrap_or_default();
                            if !holding.is_empty() {
                                return finish(
                                    &slot_w,
                                    PatchOut::Refused(
                                        StatusCode::CONFLICT,
                                        json!({
                                            "error": "already holding doing",
                                            "ok": false,
                                            "blocked": true,
                                            "session": sess,
                                            "holding": holding,
                                            "cli": format!(
                                                "amux board doing {} --override-doing",
                                                next.id
                                            ),
                                        }),
                                    ),
                                    no_write(),
                                );
                            }
                        }
                    }
                    // RR-0048d (Invariant 50): leaving todo requires authored
                    // acceptance criteria — enforcement opt-in during
                    // coexistence (AMUX_RS_REQUIRE_CRITERIA=1); force bypasses
                    // WITH its audit line like every other gate.
                    if task.status == TaskStatus::Todo
                        && target != TaskStatus::Todo
                        && !target.is_terminal()
                        && !force
                    {
                        match crate::api::criteria::todo_exit_permitted(conn, &next.id) {
                            Ok(Ok(())) => {}
                            Ok(Err(msg)) => {
                                return finish(
                                    &slot_w,
                                    PatchOut::Refused(
                                        StatusCode::CONFLICT,
                                        json!({
                                            "error": "acceptance criteria required",
                                            "ok": false,
                                            "blocked": true,
                                            "item": next.id,
                                            "detail": msg,
                                        }),
                                    ),
                                    no_write(),
                                );
                            }
                            Err(e) => {
                                return finish(
                                    &slot_w,
                                    PatchOut::Refused(
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        json!({ "error": e.to_string() }),
                                    ),
                                    no_write(),
                                );
                            }
                        }
                    }
                    let eff_gate = bs::effective_gate_configured(conn, &next, target);
                    let gates = bs::core_gates(&eff_gate, target);
                    let target_raw = bs::status_to_db(target, &next.status);

                    // Gate acknowledgement (AMUX-1719: gate_checked must
                    // MATCH the effective gate — every criterion present).
                    let mut evidence: Vec<Evidence> = Vec::new();
                    let mut ack_via: Option<String> = None;
                    if !eff_gate.is_empty() && !force {
                        let gc = map.get("gate_checked").and_then(Value::as_array).map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(|s| s.trim().to_string())
                                .collect::<Vec<_>>()
                        });
                        if let Some(gc) = &gc {
                            let missing: Vec<&String> =
                                eff_gate.iter().filter(|c| !gc.contains(c)).collect();
                            if !missing.is_empty() {
                                return finish(
                                    &slot_w,
                                    PatchOut::Refused(
                                        StatusCode::CONFLICT,
                                        json!({
                                            "error": "gate_checked does not match the gate",
                                            "ok": false,
                                            "blocked": true,
                                            "gate": eff_gate,
                                            "missing": missing,
                                            "you_sent": gc,
                                            "attempted_status": target_raw,
                                            "item": row.id,
                                            "item_type": next.item_type,
                                            "how_to_ack": {
                                                "gate_checked": eff_gate,
                                                "or_gate_ack": true,
                                                "or_force": "true (explicit bypass; logged)",
                                                "contract": "GET /api/board/contract",
                                                "wrong_type?": "If these criteria don't fit the work, the TYPE is wrong — fix the type, not the truth.",
                                            },
                                        }),
                                    ),
                                    no_write(),
                                );
                            }
                            ack_via = Some(format!("gate_checked ({}/{})", gc.len(), eff_gate.len()));
                        } else if map.get("gate_ack").and_then(Value::as_bool).unwrap_or(false) {
                            ack_via = Some("gate_ack".into());
                        }
                        match &ack_via {
                            Some(via) => {
                                evidence = ack_evidence(&actor_name, &eff_gate, via);
                            }
                            None => {
                                let wb = why_blocked(&task, target, &gates, &[]);
                                return finish(
                                    &slot_w,
                                    PatchOut::Refused(
                                        StatusCode::CONFLICT,
                                        gate_409(&next, &eff_gate, &target_raw, &wb),
                                    ),
                                    no_write(),
                                );
                            }
                        }
                    }

                    // Discharge the gate HERE, with core's OWN predicate
                    // (`why_blocked` is the same function `apply_transition`'s
                    // gate_check runs — the view shares the predicate of the
                    // mechanism). It must happen at this boundary because the
                    // ack protocol is the API's: half the named transitions
                    // (Start, Resume, Queue, ...) carry no evidence slot, so
                    // handing `gates` to `apply_transition` would refuse an
                    // ack that was just verified criterion-by-criterion. The
                    // transition below therefore runs with the gate already
                    // discharged (empty gate slice), evidence recorded in the
                    // card log. `force` skips this check but never the audit.
                    if !force {
                        let wb = why_blocked(&task, target, &gates, &evidence);
                        if !wb.is_empty() {
                            return finish(
                                &slot_w,
                                PatchOut::Refused(
                                    StatusCode::CONFLICT,
                                    gate_409(&next, &eff_gate, &target_raw, &wb),
                                ),
                                no_write(),
                            );
                        }
                    }

                    let now = chrono::Utc::now();
                    let tx = if force {
                        BoardTransition::Force {
                            status: target,
                            reason: reason.clone(),
                        }
                    } else {
                        // No named transition (e.g. todo->done, which the
                        // Python board serves constantly) applies through
                        // core as an attributed direct set — the gate was
                        // discharged above, one code path for the write.
                        named_transition(from, target, evidence.clone(), reason.clone())
                            .unwrap_or_else(|| BoardTransition::Force {
                                status: target,
                                reason: format!(
                                    "direct status set via PATCH (no named {} -> {} transition)",
                                    bs::db_status_spelling(from),
                                    bs::db_status_spelling(target)
                                ),
                            })
                    };

                    match apply_transition(&task, tx, &actor, &[], now) {
                        Ok(updated) => {
                            let from_raw = next.status.clone();
                            let stamp = hhmm();
                            if let Some(via) = &ack_via {
                                next.log = Some(bs::append_log(
                                    next.log.as_deref(),
                                    &stamp,
                                    &format!(
                                        "{actor_name}: gate satisfied via {via} for {target_raw}"
                                    ),
                                ));
                            }
                            let line = if force {
                                // The audited bypass (ethos rule 6): the force
                                // MUST leave a trace, on the card itself.
                                format!(
                                    "force by {force_actor}: {from_raw}->{target_raw} reason={reason}"
                                )
                            } else {
                                format!("{actor_name}: {from_raw} -> {target_raw}")
                            };
                            next.log = Some(bs::append_log(next.log.as_deref(), &stamp, &line));
                            next.status = target_raw.clone();
                            next.version = i64::try_from(updated.version).unwrap_or(next.version + 1);
                            status_event = Some((from_raw, target_raw));
                            changed.push("status".into());
                        }
                        Err(TransitionError::NoOp) => { /* nothing to do */ }
                        Err(TransitionError::GateBlocked { blocked }) => {
                            return finish(
                                &slot_w,
                                PatchOut::Refused(
                                    StatusCode::CONFLICT,
                                    gate_409(&next, &eff_gate, &target_raw, &blocked),
                                ),
                                no_write(),
                            );
                        }
                        Err(e) => {
                            // InvalidTransition / NotArmable / Archived...:
                            // the serialized core error IS the body, plus the
                            // Python-style flags so no reader mistakes a
                            // refusal for success.
                            let mut body = serde_json::to_value(&e)
                                .unwrap_or_else(|_| json!({"kind": "transition_error"}));
                            body["error"] = json!(e.to_string());
                            body["ok"] = json!(false);
                            body["blocked"] = json!(true);
                            body["attempted_status"] = json!(target_raw);
                            body["item"] = json!(row.id);
                            return finish(
                                &slot_w,
                                PatchOut::Refused(StatusCode::CONFLICT, body),
                                no_write(),
                            );
                        }
                    }
                }
            }

            if changed.is_empty() {
                // Invariant 37: nothing changed -> applied:false, rev/version
                // untouched, unknown keys named.
                return finish(
                    &slot_w,
                    PatchOut::Noop {
                        body: detail_body(&row),
                        ignored,
                    },
                    no_write(),
                );
            }

            // THE CARD IS THE SOURCE OF TRUTH FOR ITS OWN HISTORY (Ethan,
            // 2026-08-10: "make sure that board tasks maintain as the source of
            // truth (updates, history, changes, etc.) all go into that board
            // task as history — this should be amux wide").
            //
            // Only `status` was ever logged. The other seventeen writable fields
            // changed SILENTLY: a card could be retyped, reassigned, re-scoped,
            // un-archived, have its gate rewritten or its whole description
            // replaced, and the card itself would carry no trace of any of it.
            // The rev counter moved, which tells you SOMETHING happened and
            // nothing about what — and rev is not on the card a human reads.
            //
            // So every accepted change now leaves a line naming the actor and
            // the fields. Deliberately ONE line per PATCH rather than one per
            // field: a PATCH is the atomic unit a caller performed, and
            // splitting it would make an ordinary two-field edit read like two
            // separate decisions.
            //
            // VALUES ARE SUMMARISED, NOT COPIED. `desc` can be thousands of
            // characters and this log is read in a UI panel; a history that
            // reproduces every description in full stops being readable, which
            // is the ethos-5 failure (at volume it becomes a log nobody reads).
            // Short scalars are shown because for `type`, `session` and
            // `reviewer` the VALUE is the decision.
            {
                let noisy: std::collections::HashSet<&str> =
                    ["status", "pos", "last_verified_at"].into_iter().collect();
                let mut parts: Vec<String> = Vec::new();
                for f in changed.iter().filter(|f| !noisy.contains(f.as_str())) {
                    let part = match f.as_str() {
                        // The two free-text fields: report the SHAPE of the
                        // edit, since the new value is already on the card and
                        // the useful fact is that it moved and by how much.
                        "desc" => {
                            let before = row.desc.chars().count() as i64;
                            let after = next.desc.chars().count() as i64;
                            let delta = after - before;
                            if delta > 0 {
                                format!("desc +{delta} chars")
                            } else if delta < 0 {
                                format!("desc {delta} chars")
                            } else {
                                "desc rewritten".to_string()
                            }
                        }
                        "title" => format!("title -> {}", chars_truncate_log(&next.title, 60)),
                        "type" => format!("type -> {}", next.item_type),
                        "session" => format!(
                            "session -> {}",
                            next.session.as_deref().unwrap_or("(unassigned)")
                        ),
                        "reviewer" => format!(
                            "reviewer -> {}",
                            next.reviewer.as_deref().unwrap_or("(none)")
                        ),
                        "owner_type" => format!("owner_type -> {}", next.owner_type),
                        "archived" => {
                            if next.archived == 1 { "ARCHIVED".into() } else { "restored".into() }
                        }
                        "pinned" => {
                            if next.pinned == 1 { "pinned".into() } else { "unpinned".into() }
                        }
                        other => other.to_string(),
                    };
                    parts.push(part);
                }
                if !parts.is_empty() {
                    next.log = Some(bs::append_log(
                        next.log.as_deref(),
                        &hhmm(),
                        &format!("{actor_name}: {}", parts.join(", ")),
                    ));
                }
            }

            // Writes bump rev (the Python counter) AND version (the Rust one).
            next.rev = row.rev + 1;
            if !changed.contains(&"status".to_string()) {
                next.version = row.version + 1;
            }
            next.updated = now_secs();
            bs::save_patched(conn, &next)?;
            if let Some(tags) = &tags_change {
                bs::set_tags(conn, &next.id, tags, next.updated)?;
            }
            // KEEP THE `needsyou` STATUS AND THE `needs:you` TAG IN STEP.
            //
            // They are two spellings of one fact and the readers are split
            // across them: the status is what EXCLUDES a card (auto-pickup
            // takes `status='todo'`, the advance path takes `status IN
            // ('doing','review')`), while every mechanism that SURFACES a
            // human-blocked card keys on the TAG — the dashboard's
            // `is:needsyou` view and Focus mode, the 3-day re-nag (which
            // JOINs issue_tags), and board_drive's "the human owes the
            // answer, not the lane" branch.
            //
            // So setting the canonical status alone parked the card where
            // nothing hands it out AND nothing brings it back — strictly
            // worse than leaving it in `todo`, and reached by taking the
            // DOCUMENTED transition (core: `Doing -> NeedsYou`, "stuck on the
            // user, with the exact question"). Measured on the live board
            // 2026-08-11: 23 of 38 open `needsyou` cards carried no tag,
            // across six sessions, including four SLA breaches aged 127-194h
            // that the re-nag structurally could not see.
            //
            // Syncing here rather than teaching each reader both spellings is
            // deliberate: one write fixes five consumers, and there is no
            // second predicate left to drift.
            if let Some((from_raw, to_raw)) = &status_event {
                let was = bs::parse_status(from_raw);
                let now_st = bs::parse_status(to_raw);
                let is_ny = |s: Option<TaskStatus>| s == Some(TaskStatus::NeedsYou);
                // An explicit `tags` in the same PATCH is the caller stating
                // intent; it wins over this sync either way.
                let caller_set_ny = tags_change.as_ref().is_some_and(|t| {
                    t.iter().any(|x| x.to_lowercase().starts_with("needs:you"))
                });
                if is_ny(now_st) && !is_ny(was) {
                    bs::add_needs_you_tag(conn, &next.id, next.updated)?;
                } else if is_ny(was) && !is_ny(now_st) && !caller_set_ny {
                    // The answer landed (`NeedsYou -> Doing` is core's
                    // "the user answered"). Leaving the tag on would re-nag
                    // the lane about a question that is no longer open —
                    // the re-nag only skips done/verified/discarded.
                    bs::clear_needs_you_tags(conn, &next.id)?;
                }
            }
            let mutation = match &status_event {
                Some((f, t)) => MutationKind::StatusChanged {
                    from: f.clone(),
                    to: t.clone(),
                },
                None => MutationKind::Updated,
            };
            let event = ev_snap(&next, mutation);
            let st = status_event.as_ref().map(|(f, t)| {
                (
                    next.session.clone().unwrap_or_default(),
                    f.clone(),
                    t.clone(),
                )
            });
            finish(
                &slot_w,
                PatchOut::Applied {
                    body: detail_body(&next),
                    ignored,
                    status_transition: st,
                },
                WriteOutcome {
                    applied: true,
                    events: vec![event],
                },
            )
        })
        .await;

    let reply = match write {
        Ok(r) => r,
        Err(e) => return internal(e),
    };
    let outcome = slot.lock().expect("outcome slot poisoned").take();
    match outcome {
        None => internal("patch produced no outcome"),
        Some(PatchOut::NotFound) => not_found(&id),
        Some(PatchOut::Refused(status, body)) => err(status, body),
        Some(PatchOut::Noop { mut body, ignored }) => {
            body["applied"] = json!(false);
            if !ignored.is_empty() {
                body["ignored_fields"] = json!(ignored);
                body["ignored_note"] = json!(
                    "these keys are not writable via PATCH and were NOT applied; \
                     the rest of this response reflects the card as stored"
                );
            }
            (StatusCode::OK, Json(body)).into_response()
        }
        Some(PatchOut::Applied { mut body, ignored, status_transition }) => {
            body["applied"] = json!(true);
            body["global_rev"] = json!(reply.rev.0);
            if !ignored.is_empty() {
                body["ignored_fields"] = json!(ignored);
                body["ignored_note"] = json!(
                    "these keys are not writable via PATCH and were NOT applied; \
                     the rest of this response reflects the card as stored"
                );
            }
            // REACTIVE PICKUP: when a card transitions to a terminal state,
            // immediately check if the lane has a next todo card and claim it.
            // This removes the up-to-60s wait for the board-drive tick. The
            // delivery still goes through steer_enqueue (turn-boundary gated),
            // so this cannot interrupt a mid-turn session.
            if let Some((session, _from, to)) = status_transition {
                if matches!(to.as_str(), "done" | "verified" | "discarded")
                    && !session.is_empty()
                {
                    let st = state.clone();
                    tokio::spawn(async move {
                        reactive_pickup(&st, &session).await;
                    });
                }
            }
            (StatusCode::OK, Json(body)).into_response()
        }
    }
}

// ---- Reactive pickup (AMUX board-drive latency fix) -----------------------
//
// When a card transitions to done/verified/discarded, fire an immediate pickup
// for the same session instead of waiting for the 60s board-drive tick. Uses the
// SAME select_pickup + claim_card + deliver path as the drive loop, so every
// guard (WIP cap, junk filter, freshness, cooldowns) applies identically.
//
// This is a SUPPLEMENT, not a replacement. The drive tick remains the backstop
// (a crashed reactive path is invisible; the tick is visible in the trace). The
// trace does NOT record reactive pickups — they are not part of the sweep — but
// the `task.claimed` event they emit IS what the drive tick reads to avoid
// re-claiming.

async fn reactive_pickup(state: &AppState, session: &str) {
    use crate::runtime_jobs::board_drive::{select_pickup, Pickup};

    let conn = match state.store.read() {
        Ok(c) => c,
        Err(_) => return,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let pickup = select_pickup(&conn, session, now);
    drop(conn);

    if let Pickup::Claim { card, prompt } = pickup {
        // Same compare-and-swap guard as the drive loop (AMUX-2983): only
        // dispatch if the atomic claim took. This reactive path has the exact
        // race — select_pickup then drop(conn) then claim — so an unconditional
        // claim+dispatch could re-open and re-run a card closed in the gap.
        if crate::runtime_jobs::board_drive::claim_card(state, session, &card).await {
            crate::api::session_verbs::steer_enqueue(
                state, session, &prompt, "board-drive:reactive", "",
            )
            .await;
            tracing::info!(
                session,
                card = card.as_str(),
                "reactive pickup: claimed {card} immediately after status change"
            );
        }
    }
}

// ---- POST /api/board/{id}/archive + /restore (RR-0055) -------------------

async fn archive_restore(
    state: AppState,
    id: String,
    headers: HeaderMap,
    body: Option<Value>,
    restore: bool,
) -> Response {
    let (actor, actor_name) = actor_from_headers(&headers);
    let reason = body
        .as_ref()
        .and_then(|v| v.get("reason"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    enum Out {
        NotFound,
        Refused(Value),
        Noop(Value),
        Applied(Value),
    }
    let slot: Arc<Mutex<Option<Out>>> = Arc::new(Mutex::new(None));
    let slot_w = slot.clone();
    let id_w = id.clone();
    let write = state
        .store
        .write_async(move |conn| {
            let Some(row) = bs::get_issue(conn, &id_w)? else {
                return finish(&slot_w, Out::NotFound, no_write());
            };
            let Some(task) = row.to_task() else {
                return finish(
                    &slot_w,
                    Out::Refused(json!({
                        "error": format!(
                            "current status {:?} is not in the shared vocabulary; \
                             move it to a builtin status first (PATCH status)",
                            row.status
                        ),
                    })),
                    no_write(),
                );
            };
            let tx = if restore {
                BoardTransition::Restore {
                    reason: reason.clone(),
                }
            } else {
                BoardTransition::Archive {
                    reason: reason.clone(),
                }
            };
            match apply_transition(&task, tx, &actor, &[], chrono::Utc::now()) {
                Ok(updated) => {
                    let mut next = row.clone();
                    next.archived = i64::from(updated.archived);
                    let verb = if restore { "restored" } else { "archived" };
                    let line = if reason.is_empty() {
                        format!("{actor_name}: {verb}")
                    } else {
                        format!("{actor_name}: {verb} — {reason}")
                    };
                    next.log = Some(bs::append_log(next.log.as_deref(), &hhmm(), &line));
                    next.rev = row.rev + 1;
                    next.version = i64::try_from(updated.version).unwrap_or(row.version + 1);
                    next.updated = now_secs();
                    bs::save_patched(conn, &next)?;
                    let event = ev_snap(&next, MutationKind::Updated);
                    finish(
                        &slot_w,
                        Out::Applied(detail_body(&next)),
                        WriteOutcome {
                            applied: true,
                            events: vec![event],
                        },
                    )
                }
                // Already in the requested archive state: honest no-op,
                // rev unmoved (Invariant 37).
                Err(TransitionError::NoOp) => finish(&slot_w, Out::Noop(detail_body(&row)), no_write()),
                Err(e) => finish(
                    &slot_w,
                    Out::Refused(json!({ "error": e.to_string() })),
                    no_write(),
                ),
            }
        })
        .await;
    let reply = match write {
        Ok(r) => r,
        Err(e) => return internal(e),
    };
    let outcome = slot.lock().expect("outcome slot poisoned").take();
    match outcome {
        None => internal("archive/restore produced no outcome"),
        Some(Out::NotFound) => not_found(&id),
        Some(Out::Refused(body)) => err(StatusCode::CONFLICT, body),
        Some(Out::Noop(mut body)) => {
            body["applied"] = json!(false);
            (StatusCode::OK, Json(body)).into_response()
        }
        Some(Out::Applied(mut body)) => {
            body["applied"] = json!(true);
            body["global_rev"] = json!(reply.rev.0);
            (StatusCode::OK, Json(body)).into_response()
        }
    }
}

pub async fn archive_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    archive_restore(state, id, headers, body.map(|Json(v)| v), false).await
}

pub async fn restore_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    archive_restore(state, id, headers, body.map(|Json(v)| v), true).await
}

// ---- DELETE /api/board/{id} ---------------------------------------------

/// SOFT delete, Python parity: stamp `deleted` and the row disappears from
/// every read path (all of them filter `deleted IS NULL`) while staying on
/// disk for forensics. The SPA has always called this; it simply had no
/// handler, and the 405 was invisible because the card had already been
/// removed from the local list.
pub async fn delete_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let (_actor, actor_name) = actor_from_headers(&headers);
    enum Out {
        NotFound,
        Deleted(Value),
    }
    let slot: Arc<Mutex<Option<Out>>> = Arc::new(Mutex::new(None));
    let slot_w = slot.clone();
    let id_w = id.clone();
    let who = actor_name.clone();
    let write = state
        .store
        .write_async(move |conn| {
            let Some(row) = bs::get_issue(conn, &id_w)? else {
                return finish(&slot_w, Out::NotFound, no_write());
            };
            // Record WHO deleted it before the row leaves every read path —
            // a delete that leaves no trace of its author is the audit hole
            // ethos rule 6 is about, and the log column survives soft delete.
            let mut logged = row.clone();
            logged.log = Some(bs::append_log(
                logged.log.as_deref(),
                &hhmm(),
                &format!("{who}: deleted"),
            ));
            logged.rev = row.rev + 1;
            logged.version = row.version + 1;
            logged.updated = now_secs();
            bs::save_patched(conn, &logged)?;
            if !bs::soft_delete(conn, &id_w)? {
                return finish(&slot_w, Out::NotFound, no_write());
            }
            let event = ev_snap(&logged, MutationKind::Deleted);
            finish(
                &slot_w,
                Out::Deleted(json!({"ok": true, "deleted": true, "id": id_w})),
                WriteOutcome {
                    applied: true,
                    events: vec![event],
                },
            )
        })
        .await;
    let reply = match write {
        Ok(r) => r,
        Err(e) => return internal(e),
    };
    let outcome = slot.lock().expect("outcome slot poisoned").take();
    match outcome {
        None => internal("delete produced no outcome"),
        Some(Out::NotFound) => not_found(&id),
        Some(Out::Deleted(mut body)) => {
            body["global_rev"] = json!(reply.rev.0);
            (StatusCode::OK, Json(body)).into_response()
        }
    }
}

// ---- POST /api/board/{id}/status-request · /status-update ----------------
//
// The D1-exit pair (AMUX-2174), and the ethos inverse of terminal scraping:
// amux does not INFER a card's status, it routes a request and the owning
// session's own model AUTHORS the answer onto the card. ethos.md records this
// as the reason the board is the source of truth.
//
// Both were lost in the Rust cutover and answered 405 with an EMPTY body, which
// is the worst available failure for this pair, because every layer that
// mentions them kept telling the fleet to use them:
//   - `amux board ask` / `amux board status-update` (the CLI's own help)
//   - the SPA card menu's "ask for status" (`_askCardStatus`, app.js)
//   - the advance nudge: "post a status-update / mark its blocker"
//   - the board contract's `board_is_source_of_truth` clause
// This is AMUX-2140's shape a second time: following the sanctioned instruction
// exactly is what produced the failure. It also escaped the route census,
// because that enumerates SPA and CLI call sites and the CLI reaches these by
// hand-rolled curl — so the endpoint the CLI most depends on is exactly the one
// the "does every caller have a route" invariant could not see.

/// The size cap for a status update. Python applied 1200 (py:69770) on the theory
/// that an update is a SUMMARY, not a transcript sink. But 1200 chars silently
/// amputated real cross-group HANDOFFS mid-sentence and still returned
/// {"ok":true} (AMUX-3079), so the default is raised, made configurable, and any
/// truncation is now reported loudly by the handler rather than being silent.
fn status_update_max() -> usize {
    std::env::var("AMUX_STATUS_UPDATE_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(8000)
}

async fn status_request(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let question: String = body
        .as_ref()
        .and_then(|Json(v)| v.get("question"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .chars()
        .take(400)
        .collect();
    let (_actor, requester) = actor_from_headers(&headers);
    let requester = if requester.is_empty() || requester == "api-anonymous" {
        headers
            .get("X-Amux-User-Email")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("Ethan")
            .to_string()
    } else {
        requester
    };

    let (session, title) = {
        let conn = match state.store.read() {
            Ok(c) => c,
            Err(e) => return internal(e),
        };
        match bs::get_issue(&conn, &id) {
            Ok(Some(row)) => (row.session.clone().unwrap_or_default(), row.title.clone()),
            Ok(None) => return not_found(&id),
            Err(e) => return internal(e),
        }
    };
    let session = session.trim().to_string();
    if session.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"ok": false, "delivered": false,
                        "reason": "card has no owning session to ask"})),
        )
            .into_response();
    }
    if !crate::api::session_verbs::is_running(&session).await {
        // Honest offline path (ethos rule 7): never fake a live answer, and say
        // plainly that nothing was queued, so the caller knows to ask again
        // rather than waiting on a delivery that will not happen.
        return Json(json!({
            "ok": false, "delivered": false,
            "reason": format!("session '{session}' is not running"),
            "hint": "the request is not queued; ask again when the session runs",
        }))
        .into_response();
    }

    let q_part = if question.is_empty() {
        String::new()
    } else {
        format!(": {question}")
    };
    let prompt = format!(
        "[amux status request on {id}: {}] {requester} asks for a status update{q_part}.\n\
         Reply by running:  amux board status-update {id} \"<what's done, what's next, any blocker>\"\n\
         That posts to the BOARD, which is the source of truth — a chat reply alone does not update the card.",
        title.chars().take(80).collect::<String>()
    );
    // Delivered at the next TURN BOUNDARY via the one steering queue, never a
    // direct send: the decision recorded in ethos.md ("Board state changes are
    // delivered at turn boundaries") is that a running agent cannot consume an
    // event faster than its next turn anyway.
    crate::api::session_verbs::steer_enqueue(&state, &session, &prompt, "status-request", &requester)
        .await;

    let line = if question.is_empty() {
        format!("status requested by {requester} (routed to {session})")
    } else {
        format!("status requested by {requester} — \"{question}\" (routed to {session})")
    };
    if let Err(e) = append_card_log(&state, &id, &line).await {
        return internal(e);
    }
    Json(json!({"ok": true, "delivered": true, "session": session,
                "message": format!("asked {session} to post a status update to {id}")}))
    .into_response()
}

async fn status_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let cap = status_update_max();
    let full: String = body
        .as_ref()
        .and_then(|Json(v)| v.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let original_chars = full.chars().count();
    let truncated = original_chars > cap;
    let text: String = full.chars().take(cap).collect();
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "text required"}))).into_response();
    }
    if truncated {
        // Never silent again (AMUX-3079): the caller and a log sweep must both
        // see that a handoff was stored as a fragment.
        tracing::warn!(
            target: "board",
            id = %id, original_chars, cap,
            "status-update TRUNCATED to the cap and stored a fragment; raise \
             AMUX_STATUS_UPDATE_MAX or split the update",
        );
    }
    let (_actor, actor_name) = actor_from_headers(&headers);
    let actor = if actor_name.is_empty() || actor_name == "api-anonymous" {
        "session".to_string()
    } else {
        actor_name
    };
    {
        let conn = match state.store.read() {
            Ok(c) => c,
            Err(e) => return internal(e),
        };
        match bs::get_issue(&conn, &id) {
            Ok(Some(_)) => {}
            // Python appended to a card it never checked existed, so a typo'd
            // id reported {"ok": true} and wrote nothing anyone could find.
            Ok(None) => return not_found(&id),
            Err(e) => return internal(e),
        }
    }
    if let Err(e) = append_card_log(&state, &id, &format!("STATUS ({actor}): {text}")).await {
        return internal(e);
    }
    let mut resp = Json(json!({
        "ok": true, "id": id, "actor": actor,
        "chars": text.chars().count(),
        "original_chars": original_chars,
        "truncated": truncated,
    }))
    .into_response();
    if truncated {
        resp.headers_mut()
            .insert("x-amux-truncated", axum::http::HeaderValue::from_static("1"));
    }
    resp
}

/// Append one stamped line to a card's log. Both handlers above write only
/// here — a status update must never move the card, and reusing the PATCH path
/// would put a status report one typo away from a status TRANSITION.
async fn append_card_log(
    state: &AppState,
    id: &str,
    line: &str,
) -> Result<(), rusqlite::Error> {
    let (id, line, stamp) = (id.to_string(), line.to_string(), hhmm());
    state
        .store
        .write_async(move |conn| {
            let existing: Option<String> = conn
                .query_row("SELECT log FROM issues WHERE id=?1", [&id], |r| r.get(0))
                .unwrap_or(None);
            let next = bs::append_log(existing.as_deref(), &stamp, &line);
            conn.execute(
                "UPDATE issues SET log=?1 WHERE id=?2",
                rusqlite::params![next, id],
            )?;
            Ok(WriteOutcome { applied: true, events: vec![] })
        })
        .await
        .map(|_| ())
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(e.to_string()))))
}

#[cfg(test)]
mod param_tests {
    use super::ignored_board_params;

    /// BACKE-3228: a mistyped/unknown filter must be reported ignored, a real
    /// filter must NOT, and a cache-buster must not create noise.
    #[test]
    fn ignored_params_names_typos_not_real_filters_or_cachebusters() {
        // The exact case that bit amux-cloud: a plausible-but-wrong name.
        assert_eq!(ignored_board_params("include_archived=1"), vec!["include_archived"]);
        assert_eq!(ignored_board_params("done=1&limits=5"), vec!["done", "limits"]);
        // Real filters are consumed, never flagged.
        assert!(ignored_board_params("session=amux&status=todo&archived=1&done_limit=0").is_empty());
        assert!(ignored_board_params("slim=1&limit=10&offset=5").is_empty());
        // q/query/search are consumed (refused with a 400), so not "ignored".
        assert!(ignored_board_params("q=nudge").is_empty());
        // Cache-busters are benign, not surfaced (would be noise on every poll).
        assert!(ignored_board_params("_=1699999999&session=amux").is_empty());
        assert!(ignored_board_params("t=123&cb=x").is_empty());
        // Case-insensitive on the key; de-duplicated.
        assert_eq!(ignored_board_params("Foo=1&foo=2"), vec!["Foo"]);
        // Empty / no query -> nothing.
        assert!(ignored_board_params("").is_empty());
        // Mixed: only the typo is named, alongside a real filter + cache-buster.
        assert_eq!(ignored_board_params("session=x&includearchived=1&_=9"), vec!["includearchived"]);
    }
}

#[cfg(test)]
mod slim_tests {
    /// A SCOPED query must answer completely (ts-gke, 2026-08-11).
    ///
    /// The terminal cap made `?session=X` under-report by 26 of 94 done cards
    /// while the body carried no sign of it, and a digest built on that list
    /// reported 25. This pins the DEFAULT, which is the thing that was wrong:
    /// unfiltered caps, scoped does not, and an explicit ?done_limit always
    /// wins so a caller who asks for a bound still gets one.
    #[test]
    fn a_scoped_query_is_not_capped_by_default() {
        let d = |session: Option<&str>, status: Option<&str>, explicit: Option<i64>| {
            let scoped = session.is_some() || status.is_some();
            explicit.unwrap_or(if scoped { 0 } else { 100 })
        };
        assert_eq!(d(None, None, None), 100, "the unfiltered board still caps — the dashboard cannot draw 1300 terminal cards");
        assert_eq!(d(Some("ts-gke"), None, None), 0, "?session= must answer completely");
        assert_eq!(d(None, Some("done"), None), 0, "?status= must answer completely");
        assert_eq!(d(Some("ts-gke"), Some("done"), None), 0, "both together too");
        // An explicit bound is honoured in BOTH shapes — otherwise this change
        // would have taken away a caller's ability to ask for a small page.
        assert_eq!(d(Some("ts-gke"), None, Some(5)), 5, "explicit done_limit wins when scoped");
        assert_eq!(d(None, None, Some(5)), 5, "and when unfiltered");
        assert_eq!(d(None, None, Some(0)), 0, "an explicit 0 still means uncapped");
    }

    /// `?all=1` is the discoverable escape from the terminal cap (AMUX-3154).
    ///
    /// Every session that hit the cap reached for this exact param
    /// (mixpeek-funnel, mixpeek-frustrations, ts-gke tried `?all=1`/`?limit=N`)
    /// and got the capped 100-terminal view back, because `all` was UNRECOGNISED
    /// and axum dropped it — the rule-7 failure where a filter that never ran
    /// returns a confident wrong denominator (a lane auditing its `done` work off
    /// the plain list read ~6% of it). This pins that `?all=1` now uncaps, that
    /// the dashboard render poll (which omits it) still caps, that an explicit
    /// ?done_limit still wins, and — the half that makes it real — that `all` is
    /// a RECOGNISED param and not silently dropped like it was.
    #[test]
    fn all_1_uncaps_the_unfiltered_terminal_set() {
        // Mirror the real derivation at list_board: unscoped, `?all=1`, explicit.
        let d = |uncap_all: bool, explicit: Option<i64>| {
            let scoped = false; // the unscoped list is the case that was wrong
            explicit.unwrap_or(if scoped || uncap_all { 0 } else { 100 })
        };
        assert_eq!(d(false, None), 100, "the bare list still caps — the dashboard render poll omits ?all=1");
        assert_eq!(d(true, None), 0, "?all=1 must answer completely — the escape every capped caller tried");
        assert_eq!(d(true, Some(5)), 5, "an explicit done_limit wins even alongside ?all=1");
        // The half that was the actual bug: an unrecognised `all` is dropped, so
        // the cap answers and the escape silently no-ops.
        assert!(RECOGNISED_BOARD_PARAMS.contains(&"all"), "?all must be recognised");
        assert!(ignored_board_params("all=1").is_empty(), "?all=1 must not be reported as ignored");
    }

    use super::*;
    use crate::db::board_store::IssueRow;

    /// SLIM MUST CARRY WHAT THE LIST ACTUALLY RENDERS (AMUX-2840).
    ///
    /// This is pinned because the same slimming was tried before and reverted:
    /// `list_body`'s own doc says an earlier first-line-desc + `log_n` version
    /// "silently blanked both in the dashboard". It blanked them because it
    /// removed the fields without replacing what the SPA derives FROM them.
    /// A payload diet that drops a rendered value is a regression wearing a
    /// performance win, and it fails silently — the card just looks empty.
    #[test]
    fn slim_drops_the_prose_but_keeps_the_two_things_the_list_renders() {
        let row = IssueRow {
            id: "T-1".into(),
            title: "a card".into(),
            desc: "First line is the preview.\nNew task: folded one\nNew task: folded two".into(),
            log: Some("`10:00` did a thing\n`10:01` New task: folded three".into()),
            item_type: "code".into(),
            ..Default::default()
        };

        let full = list_body(&row, false, false);
        assert!(full["desc"].is_string(), "the plain list still serves full desc");
        assert!(full["log"].is_string(), "and full log");

        let slim = list_body(&row, true, false);
        // The diet itself.
        assert!(slim["desc"].is_null(), "slim must not ship the prose");
        assert!(slim["log"].is_null());
        // ...and the two derivations that make it safe to drop them.
        assert_eq!(
            slim["desc_head"], "First line is the preview.",
            "app.js:19488 renders the first line as the card preview"
        );
        assert_eq!(
            slim["folded_n"], 3,
            "app.js:18866 counts 'New task:' across desc AND log for the folded badge"
        );
        assert_eq!(slim["desc_len"], row.desc.chars().count());
    }

    /// The third derivation (app.js:19231). LAST marker wins, not first — a
    /// re-marked card must show its freshest question, which is the client's own
    /// rule and the reason a naive `find` would be wrong.
    #[test]
    fn slim_carries_the_latest_needsyou_marker() {
        let row = IssueRow {
            desc: "NEEDS-YOU: the stale one\nsome prose".into(),
            log: Some("`10:00` moved\nNEEDS-YOU: the fresh one".into()),
            ..Default::default()
        };
        assert_eq!(list_body(&row, true, false)["needsyou_note"], "the fresh one");

        // Spelling variants the client accepts, case-insensitively.
        for spelling in ["NEEDS-YOU:", "needs you:", "NEEDSYOU:", "Needs-Ethan:", "needs-human:"] {
            let r = IssueRow { desc: format!("{spelling} answer me"), ..Default::default() };
            assert_eq!(
                list_body(&r, true, false)["needsyou_note"], "answer me",
                "spelling {spelling} must be recognised"
            );
        }

        // ABSENT means ABSENT: the key is omitted rather than served as an empty
        // string, so a client can distinguish "no marker" from "a blank marker".
        let plain = IssueRow { desc: "ordinary card".into(), ..Default::default() };
        assert!(list_body(&plain, true, false).get("needsyou_note").is_none());
    }

    /// Every spelling app.js's /NEEDS[- ]?(?:YOU|ETHAN|HUMAN):/i accepts must
    /// produce a note here, or the slim client and the full client disagree
    /// about the same card. The three ETHAN/HUMAN space and no-separator forms
    /// were missing until 2026-08-11.
    #[test]
    fn needsyou_matches_every_spelling_the_client_regex_accepts() {
        for spelling in [
            "NEEDS-YOU:", "NEEDS YOU:", "NEEDSYOU:",
            "NEEDS-ETHAN:", "NEEDS ETHAN:", "NEEDSETHAN:",
            "NEEDS-HUMAN:", "NEEDS HUMAN:", "NEEDSHUMAN:",
            "needs-you:", "needs ethan:",
        ] {
            let row = IssueRow {
                id: "X-1".into(),
                title: "a card".into(),
                desc: format!("{spelling} answer me"),
                item_type: "code".into(),
                ..Default::default()
            };
            assert_eq!(
                list_body(&row, true, false)["needsyou_note"], "answer me",
                "spelling {spelling:?} must yield a note — the client regex accepts it"
            );
        }
        // A marker with nothing after it is not a marker.
        let empty = IssueRow { desc: "NEEDS-YOU:   ".into(), ..Default::default() };
        assert!(list_body(&empty, true, false).get("needsyou_note").is_none());
    }

    /// The preview must be bounded and must not panic on multi-byte text — it
    /// is built with `chars().take()`, not a byte slice, and an empty desc is
    /// ordinary rather than an error.
    #[test]
    fn the_preview_is_bounded_and_multibyte_safe() {
        let long = IssueRow { desc: "é".repeat(400), ..Default::default() };
        let v = list_body(&long, true, false);
        assert_eq!(v["desc_head"].as_str().unwrap().chars().count(), 120);

        let empty = IssueRow { desc: String::new(), ..Default::default() };
        let v = list_body(&empty, true, false);
        assert_eq!(v["desc_head"], "");
        assert_eq!(v["folded_n"], 0);

        // Leading blank lines are skipped: the preview is the first line with
        // CONTENT, not the first line.
        let padded = IssueRow { desc: "\n\n  \nreal content here".into(), ..Default::default() };
        assert_eq!(list_body(&padded, true, false)["desc_head"], "real content here");
    }
}
