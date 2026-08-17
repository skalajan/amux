//! RR-0110 — universal search (FTS5): index maintenance and ranking.
//!
//! These tests write to the source tables with RAW SQL rather than through the
//! board/memory APIs, on purpose. The design claim of migration 0013 is that
//! the index is maintained by SQLite triggers and therefore cannot be bypassed
//! by a writer that forgets to index — a test that went through the API would
//! pass equally well against a Rust write-hook implementation, so it could not
//! tell the two apart and could not fail on the thing being claimed.

use amux_server::api::{router, AppState};
use amux_server::db::Store;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

fn app() -> (axum::Router, Arc<Store>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("amux-test.db")).unwrap());
    let state = AppState {
        store: store.clone(),
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: None,
    };
    (router(state), store, dir)
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    let req = Request::builder().method("GET").uri(path).body(Body::empty()).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

/// Insert a board card straight into `issues`. `log` is the card's history
/// column (`` `HH:MM` text `` lines).
fn card(store: &Store, id: &str, title: &str, desc: &str, log: Option<&str>) {
    let (id, title, desc, log) = (
        id.to_string(),
        title.to_string(),
        desc.to_string(),
        log.map(str::to_string),
    );
    store
        .write(move |conn| {
            conn.execute(
                "INSERT INTO issues (id, title, desc, status, session, creator, created, updated, log)
                 VALUES (?1, ?2, ?3, 'todo', 'lane-a', 'tester', 1785000000, 1785000000, ?4)",
                rusqlite::params![id, title, desc, log],
            )?;
            Ok(amux_server::db::WriteOutcome { applied: false, events: vec![] })
        })
        .unwrap();
}

fn hit_ids(v: &Value) -> Vec<String> {
    v["hits"]
        .as_array()
        .map(|a| a.iter().map(|h| h["id"].as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn fts5_is_compiled_in_and_the_migration_created_the_index() {
    // If the bundled SQLite lacked FTS5 the migration would have failed at
    // Store::open with "no such module: fts5" — this asserts the shape rather
    // than trusting that the build flags are what we think they are.
    let (_app, store, _d) = app();
    let conn = store.read().unwrap();
    let kinds: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE name IN ('search_docs','search_fts')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(kinds.len(), 2, "both the content table and the FTS index must exist: {kinds:?}");
}

#[tokio::test]
async fn index_counts_match_table_counts_and_status_says_so() {
    let (app, store, _d) = app();
    for i in 1..=5 {
        card(&store, &format!("T-{i}"), &format!("card {i}"), "body text", None);
    }
    let (st, v) = get(&app, "/api/search/status").await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["consistent"].as_bool().unwrap(), "status: {v}");
    let task = v["families"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["type"] == "task")
        .cloned()
        .unwrap();
    assert_eq!(task["indexed"], 5);
    assert_eq!(task["live"], 5);
    assert_eq!(v["docs_total"], v["fts_rows"], "content table and FTS index must agree");
}

#[tokio::test]
async fn a_term_present_only_in_the_card_log_is_findable() {
    let (app, store, _d) = app();
    // The discriminating case: `parsnip` appears in NEITHER the title nor the
    // desc — only in a history line. A body built from title+desc would pass
    // every other test in this file and fail this one.
    card(&store, "T-1", "unrelated title", "unrelated description", Some("`09:12` moved to doing by parsnip\n"));
    card(&store, "T-2", "another card", "nothing to see", None);

    let (_, v) = get(&app, "/api/search?q=parsnip").await;
    assert_eq!(hit_ids(&v), vec!["T-1"], "log-only term must be findable: {v}");
    assert!(
        v["hits"][0]["snippet"].as_str().unwrap().contains("<mark>parsnip</mark>"),
        "the match must be highlighted where it was found: {}",
        v["hits"][0]["snippet"]
    );
}

#[tokio::test]
async fn deleting_a_card_removes_it_from_the_index() {
    let (app, store, _d) = app();
    card(&store, "T-1", "quokka sighting", "in the desc", None);
    let (_, v) = get(&app, "/api/search?q=quokka").await;
    assert_eq!(hit_ids(&v), vec!["T-1"]);

    // The board soft-deletes (sets `deleted`), which is the path that actually
    // happens; a hard DELETE is covered below because a restored/repaired DB
    // can take that path.
    store
        .write(|conn| {
            conn.execute("UPDATE issues SET deleted = 1785000001 WHERE id = 'T-1'", [])?;
            Ok(amux_server::db::WriteOutcome { applied: false, events: vec![] })
        })
        .unwrap();
    let (_, v) = get(&app, "/api/search?q=quokka").await;
    assert!(hit_ids(&v).is_empty(), "soft-deleted card must leave the index: {v}");

    // …and the index must not be left holding an orphan row either, which a
    // hit count alone would not reveal.
    let (_, st) = get(&app, "/api/search/status").await;
    assert_eq!(st["docs_total"], 0, "{st}");
    assert_eq!(st["fts_rows"], 0, "the FTS side must be cleaned too: {st}");
    assert!(st["consistent"].as_bool().unwrap());

    card(&store, "T-2", "quokka again", "x", None);
    store
        .write(|conn| {
            conn.execute("DELETE FROM issues WHERE id = 'T-2'", [])?;
            Ok(amux_server::db::WriteOutcome { applied: false, events: vec![] })
        })
        .unwrap();
    let (_, v) = get(&app, "/api/search?q=quokka").await;
    assert!(hit_ids(&v).is_empty(), "hard-deleted card must leave the index: {v}");
}

#[tokio::test]
async fn updating_a_card_reindexes_rather_than_duplicating() {
    let (app, store, _d) = app();
    card(&store, "T-1", "before", "old word: tapir", None);
    store
        .write(|conn| {
            conn.execute("UPDATE issues SET desc = 'new word: okapi', updated = 1785000002 WHERE id = 'T-1'", [])?;
            Ok(amux_server::db::WriteOutcome { applied: false, events: vec![] })
        })
        .unwrap();
    let (_, old) = get(&app, "/api/search?q=tapir").await;
    assert!(hit_ids(&old).is_empty(), "the superseded text must not still match: {old}");
    let (_, new) = get(&app, "/api/search?q=okapi").await;
    assert_eq!(hit_ids(&new), vec!["T-1"]);
    let (_, st) = get(&app, "/api/search/status").await;
    assert_eq!(st["docs_total"], 1, "an update must not leave a second doc: {st}");
}

#[tokio::test]
async fn ranking_puts_a_title_match_above_a_body_match() {
    let (app, store, _d) = app();
    // Same term, different field. bm25 weights title 10x, so the title hit
    // must come first regardless of insertion order — which is why the body
    // card is inserted FIRST (a stable-order bug would pass otherwise).
    card(&store, "BODY-1", "some other heading", "a long description that mentions numbat once", None);
    card(&store, "TITLE-1", "numbat", "a long description with no such term at all", None);

    let (_, v) = get(&app, "/api/search?q=numbat").await;
    let ids = hit_ids(&v);
    assert_eq!(ids.len(), 2, "{v}");
    assert_eq!(ids[0], "TITLE-1", "title match must outrank body match: {v}");
    let ranks: Vec<f64> = v["hits"].as_array().unwrap().iter().map(|h| h["rank"].as_f64().unwrap()).collect();
    assert!(ranks[0] < ranks[1], "bm25 rank is smaller-is-better: {ranks:?}");
}

#[tokio::test]
async fn type_filter_restricts_and_is_reported() {
    let (app, store, _d) = app();
    card(&store, "T-1", "wombat card", "x", None);
    store
        .write(|conn| {
            conn.execute(
                "INSERT INTO schedules (id, title, session, command, created, updated)
                 VALUES ('SCHED-1', 'wombat schedule', 'lane-a', 'echo wombat', 1785000000, 1785000000)",
                [],
            )?;
            Ok(amux_server::db::WriteOutcome { applied: false, events: vec![] })
        })
        .unwrap();

    let (_, all) = get(&app, "/api/search?q=wombat").await;
    assert_eq!(all["total"], 2, "{all}");
    let (_, only) = get(&app, "/api/search?q=wombat&types=schedule").await;
    assert_eq!(hit_ids(&only), vec!["SCHED-1"], "{only}");
    assert_eq!(only["types"][0], "schedule");
}

#[tokio::test]
async fn a_query_that_is_fts_syntax_is_treated_as_text_not_as_an_error() {
    let (app, store, _d) = app();
    card(&store, "T-1", "notes about a:b", "and NOT much else", None);
    // Each of these is a syntax error or an unintended operator if passed to
    // FTS5 raw. A 500 here is the bug this guards.
    for q in ["a%3Ab", "NOT", "%22unbalanced", "*", "%28paren"] {
        let (st, v) = get(&app, &format!("/api/search?q={q}")).await;
        assert_eq!(st, StatusCode::OK, "query {q:?} must not error: {v}");
        assert!(v["error"].is_null(), "query {q:?}: {v}");
    }
}

#[tokio::test]
async fn snippets_cannot_inject_markup() {
    let (app, store, _d) = app();
    card(&store, "T-1", "xss probe", "<script>alert('pwned')</script> containing echidna", None);
    let (_, v) = get(&app, "/api/search?q=echidna").await;
    let snip = v["hits"][0]["snippet"].as_str().unwrap();
    assert!(snip.contains("<mark>echidna</mark>"), "{snip}");
    assert!(!snip.contains("<script>"), "raw markup must not survive into the snippet: {snip}");
    assert!(snip.contains("&lt;script&gt;"), "{snip}");
}

#[tokio::test]
async fn an_empty_query_says_so_instead_of_matching_everything() {
    let (app, store, _d) = app();
    card(&store, "T-1", "anything", "at all", None);
    let (st, v) = get(&app, "/api/search?q=").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["total"], 0, "an empty query must not return the corpus: {v}");
    assert!(v["note"].as_str().unwrap().contains("empty query"), "{v}");
}

#[tokio::test]
async fn status_reports_drift_and_reindex_repairs_it() {
    let (app, store, _d) = app();
    card(&store, "T-1", "indexed card", "x", None);
    // Simulate the failure this instrument exists for: docs removed without
    // their source rows. Without a status view, the only symptom is a search
    // that returns nothing — indistinguishable from "no matches".
    store
        .write(|conn| {
            conn.execute("DELETE FROM search_docs", [])?;
            Ok(amux_server::db::WriteOutcome { applied: false, events: vec![] })
        })
        .unwrap();
    let (_, st) = get(&app, "/api/search/status").await;
    assert!(!st["consistent"].as_bool().unwrap(), "drift must be visible: {st}");

    let req = Request::builder()
        .method("POST")
        .uri("/api/search/reindex")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let report: Value = serde_json::from_slice(&bytes).unwrap();
    // The rebuild reports its counts — that is the backfill's report.
    assert!(report["before"]["consistent"] == false, "{report}");
    assert!(report["after"]["consistent"] == true, "{report}");
    let task = report["per_family"].as_array().unwrap().iter().find(|f| f["type"] == "task").cloned().unwrap();
    assert_eq!(task["indexed"], 1, "{report}");

    let (_, v) = get(&app, "/api/search?q=indexed").await;
    assert_eq!(hit_ids(&v), vec!["T-1"]);
}

#[tokio::test]
async fn memories_messages_and_workers_are_indexed_too() {
    let (app, store, _d) = app();
    store
        .write(|conn| {
            conn.execute(
                "INSERT INTO _amux_memories (id, scope, name, content, memory_type, created_at, updated_at, provenance)
                 VALUES ('mem_1', '{\"level\":\"global\"}', 'pangolin-note', 'remember the pangolin', 'user',
                         '2026-08-09T00:00:00+00:00', '2026-08-09T00:00:00+00:00', '{}')",
                [],
            )?;
            conn.execute(
                "INSERT INTO _amux_messages (id, from_actor, target, body, created_at, delivery)
                 VALUES ('msg_1', '{\"id\":\"lane-a\"}', '{\"id\":\"lane-b\"}', 'ping about the pangolin',
                         '2026-08-09T00:00:00+00:00', '{}')",
                [],
            )?;
            conn.execute(
                "INSERT INTO _amux_workers (id, display_name, cwd, provider, backend, created_at, updated_at)
                 VALUES ('wrk_1', 'pangolin-lane', '/tmp', 'claude', 'tmux',
                         '2026-08-09T00:00:00+00:00', '2026-08-09T00:00:00+00:00')",
                [],
            )?;
            Ok(amux_server::db::WriteOutcome { applied: false, events: vec![] })
        })
        .unwrap();

    let (_, v) = get(&app, "/api/search?q=pangolin").await;
    let mut types: Vec<&str> = v["hits"].as_array().unwrap().iter().map(|h| h["type"].as_str().unwrap()).collect();
    types.sort_unstable();
    assert_eq!(types, vec!["memory", "message", "worker"], "{v}");
    // Provenance chips the plan asks a SearchHit to carry.
    for h in v["hits"].as_array().unwrap() {
        assert!(h["link"].as_str().map(|l| !l.is_empty()).unwrap_or(false), "hit needs a link target: {h}");
        assert!(h["updated_at"].as_i64().unwrap() > 0, "hit needs a timestamp: {h}");
    }
    let (_, st) = get(&app, "/api/search/status").await;
    assert!(st["consistent"].as_bool().unwrap(), "{st}");
}

#[tokio::test]
async fn archived_cards_stay_searchable_and_carry_the_flag() {
    // Ethos rule 1's archived-filter trap: an index that quietly drops
    // archived cards makes the majority of the board unfindable, and the
    // symptom is a plausible-looking empty result.
    let (app, store, _d) = app();
    card(&store, "T-1", "archived aardvark", "x", None);
    store
        .write(|conn| {
            conn.execute("UPDATE issues SET archived = 1 WHERE id = 'T-1'", [])?;
            Ok(amux_server::db::WriteOutcome { applied: false, events: vec![] })
        })
        .unwrap();
    let (_, v) = get(&app, "/api/search?q=aardvark").await;
    assert_eq!(hit_ids(&v), vec!["T-1"], "archived cards must remain findable: {v}");
    assert_eq!(v["hits"][0]["meta"]["archived"], 1, "the flag must ride along so a client can filter: {v}");
}
