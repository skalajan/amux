//! ROUTE_TABLE honesty test (AMUX-2610).
//!
//! `request_log::ROUTE_TABLE` is the hand-maintained routing truth behind
//! `GET /api/debug/routes` and the 404/405 annotations + verdicts in
//! `GET /api/logs/analyze`. axum's Router cannot enumerate its routes, so
//! the table can only stay honest by being WALKED against the real
//! composition (`api::router`), and the walk must be able to fail BOTH
//! directions:
//!
//! - **Claimed but not routed**: an OPTIONS probe at a table path that axum
//!   does not route lands in the SPA catch-all — a nested miss answers 404,
//!   a top-level miss answers the catch-all's 405 whose `Allow` is exactly
//!   `GET(,HEAD)`. Concrete entries assert 405 with `Allow` EQUAL (as a
//!   set, HEAD/OPTIONS aside) to the table's methods, so a bogus path fails
//!   either on status or on the Allow set; the one ambiguous case — a
//!   GET-only entry, whose Allow matches the GET-only catch-all's — is
//!   disambiguated by firing the GET and rejecting the catch-all's
//!   BYTE-EXACT 404 body (`{"error": "not found"}` WITH the space —
//!   hand-written only in static_files.rs; every module body is serde-
//!   serialized without it).
//! - **Routed but not claimed** (an under-listed method): the `Allow` set
//!   equality catches it, and the negative twin — a method the table does
//!   NOT list — is fired and must answer 405. If the router actually
//!   mounts that method, the handler answers something else and the walk
//!   fails, forcing the table to list it.
//!
//! Both directions were demonstrated against deliberately-wrong entries
//! before this landed (a nonexistent path, and a GET-only claim on
//! /api/board which also routes POST); see AMUX-2610.
//!
//! `["*"]` (any()) entries invoke their handler on OPTIONS, so for them the
//! walk asserts the answer is NOT the catch-all's signature (405 with
//! Allow ⊆ {GET, HEAD}, or an HTML 404).

use amux_server::api::request_log::ROUTE_TABLE;
use amux_server::api::{router, AppState};
use amux_server::db::Store;
use axum::body::Body;
use axum::http::Request;
use std::collections::BTreeSet;
use tower::ServiceExt;

/// The SPA catch-all's 404 body for /api paths, byte-exact (the SPACE is the
/// discriminator — see module doc).
const STATIC_404_BODY: &str = "{\"error\": \"not found\"}";

fn concretize(pattern: &str) -> String {
    pattern
        .split('/')
        .map(|seg| {
            if seg.starts_with("{*") {
                "zz-probe"
            } else if seg.starts_with('{') {
                "zz-probe-1"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn allow_set(res: &axum::response::Response) -> BTreeSet<String> {
    res.headers()
        .get("allow")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(',')
        .map(|m| m.trim().to_uppercase())
        .filter(|m| !m.is_empty() && m != "HEAD" && m != "OPTIONS")
        .collect()
}

async fn fire(app: &axum::Router, method: &str, path: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// One test fn on purpose: it mutates process env (AMUX_PY_URL, AMUX_HOME)
/// and tests within a binary share the process (same shape as
/// proxy_composition.rs).
#[tokio::test]
async fn route_table_matches_the_real_router_both_directions() {
    // Dead python: /api/scope's any() probe must answer a deterministic 502,
    // never touch a real server.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}", l.local_addr().unwrap());
    drop(l);
    std::env::set_var("AMUX_PY_URL", &dead);
    // Hermetic fleet home: probes on sessions/groups/files must not read the
    // developer's real ~/.amux.
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("sessions")).unwrap();
    std::env::set_var("AMUX_HOME", home.path());

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db")).unwrap();
    let app = router(AppState {
        store: std::sync::Arc::new(store),
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: None,
    });

    let twin_candidates = ["DELETE", "PUT", "PATCH", "POST", "GET"];
    for entry in ROUTE_TABLE {
        let path = concretize(entry.path);
        let res = fire(&app, "OPTIONS", &path).await;
        let status = res.status().as_u16();

        if entry.methods.contains(&"*") {
            // any(): OPTIONS reaches the handler. The only failure shape is
            // the catch-all answering instead of a route.
            let allow = allow_set(&res);
            let catchall_405 =
                status == 405 && !allow.is_empty() && allow.iter().all(|m| m == "GET");
            assert!(
                !catchall_405,
                "{}: claimed any() but OPTIONS hit the GET-only SPA catch-all (405, allow={allow:?})",
                entry.path
            );
            let ct = res
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert!(
                !(status == 404 && ct.starts_with("text/html")),
                "{}: claimed any() but answered the SPA shell",
                entry.path
            );
            continue;
        }

        // Concrete methods: the route's MethodRouter must answer the OPTIONS
        // probe with 405 + an Allow set equal to the table's claim.
        assert_eq!(
            status, 405,
            "{}: not routed where the table claims (OPTIONS answered {status}, not the \
             method-router's 405)",
            entry.path
        );
        let allow = allow_set(&res);
        let claimed: BTreeSet<String> =
            entry.methods.iter().map(|m| m.to_uppercase()).collect();
        assert_eq!(
            allow, claimed,
            "{}: table methods disagree with the router's Allow set",
            entry.path
        );

        // GET-only ambiguity: the catch-all is itself a GET route at every
        // path, so Allow == {GET} does not prove THIS route exists. The GET
        // answer does: the catch-all's /api 404 body is byte-exact.
        if claimed.len() == 1 && claimed.contains("GET") {
            let res = fire(&app, "GET", &path).await;
            let status = res.status().as_u16();
            // Read the body ONLY on 404: a non-404 already proves a real
            // route answered, and some GET routes stream forever
            // (/api/events is SSE — to_bytes on it never returns).
            if status == 404 {
                let body =
                    axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
                let body = String::from_utf8_lossy(&body);
                assert!(
                    body != STATIC_404_BODY,
                    "{}: GET answered the SPA catch-all's 404 — the table claims a \
                     route that does not exist",
                    entry.path
                );
            }
        }

        // Negative twin: a method the table does NOT list must 405 (an
        // under-listed method would reach a handler and answer otherwise).
        let twin = twin_candidates
            .iter()
            .find(|m| !claimed.contains(**m))
            .expect("a method outside the claimed set always exists");
        let res = fire(&app, twin, &path).await;
        assert_eq!(
            res.status().as_u16(),
            405,
            "{}: unlisted method {twin} did not 405 — the router mounts more than the \
             table lists",
            entry.path
        );
    }

    std::env::remove_var("AMUX_PY_URL");
    std::env::remove_var("AMUX_HOME");
}

/// THE OTHER DIRECTION: a route MOUNTED but absent from the table.
///
/// The module comment above claimed ROUTE_TABLE was "kept honest BOTH
/// directions by tests/route_table.rs". It was not — the test above iterates
/// `for entry in ROUTE_TABLE`, so it can only catch a table row with no route.
/// A route with no table row is invisible to it, and one sat that way:
/// `/api/log-search` was mounted and answering while the route census (which
/// reads the TABLE) reported it as "no route matches this path" and someone
/// nearly ported it a second time.
///
/// Only DIRECT `.route("/api/...")` calls in api/mod.rs are checked here.
/// Nested families (`.nest("/api/x", x::routes())`) declare their sub-paths
/// inside the module and are not visible to a source scan of mod.rs — stating
/// that plainly rather than implying full coverage, which is the overclaim this
/// test exists to correct.
/// Nested routes known to be missing from ROUTE_TABLE (AMUX-2937).
///
/// These answer for real — probed live, they return JSON from their handlers,
/// not the SPA catch-all's HTML — while `/api/debug/routes` and the
/// `route.callers_have_routes` census both read the TABLE and so report them as
/// unrouted. They are listed rather than fixed in the same pass because a
/// ROUTE_TABLE row carries METHODS, and a row with the WRONG methods is worse
/// than a missing one: it manufactures false 405 verdicts in the very census
/// this table feeds. Extracting the verbs mechanically resolved 6 of 17
/// reliably, so the rest need reading by hand.
///
/// The list is checked in BOTH directions below, so it cannot go stale: adding
/// a row to ROUTE_TABLE without removing it here fails too.
const KNOWN_UNTABLED: &[&str] = &[];

#[test]
fn every_directly_routed_api_path_is_in_the_table() {
    let src = include_str!("../src/api/mod.rs");
    let tabled: std::collections::HashSet<&str> =
        ROUTE_TABLE.iter().map(|e| e.path).collect();
    // SCANS THE WHOLE SOURCE, NOT LINE BY LINE. The first version of this test
    // read one line at a time and so missed every multi-line declaration —
    //     .route(
    //         "/api/memory/global",
    //         axum::routing::get(...),
    //     )
    // which is the shape rustfmt produces for anything with two handlers. It
    // passed while /api/memory/global, /api/review/* and /api/channels sat
    // mounted-and-untabled, i.e. it had exactly the blind spot it was written
    // to close, one formatting style over.
    let mut missing = Vec::new();
    let mut rest = src;
    while let Some(i) = rest.find(".route(") {
        rest = &rest[i + ".route(".len()..];
        // Skip whitespace/newlines between `.route(` and the path literal.
        let after = rest.trim_start();
        let Some(stripped) = after.strip_prefix('"') else { continue };
        let Some(end) = stripped.find('"') else { continue };
        let path = &stripped[..end];
        if path.starts_with("/api/") && !tabled.contains(path) {
            missing.push(path.to_string());
        }
    }
    // AND FOLLOW `.nest()` INTO THE SUB-ROUTER. The scan above only sees
    // `.route()` calls written in api/mod.rs itself, so an entire family
    // mounted as `.nest("/api/crm", crm::routes())` was invisible: its routes
    // live in crm.rs. That is the same blind spot as the multi-line one above,
    // one level up — and it cost exactly what the docstring predicts. The CRM
    // port (AMUX-2929) shipped five routes that answered 200 while
    // `/api/debug/routes` and `route.callers_have_routes` both called them
    // unrouted, and this test stayed green through all of it.
    //
    // Found by the CLI caller scan (AMUX-2917) reporting `POST /api/crm/contacts`
    // as having no route — a NEW instrument catching the gap an older one was
    // structurally unable to see.
    let api_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
    let mut rest = src;
    while let Some(i) = rest.find(".nest(") {
        rest = &rest[i + ".nest(".len()..];
        let after = rest.trim_start();
        let Some(stripped) = after.strip_prefix('"') else { continue };
        let Some(end) = stripped.find('"') else { continue };
        let prefix = &stripped[..end];
        if !prefix.starts_with("/api/") {
            continue;
        }
        // `.nest("/api/crm", crm::routes())` and
        // `.nest("/api/push", crate::push::routes())` — take the LAST module
        // segment before `::fn()`, so a `crate::` prefix does not become a
        // module named "crate" (it did, and reported an unreadable crate.rs).
        let tail = &stripped[end..];
        let Some(comma) = tail.find(',') else { continue };
        let Some(open) = tail[comma..].find('(') else { continue };
        let callee: String = tail[comma + 1..comma + open].trim().to_string();
        let segs: Vec<&str> = callee.split("::").map(str::trim).collect();
        if segs.len() < 2 {
            continue;
        }
        let func = segs[segs.len() - 1];
        let module = segs[segs.len() - 2];
        if module.is_empty() || module == "crate" {
            continue;
        }
        // `push` is a DIRECTORY module (src/push/mod.rs), so try both layouts
        // before declaring it unreadable.
        let msrc = std::fs::read_to_string(api_dir.join(format!("{module}.rs")))
            .or_else(|_| std::fs::read_to_string(api_dir.join(module).join("mod.rs")))
            .or_else(|_| {
                std::fs::read_to_string(
                    api_dir.parent().unwrap().join(module).join("mod.rs"),
                )
            });
        let Ok(msrc) = msrc else {
            // A module we cannot read is NOT a pass — a silently skipped nest
            // is the failure this block exists to fix.
            missing.push(format!("<unreadable {module}.rs for nest {prefix}>"));
            continue;
        };
        // ONLY THE NAMED FUNCTION'S BODY. Scanning the whole file was the first
        // version and it was wrong in two ways at once: a module may define
        // SEVERAL routers (groups.rs has routes() and tags_routes()), and any
        // `.route("/api/...")` belonging to a non-nested one got the nest
        // prefix glued on — producing "/api/cal-events/api/calendar.ics" and
        // "/api/logs/api/board", paths that exist nowhere. A check that invents
        // paths is worse than the blindness it replaced.
        let Some(fstart) = msrc.find(&format!("fn {func}(")) else { continue };
        let Some(brace) = msrc[fstart..].find('{') else { continue };
        let mut depth = 0i32;
        let mut fend = fstart + brace;
        for (k, c) in msrc[fstart + brace..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        fend = fstart + brace + k;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &msrc[fstart + brace..fend];
        let mut mrest = body;
        while let Some(j) = mrest.find(".route(") {
            mrest = &mrest[j + ".route(".len()..];
            let a = mrest.trim_start();
            let Some(st) = a.strip_prefix('"') else { continue };
            let Some(e) = st.find('"') else { continue };
            let sub = &st[..e];
            // A nested router's paths are RELATIVE. An absolute /api/ literal
            // here means the scan wandered outside the nested router, so
            // gluing the prefix on would fabricate a path.
            if sub.starts_with("/api/") {
                continue;
            }
            // HONOUR THE TABLE'S OWN DOCUMENTED EXCLUSION. request_log.rs says
            // plainly what is deliberately NOT a row: "module-internal
            // catch-alls whose only job is answering a JSON 404 … they are 'no
            // such route' answerers, not capabilities", and it names
            // /api/fs/{*rest}, /api/browser/{*rest} and four more.
            //
            // My first version reported all six as gaps, which is the same
            // mistake I made filing AMUX-2917 against /api/stripe: read a
            // missing row, call it an omission, never read the policy sitting
            // above the list. A test that contradicts the documented design of
            // the thing it checks trains people to allowlist their way past it.
            //
            // Detected from the HANDLER, not the path shape, because the two
            // diverge: /api/groups/{*rest} is a real capability and IS tabled,
            // while /api/fs/{*rest} answers 404. Whatever the policy is, the
            // handler is where it is true.
            let handler_end = st[e..].find(')').map(|k| e + k).unwrap_or(st.len());
            let handler = &st[e..handler_end];
            // Matched on the NAMING CONVENTION, not one spelling: browser.rs
            // calls its answerer `catalog_404`, fs.rs calls it `not_found`,
            // dictation.rs inlines `route_not_found()`. A check keyed to a
            // single name passes on the module that happens to use it and
            // silently reports the others as gaps — which is exactly what the
            // first version did.
            if handler.contains("not_found") || handler.contains("_404") {
                continue;
            }
            let full =
                if sub == "/" { prefix.to_string() } else { format!("{}{}", prefix, sub) };
            if !tabled.contains(full.as_str()) {
                missing.push(full);
            }
        }
    }

    // AND FOLLOW `.merge()` THE SAME WAY. A merged router carries ABSOLUTE
    // paths (merge adds no prefix), which is exactly why modules use it — the
    // public gmail callback must not sit under a nest wildcard. But this
    // scanner only followed `.nest()`, so a whole merged family was invisible:
    // the gmail mailbox port (AMUX-2883) shipped four routes and this test
    // stayed green with none of them tabled — found only because the graph
    // port a day earlier used `.nest` and DID trip it, and the asymmetry
    // begged the question. gmail_auth's rows exist because someone added them
    // by hand, which is the discipline this test exists to replace.
    let mut rest = src;
    while let Some(i) = rest.find(".merge(") {
        rest = &rest[i + ".merge(".len()..];
        let Some(close) = rest.find(')') else { continue };
        let callee: String = rest[..close].trim().to_string();
        let segs: Vec<&str> = callee.split("::").map(str::trim).collect();
        if segs.len() < 2 {
            continue;
        }
        let module = segs[segs.len() - 2];
        if module.is_empty() || module == "crate" {
            continue;
        }
        // Resolve the FULL module path — `crate::runtime_jobs::autofix::routes()`
        // lives at src/runtime_jobs/autofix.rs, and the first cut of this
        // block looked only under src/api/, reporting four readable modules
        // as unreadable.
        let mod_segs: Vec<&str> =
            segs[..segs.len() - 1].iter().copied().filter(|s| *s != "crate").collect();
        let src_dir = api_dir.parent().unwrap();
        let joined = mod_segs.join("/");
        let msrc = std::fs::read_to_string(api_dir.join(format!("{module}.rs")))
            .or_else(|_| std::fs::read_to_string(api_dir.join(module).join("mod.rs")))
            .or_else(|_| std::fs::read_to_string(src_dir.join(format!("{joined}.rs"))))
            .or_else(|_| std::fs::read_to_string(src_dir.join(&joined).join("mod.rs")));
        let Ok(msrc) = msrc else {
            missing.push(format!("<unreadable {module}.rs for merge {callee}>"));
            continue;
        };
        // The WHOLE FILE, not the named fn's body — deliberately different
        // from the nest scanner above. Merged paths carry no prefix, so an
        // absolute `/api/` literal anywhere in a merged module is a real
        // route, and fn-body scoping is what let this family hide:
        // `routes()` delegating to `routes_with(ctx)` has no `.route()` calls
        // of its own, which is exactly the gmail_auth/gmail shape.
        let mut mrest = msrc.as_str();
        while let Some(j) = mrest.find(".route(") {
            mrest = &mrest[j + ".route(".len()..];
            let a = mrest.trim_start();
            let Some(st) = a.strip_prefix('"') else { continue };
            let Some(e) = st.find('"') else { continue };
            let sub = &st[..e];
            // Merged paths are absolute-as-written; anything not under /api/
            // is not this table's business.
            if !sub.starts_with("/api/") {
                continue;
            }
            let handler_end = st[e..].find(')').map(|k| e + k).unwrap_or(st.len());
            let handler = &st[e..handler_end];
            if handler.contains("not_found") || handler.contains("_404") {
                continue;
            }
            if !tabled.contains(sub) {
                missing.push(sub.to_string());
            }
        }
    }

    missing.sort();
    missing.dedup();

    // The mirror, so the allowlist cannot rot: a path that has since been
    // tabled must be dropped from KNOWN_UNTABLED, or the list slowly becomes a
    // place where real gaps hide.
    let stale: Vec<&str> =
        KNOWN_UNTABLED.iter().copied().filter(|k| !missing.iter().any(|m| m == k)).collect();
    assert!(
        stale.is_empty(),
        "KNOWN_UNTABLED lists paths that are now tabled (or gone) — delete them: {stale:?}"
    );

    let missing: Vec<String> =
        missing.into_iter().filter(|m| !KNOWN_UNTABLED.contains(&m.as_str())).collect();
    assert!(
        missing.is_empty(),
        "mounted in api/mod.rs but absent from ROUTE_TABLE — the route census reads \
         the TABLE, so these are reported as unrouted while they answer fine: {missing:?}"
    );
}
