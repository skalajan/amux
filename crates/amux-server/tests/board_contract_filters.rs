//! `/api/board/contract` must document the list filters that actually exist,
//! and must not document ones that do not (AMUX-2933, reported by ts-gke).
//!
//! The filters worked and were documented NOWHERE — "discoverable only by
//! guessing", in the report's words. The cap was worse than undocumented, it
//! was silent: a lane auditing its own board got the 100 most-recently-updated
//! TERMINAL rows fleet-wide with nothing in the body saying so, which is how
//! `GET /api/board` came to return FEWER of a lane's done cards (4) than
//! `?session=<lane>` did (102). A list that drops rows with no signal reads as
//! data, not as truncation — and ts-gke was one step from reporting "only 4
//! done cards exist".
//!
//! So this test exists to stop the DOC drifting from the CODE, which is the
//! failure that would put the silence back. It compares the documented filter
//! names against `ListParams`' real fields, in BOTH directions.

use amux_server::api::{router, AppState};
use amux_server::db::Store;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

async fn contract() -> Value {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("contract-test.db")).unwrap();
    let state = AppState {
        store: std::sync::Arc::new(store),
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: None,
    };
    let app = router(state);
    let res = app
        .oneshot(Request::builder().uri("/api/board/contract").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// The names ListParams actually accepts, read off the struct's source rather
/// than restated here — a hand-copied list is the thing that drifts.
fn list_params_fields() -> Vec<String> {
    let src = include_str!("../src/api/board.rs");
    // Start AFTER the declaration line: `pub struct ListParams {` itself
    // matches the `pub ` prefix below and was scraped as a field named
    // "struct ListParams {" on the first run.
    let decl = "pub struct ListParams {";
    let start = src.find(decl).expect("ListParams exists") + decl.len();
    let end = start + src[start..].find("\n}\n").expect("struct closes");
    src[start..end]
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            t.strip_prefix("pub ")
                .and_then(|r| r.split(':').next())
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
        })
        .collect()
}

#[tokio::test]
async fn the_contract_documents_every_real_list_filter() {
    let c = contract().await;
    let list = c.get("list").expect("contract documents the list endpoint");
    let filters = list["filters"].as_object().expect("filters object");
    let refused = list["not_a_filter"].as_object().expect("refused params named too");

    let documented: Vec<String> = filters
        .keys()
        .cloned()
        .chain(refused.keys().flat_map(|k| {
            // "q / query / search" documents three names in one key.
            k.split('/').map(|s| s.trim().to_string()).collect::<Vec<_>>()
        }))
        .collect();

    let real = list_params_fields();
    assert!(real.len() >= 7, "the ListParams scraper is broken, found {real:?}");

    for f in &real {
        assert!(
            documented.iter().any(|d| d == f),
            "ListParams accepts `{f}` but /api/board/contract does not document it — \
             that is the AMUX-2933 defect (filters discoverable only by guessing). \
             documented: {documented:?}"
        );
    }
    // The mirror: nothing documented that does not exist, or callers write
    // queries that are silently ignored — which is exactly how `?q=` used to
    // return the whole board.
    for d in &documented {
        assert!(
            real.iter().any(|r| r == d),
            "contract documents `{d}` but ListParams has no such field — a param axum will \
             silently drop. real: {real:?}"
        );
    }
}

/// The cap is legitimate; the SILENCE was the bug. The contract has to state
/// the default, that scoping lifts it, and how to detect a truncated read —
/// otherwise the next lane repeats ts-gke's measurement from scratch.
#[tokio::test]
async fn the_contract_explains_the_terminal_cap_and_how_to_see_it() {
    let c = contract().await;
    let cap = &c["list"]["terminal_cap"];
    assert_eq!(cap["default_unscoped"], 100, "the unscoped default cap must be stated");
    assert_eq!(cap["default_scoped"], 0, "a scoped query is uncapped — that is the fix ts-gke got");

    for key in ["why", "detect_truncation", "to_get_everything", "auditing_your_own_cards"] {
        let v = cap[key].as_str().unwrap_or("");
        assert!(!v.is_empty(), "terminal_cap.{key} must be documented, got {v:?}");
    }
    // The headers named in the contract must be the ones list_board actually
    // sets — naming a header that does not exist is the same class of lie.
    let detect = cap["detect_truncation"].as_str().unwrap();
    let src = include_str!("../src/api/board.rs");
    for h in ["x-amux-truncated", "x-amux-terminal-total", "x-amux-terminal-returned"] {
        assert!(detect.contains(h), "contract must name {h}");
        assert!(src.contains(h), "list_board must actually set {h}");
    }
}
