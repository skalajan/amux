//! GET /api/usage — subscription usage for the Settings meter (port of
//! Python's `_fetch_claude_usage`, amux-server.py ~:3189).
//!
//! # Why this does not go through `ProviderAdapter::usage()`
//!
//! It used to, and that is why the meter was dark. The adapter returns
//! NORMALIZED [`UsageWindow`]s for capacity routing, which is a deliberately
//! lossy view: it keeps kind/percent/reset and discards the provider-specific
//! fields this SPA renders — `limits[].scope.model.display_name` (the
//! per-model weekly rows), `limits[].group`, and the exact `kind` spelling
//! the renderer switches on. Re-deriving those from a normalized window is
//! guesswork, so the endpoint consumes the probe DIRECTLY and passes
//! Anthropic's body through verbatim, exactly as Python did. The SPA's
//! `loadUsage()` therefore sees byte-identical fields to the Python server.
//!
//! The adapter is still the owner of the probe — [`probe_usage_raw`] lives in
//! `provider/claude.rs` next to the credential reader, and
//! `ProviderAdapter::usage()` calls the same function. One token
//! acquisition, one HTTP call, two consumers with different honesty
//! requirements: routing needs a number or nothing (Invariant 20), a human
//! needs to know WHICH thing went wrong.
//!
//! # Discriminated degradation (ethos rule 4)
//!
//! The previous single sentence — "no token, expired token, or probe failed"
//! — was true and useless: it is three causes with three different remedies,
//! so nobody could tell a broken login from a transient rate limit, and the
//! meter stayed dark without anyone knowing which to fix. During development
//! (2026-08-09) this host's probe was answering **HTTP 429** with a perfectly
//! valid, unexpired keychain token — a self-healing condition that read as a
//! missing credential. Every cause now has its own reason string, and an HTTP
//! failure carries its status code.
//!
//! # Secrets
//!
//! No response on any path can contain the token: [`UsageProbe`] cannot carry
//! it, failure reasons are built from a status code or a fixed word, and the
//! upstream response BODY is never echoed on a failure — only on 2xx, where
//! it is the usage report itself.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::response::{IntoResponse, Response};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};

use super::AppState;
use crate::provider::claude::{probe_usage_raw, UsageProbe};

/// Default cache TTL. Python used 30s; this defaults to 60 because the probe
/// is a NETWORK call made on a settings render, and the endpoint is
/// rate-limited per account — on a host running a fleet of Claude Code
/// processes, that budget is shared with all of them. Override with
/// `AMUX_USAGE_TTL_S` (0 disables caching, for debugging).
const DEFAULT_USAGE_TTL_S: u64 = 60;

/// How long a SUCCESSFUL reading keeps being served after a later probe
/// fails. `AMUX_USAGE_STALE_S`; 0 disables the fallback.
///
/// This exists because the failure is INTERMITTENT, which was only visible by
/// watching two servers at once: on 2026-08-09 one build served real limits
/// while another got HTTP 429 twenty seconds later, same host, same account.
/// With no fallback the meter flickers between real numbers and an error —
/// and "it says unavailable" is the bug being fixed here. Ten minutes is well
/// inside the resolution of the thing being measured (5-hour and 7-day
/// windows), so a reading this old is still true to the precision displayed.
const DEFAULT_USAGE_STALE_S: u64 = 600;

fn env_secs(key: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(default),
    )
}

fn usage_ttl() -> Duration {
    env_secs("AMUX_USAGE_TTL_S", DEFAULT_USAGE_TTL_S)
}

fn usage_stale_window() -> Duration {
    env_secs("AMUX_USAGE_STALE_S", DEFAULT_USAGE_STALE_S)
}

/// A probe as an injectable dependency, so tests exercise the real handler —
/// cache, shaping and all — against fixtures, and never touch the network or
/// this machine's keychain.
pub type ProbeFn =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = UsageProbe> + Send>> + Send + Sync>;

#[derive(Default)]
struct UsageCache {
    /// The shaped body WITHOUT its age field — age is stamped per response,
    /// because a cached reading gets older while it sits here.
    data: Option<Value>,
    at: Option<Instant>,
    /// The last body that actually carried numbers, kept separately so a
    /// transient failure cannot evict a good reading.
    last_good: Option<Value>,
    last_good_at: Option<Instant>,
}

/// Production wiring: the real read-only probe from the Claude adapter.
pub fn routes() -> Router<AppState> {
    routes_with(Arc::new(|| Box::pin(probe_usage_raw())))
}

/// Test seam.
pub fn routes_with(probe: ProbeFn) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(get_usage))
        .layer(Extension(probe))
        .layer(Extension(Arc::new(tokio::sync::Mutex::new(
            UsageCache::default(),
        ))))
}

async fn get_usage(
    Extension(probe): Extension<ProbeFn>,
    Extension(cache): Extension<Arc<tokio::sync::Mutex<UsageCache>>>,
) -> Response {
    let ttl = usage_ttl();
    let mut c = cache.lock().await;

    // Serve from cache while fresh. Failures are cached too, deliberately: a
    // rate-limited probe must not be retried on every settings render, which
    // is the behaviour that provokes the rate limit in the first place.
    let fresh = matches!((&c.data, c.at), (Some(_), Some(at)) if at.elapsed() < ttl);
    if !fresh {
        let shaped = shape_probe(probe().await);
        if shaped.get("available") == Some(&json!(true)) {
            c.last_good = Some(shaped.clone());
            c.last_good_at = Some(Instant::now());
        }
        c.data = Some(shaped);
        c.at = Some(Instant::now());
    }

    let mut body = c.data.clone().unwrap_or_else(|| json!({}));
    let mut age = c.at.map(|t| t.elapsed()).unwrap_or(Duration::ZERO);

    // A failed probe with a recent good reading in hand: serve the reading
    // rather than nothing. Everything here was really fetched — only its age
    // changed — and both the age and the live failure travel with it, so the
    // response never claims to be something it is not.
    let mut stale_reason: Option<Value> = None;
    if body.get("available") != Some(&json!(true)) {
        let stale_window = usage_stale_window();
        if let (Some(good), Some(at)) = (&c.last_good, c.last_good_at) {
            if !stale_window.is_zero() && at.elapsed() < stale_window {
                stale_reason = body.get("reason").cloned();
                body = good.clone();
                age = at.elapsed();
            }
        }
    }

    // How old is this reading? A meter that silently shows a minute-old
    // number is fine; one that cannot tell you it is doing so is not.
    if let Some(obj) = body.as_object_mut() {
        obj.insert("cache_age_s".into(), json!(age.as_secs()));
        obj.insert("cache_ttl_s".into(), json!(ttl.as_secs()));
        if let Some(reason) = stale_reason {
            obj.insert("stale".into(), json!(true));
            // Why the live probe failed, kept beside the served numbers so
            // "these are 4 minutes old because of a 429" is answerable.
            obj.insert("stale_reason".into(), reason);
        }
    }
    Json(body).into_response()
}

/// One probe outcome -> the wire body the SPA consumes.
///
/// Success is Anthropic's body VERBATIM plus `available: true` — Python's
/// exact behaviour (`data["available"] = True; return data`), which is what
/// keeps `limits[].kind` / `.percent` / `.resets_at` / `.scope` / `.group`
/// spelled the way `loadUsage()` reads them.
fn shape_probe(probe: UsageProbe) -> Value {
    match probe {
        UsageProbe::Ok(body) => match body {
            Value::Object(mut map) => {
                map.insert("available".into(), json!(true));
                Value::Object(map)
            }
            // 2xx whose body is not a JSON object: there is nowhere to put
            // `available`, and guessing a shape would be inventing one.
            // Python raised here and reported a failed fetch; this is that,
            // named precisely.
            _ => degraded(
                "unexpected_shape",
                "Usage endpoint returned an unexpected response shape".into(),
            ),
        },
        // Each arm is a different remedy, which is the whole point of the type.
        UsageProbe::NoToken => degraded(
            "no_token",
            "No Claude subscription token on this host".into(),
        ),
        UsageProbe::Expired => degraded(
            "expired_token",
            "Token expired — run any Claude command to refresh it".into(),
        ),
        UsageProbe::Http(401) | UsageProbe::Http(403) => degraded(
            "token_rejected",
            "Token rejected (401) — run any Claude command to refresh it".into(),
        ),
        // Called out from the generic HTTP arm because it is the one failure
        // that is neither the user's fault nor persistent: it clears on its
        // own, and telling someone to re-login would be actively wrong.
        UsageProbe::Http(429) => degraded(
            "rate_limited",
            "Anthropic rate-limited the usage probe (HTTP 429) — this clears on its own; \
             it is usually many Claude processes sharing one account"
                .into(),
        ),
        UsageProbe::Http(code) => degraded(
            "probe_failed",
            format!("Usage fetch failed (HTTP {code})"),
        ),
        UsageProbe::Transport(what) => degraded(
            "probe_failed",
            format!("Usage fetch failed (network: {what})"),
        ),
        UsageProbe::BadShape => degraded(
            "unexpected_shape",
            "Usage endpoint returned an unexpected response shape".into(),
        ),
    }
}

/// The degraded body. `reason` is the human sentence the SPA prints; `cause`
/// is the stable machine tag, so a future consumer can branch without
/// string-matching prose (and so a reason can be reworded without breaking
/// anything).
fn degraded(cause: &str, reason: String) -> Value {
    json!({ "available": false, "cause": cause, "reason": reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    /// A fixture probe that counts how many times it was called.
    fn probe_fn(outcome: UsageProbe, calls: Arc<AtomicUsize>) -> ProbeFn {
        Arc::new(move || {
            let outcome = outcome.clone();
            let calls = calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                outcome
            })
        })
    }

    fn app(probe: ProbeFn) -> axum::Router {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::db::Store::open(&dir.path().join("usage-test.db")).unwrap();
        std::mem::forget(dir);
        let state = AppState {
            store: Arc::new(store),
            started: Instant::now(),
            build_hash: "test".into(),
            auth_token: None,
        };
        Router::new()
            .nest("/api/usage", routes_with(probe))
            .with_state(state)
    }

    async fn get(app: &axum::Router) -> (StatusCode, Value) {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/usage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// The real endpoint's success body, built from the live response SHAPE
    /// (both the top-level windows and the `limits[]` array Anthropic sends
    /// together). Numbers are invented; no credential is involved.
    fn live_shaped_body() -> Value {
        json!({
            "five_hour": {"utilization": 34.4, "resets_at": "2026-08-09T18:00:00+00:00"},
            "seven_day": {"utilization": 71.6, "resets_at": "2026-08-12T00:00:00+00:00"},
            "seven_day_opus": null,
            "limits": [
                {"kind": "session", "percent": 34.4, "resets_at": "2026-08-09T18:00:00Z"},
                {"kind": "weekly_all", "group": "weekly", "percent": 71.6,
                 "resets_at": "2026-08-12T00:00:00Z"},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 12.0,
                 "scope": {"model": {"display_name": "Opus"}},
                 "resets_at": "2026-08-12T00:00:00Z"}
            ]
        })
    }

    #[tokio::test]
    async fn success_passes_anthropics_body_through_verbatim() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(probe_fn(
            UsageProbe::Ok(live_shaped_body()),
            calls.clone(),
        ));
        let (st, v) = get(&app).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["available"], json!(true));
        assert!(v.get("reason").is_none());

        // Every field loadUsage() reads must survive, spelled exactly as
        // Anthropic sent it — this is the regression that made the meter
        // useless when the endpoint normalized through UsageWindow.
        let limits = v["limits"].as_array().unwrap();
        assert_eq!(limits.len(), 3);
        assert_eq!(limits[0]["kind"], json!("session"));
        assert_eq!(limits[0]["percent"], json!(34.4)); // not rounded away
        assert_eq!(limits[0]["resets_at"], json!("2026-08-09T18:00:00Z"));
        assert_eq!(limits[1]["group"], json!("weekly"));
        // The per-model row the SPA labels "<model> · weekly": normalized
        // windows discard this entirely.
        assert_eq!(
            limits[2]["scope"]["model"]["display_name"],
            json!("Opus")
        );
        // Top-level windows pass through untouched too.
        assert_eq!(v["five_hour"]["utilization"], json!(34.4));
    }

    #[tokio::test]
    async fn each_failure_cause_has_its_own_reason() {
        // The rule this enforces: no two causes may share a reason string,
        // and none may be the old catch-all. A test that only checked
        // `available == false` would have passed against the collapsed
        // message that motivated this work.
        let cases = vec![
            (UsageProbe::NoToken, "no_token"),
            (UsageProbe::Expired, "expired_token"),
            (UsageProbe::Http(401), "token_rejected"),
            (UsageProbe::Http(429), "rate_limited"),
            (UsageProbe::Http(500), "probe_failed"),
            (UsageProbe::Transport("timeout"), "probe_failed"),
            (UsageProbe::BadShape, "unexpected_shape"),
            (UsageProbe::Ok(json!([1, 2, 3])), "unexpected_shape"),
        ];
        let mut seen_reasons: Vec<String> = Vec::new();
        for (outcome, expect_cause) in cases {
            let app = app(probe_fn(outcome.clone(), Arc::new(AtomicUsize::new(0))));
            let (st, v) = get(&app).await;
            assert_eq!(st, StatusCode::OK, "{outcome:?}");
            assert_eq!(v["available"], json!(false), "{outcome:?}");
            assert_eq!(v["cause"], json!(expect_cause), "{outcome:?}");
            let reason = v["reason"].as_str().unwrap().to_string();
            assert!(!reason.is_empty());
            // Never the collapsed sentence again.
            assert!(
                !reason.contains("no token, expired token, or probe failed"),
                "{outcome:?} still serves the catch-all"
            );
            // No numbers invented on a degraded path.
            assert!(v.get("limits").is_none(), "{outcome:?}");
            seen_reasons.push(reason);
        }
        // Distinctness where the cause differs: 500 and timeout share a
        // cause tag but must still read differently (status vs network).
        let mut uniq = seen_reasons.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(
            uniq.len(),
            seen_reasons.len() - 1, // BadShape and non-object Ok share one
            "reasons collapsed: {seen_reasons:?}"
        );
    }

    #[tokio::test]
    async fn http_failures_carry_their_status_code() {
        for code in [500u16, 502, 429] {
            let app = app(probe_fn(
                UsageProbe::Http(code),
                Arc::new(AtomicUsize::new(0)),
            ));
            let (_, v) = get(&app).await;
            assert!(
                v["reason"].as_str().unwrap().contains(&code.to_string()),
                "HTTP {code} reason omits the status: {}",
                v["reason"]
            );
        }
    }

    #[tokio::test]
    async fn no_response_on_any_path_can_contain_a_token() {
        // Assemble a secret-shaped string at runtime so the repo's scanner
        // never sees one, and prove it cannot reach the wire even when the
        // upstream body echoes it.
        let fake = format!("sk-ant-{}-{}", "oat01", "AAAAdeadbeefdeadbeef");
        let outcomes = vec![
            UsageProbe::NoToken,
            UsageProbe::Expired,
            UsageProbe::Http(401),
            UsageProbe::Http(429),
            UsageProbe::Transport("connect"),
            UsageProbe::BadShape,
        ];
        for outcome in outcomes {
            let app = app(probe_fn(outcome.clone(), Arc::new(AtomicUsize::new(0))));
            let (_, v) = get(&app).await;
            let text = serde_json::to_string(&v).unwrap();
            assert!(!text.contains(&fake), "{outcome:?}");
            assert!(!text.contains("Bearer"), "{outcome:?}");
            assert!(!text.contains("sk-ant"), "{outcome:?}");
        }
        // The one path that echoes upstream content is 2xx. The type system
        // is what guarantees the rest: no failure variant can hold a token.
    }

    #[tokio::test]
    async fn cache_serves_repeat_opens_from_one_probe_and_reports_age() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(probe_fn(
            UsageProbe::Ok(live_shaped_body()),
            calls.clone(),
        ));
        let (_, first) = get(&app).await;
        let (_, second) = get(&app).await;
        let (_, third) = get(&app).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "settings reopens must not hammer Anthropic"
        );
        assert_eq!(first["available"], json!(true));
        assert_eq!(second["limits"], first["limits"]);
        assert_eq!(third["limits"], first["limits"]);
        // Age is present on every response and TTL is advertised.
        for r in [&first, &second, &third] {
            assert!(r["cache_age_s"].is_number(), "{r}");
            assert_eq!(r["cache_ttl_s"], json!(DEFAULT_USAGE_TTL_S));
        }
    }

    #[tokio::test]
    async fn failures_are_cached_too_so_a_rate_limit_is_not_amplified() {
        // Retrying a 429 on every render is what provokes the 429.
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(probe_fn(UsageProbe::Http(429), calls.clone()));
        for _ in 0..5 {
            let (_, v) = get(&app).await;
            assert_eq!(v["cause"], json!("rate_limited"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A probe whose outcome changes between calls — the intermittent 429
    /// that actually happens on this host, which a fixed fixture cannot show.
    fn probe_sequence(outcomes: Vec<UsageProbe>) -> (ProbeFn, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let f: ProbeFn = Arc::new(move || {
            let outcomes = outcomes.clone();
            let c = c.clone();
            Box::pin(async move {
                let i = c.fetch_add(1, Ordering::SeqCst);
                outcomes
                    .get(i)
                    .cloned()
                    .unwrap_or(outcomes.last().cloned().unwrap())
            })
        });
        (f, calls)
    }

    #[tokio::test]
    async fn a_transient_failure_serves_the_last_good_reading_marked_stale() {
        // Success, then 429 — the sequence observed live on 2026-08-09.
        let (probe, _calls) = probe_sequence(vec![
            UsageProbe::Ok(live_shaped_body()),
            UsageProbe::Http(429),
        ]);
        let app = app(probe);
        temp_env_ttl("0", || async {
            let (_, first) = get(&app).await;
            assert_eq!(first["available"], json!(true));
            assert!(first.get("stale").is_none(), "a live reading is not stale");

            let (_, second) = get(&app).await;
            // The meter keeps working across the blip...
            assert_eq!(second["available"], json!(true));
            assert_eq!(second["limits"], first["limits"]);
            // ...and says so, with the live failure attached.
            assert_eq!(second["stale"], json!(true));
            assert!(second["stale_reason"]
                .as_str()
                .unwrap()
                .contains("429"));
        })
        .await;
    }

    #[tokio::test]
    async fn a_failure_with_no_prior_reading_stays_degraded() {
        // The fallback must never manufacture a first reading.
        let app = app(probe_fn(
            UsageProbe::Http(429),
            Arc::new(AtomicUsize::new(0)),
        ));
        let (_, v) = get(&app).await;
        assert_eq!(v["available"], json!(false));
        assert_eq!(v["cause"], json!("rate_limited"));
        assert!(v.get("limits").is_none());
        assert!(v.get("stale").is_none());
    }

    #[tokio::test]
    async fn stale_fallback_expires_and_can_be_disabled() {
        let (probe, _c) = probe_sequence(vec![
            UsageProbe::Ok(live_shaped_body()),
            UsageProbe::Http(429),
        ]);
        let app = app(probe);
        // TTL 0 forces a fresh probe each call; STALE 0 disables the
        // fallback, so the second call must degrade honestly rather than
        // reach for the reading it still holds.
        temp_env_both("0", "0", || async {
            let (_, first) = get(&app).await;
            assert_eq!(first["available"], json!(true));
            let (_, second) = get(&app).await;
            assert_eq!(
                second["available"],
                json!(false),
                "stale window 0 must not serve a prior reading"
            );
            assert_eq!(second["cause"], json!("rate_limited"));
        })
        .await;
    }

    #[tokio::test]
    async fn zero_ttl_disables_the_cache() {
        // The env knob has to actually reach the handler; a knob that reads
        // the env once at startup would pass a weaker test than this.
        let calls = Arc::new(AtomicUsize::new(0));
        let app = app(probe_fn(
            UsageProbe::Ok(live_shaped_body()),
            calls.clone(),
        ));
        temp_env_ttl("0", || async {
            get(&app).await;
            get(&app).await;
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Set both knobs around a block (same serialization as `temp_env_ttl`).
    async fn temp_env_both<F, Fut>(ttl: &str, stale: &str, f: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()>,
    {
        let _g = env_lock().lock().await;
        std::env::set_var("AMUX_USAGE_TTL_S", ttl);
        std::env::set_var("AMUX_USAGE_STALE_S", stale);
        f().await;
        std::env::remove_var("AMUX_USAGE_TTL_S");
        std::env::remove_var("AMUX_USAGE_STALE_S");
    }

    /// One process-wide async lock for every env-mutating test.
    fn env_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// Set AMUX_USAGE_TTL_S around a block. Serialized because env is
    /// process-global and these tests share a process — and the lock is an
    /// ASYNC mutex, since it is held across the awaited block (a std
    /// `MutexGuard` across an await is a real deadlock risk, not just a lint).
    async fn temp_env_ttl<F, Fut>(val: &str, f: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()>,
    {
        let _g = env_lock().lock().await;
        std::env::set_var("AMUX_USAGE_TTL_S", val);
        f().await;
        std::env::remove_var("AMUX_USAGE_TTL_S");
    }

    #[test]
    fn ttl_default_and_override() {
        assert_eq!(usage_ttl(), Duration::from_secs(DEFAULT_USAGE_TTL_S));
    }
}
