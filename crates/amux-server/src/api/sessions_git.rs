//! `GET /api/sessions-git` (AMUX-2599) — the bulk git map the session cards use.
//!
//! One request instead of 80. The SPA calls this on every session refresh
//! (throttled to 20s client-side) and reads three things per session: `branch`,
//! `repo`, and the `_conflict` flag it synthesizes itself by grouping on
//! `repo + branch`.
//!
//! # Shape (python parity, py:66429)
//!
//! An OBJECT keyed by session name, NOT an array:
//! `{"<name>": {"name": "<name>", "branch": "<str>", "repo": "<abs toplevel>"}}`
//! Sessions whose directory has no branch are OMITTED entirely — the client's
//! conflict grouping does a hard `continue` on a falsy `repo`, so a row with an
//! empty branch would be dead weight in every payload.
//!
//! # Why this does not shell git for the branch
//!
//! `sessions_legacy::build_array` already resolves a branch per session,
//! deduped by directory, for the session list the SPA renders alongside this.
//! Computing it a second time here would (a) double the git subprocess load on
//! a 50-session fleet and (b) create two answers to the same question that can
//! disagree — the SPA reads `sess.branch` preferentially and falls back to
//! `gi.branch`, so a disagreement would surface as a badge that changes
//! depending on which fetch landed last. So the branch is REUSED and only
//! `repo` (the one field the session list does not carry) is computed here,
//! one `rev-parse --show-toplevel` per DISTINCT directory.
//!
//! # The cache is not an optimization, it is a load ceiling
//!
//! Every open client polls this. Without the 30s TTL the git subprocess count
//! scales with (clients x sessions), which is the shape that made python add
//! the same TTL. A stale-by-30s branch is correct: branches change on commit
//! cadence, not seconds.

use super::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// py:21339 `_SESSIONS_GIT_CACHE_TTL`.
const CACHE_TTL: Duration = Duration::from_secs(30);
/// py's `threading.Semaphore(4)` — a cap on concurrent git subprocesses so a
/// 50-session fleet cannot fork-bomb the box on one refresh.
const GIT_CONCURRENCY: usize = 4;
const GIT_TIMEOUT: Duration = Duration::from_secs(2);

static CACHE: Mutex<Option<(Instant, Value)>> = Mutex::new(None);

fn cached() -> Option<Value> {
    let g = CACHE.lock().ok()?;
    let (at, v) = g.as_ref()?;
    (at.elapsed() < CACHE_TTL).then(|| v.clone())
}

async fn git_toplevel(dir: &str) -> Option<String> {
    let out = tokio::time::timeout(
        GIT_TIMEOUT,
        tokio::process::Command::new("git")
            .args(["-C", dir, "rev-parse", "--show-toplevel"])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

pub async fn sessions_git(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    if let Some(v) = cached() {
        return (StatusCode::OK, Json(v));
    }

    // (name, dir, branch) from the SAME source the session list renders.
    let rows: Vec<(String, String, String)> = {
        let conn = match state.store.read() {
            Ok(c) => c,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("store unreadable: {e}")})),
                )
            }
        };
        match super::sessions_legacy::build_array(&conn) {
            Ok(arr) => arr
                .iter()
                .filter_map(|v| {
                    let name = v["name"].as_str()?.to_string();
                    let dir = v["dir"].as_str().unwrap_or("").to_string();
                    let branch = v["branch"].as_str().unwrap_or("").to_string();
                    (!name.is_empty() && !dir.is_empty() && !branch.is_empty())
                        .then_some((name, dir, branch))
                })
                .collect(),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("session list unavailable: {e}")})),
                )
            }
        }
    };

    // One git call per DISTINCT directory, then fan the answer out to every
    // session sharing it (many sessions share one checkout).
    let dirs: Vec<String> = rows
        .iter()
        .map(|(_, d, _)| d.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut repos: BTreeMap<String, String> = BTreeMap::new();
    for chunk in dirs.chunks(GIT_CONCURRENCY) {
        let results = futures::future::join_all(chunk.iter().map(|d| async move {
            (d.clone(), git_toplevel(d).await)
        }))
        .await;
        for (d, top) in results {
            if let Some(t) = top {
                repos.insert(d, t);
            }
        }
    }

    let mut out = Map::new();
    for (name, dir, branch) in rows {
        let Some(repo) = repos.get(&dir) else { continue };
        out.insert(
            name.clone(),
            json!({"name": name, "branch": branch, "repo": repo}),
        );
    }
    let body = Value::Object(out);

    if let Ok(mut g) = CACHE.lock() {
        *g = Some((Instant::now(), body.clone()));
    }
    (StatusCode::OK, Json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The cache must actually expire — a TTL that never lets go turns a live
    // branch map into a frozen one, and the failure is invisible (the payload
    // still looks well-formed).
    #[test]
    fn cache_is_a_ttl_not_a_permanent_store() {
        {
            let mut g = CACHE.lock().unwrap();
            *g = Some((Instant::now(), json!({"a": 1})));
        }
        assert!(cached().is_some(), "fresh entry should be served");
        {
            let mut g = CACHE.lock().unwrap();
            *g = Some((Instant::now() - CACHE_TTL - Duration::from_secs(1), json!({"a": 1})));
        }
        assert!(cached().is_none(), "expired entry must not be served");
        *CACHE.lock().unwrap() = None;
    }

    // The SPA does `if (!gi.repo) continue`, so a row without a repo is dead
    // weight. This pins the omission rule rather than trusting the comment.
    #[tokio::test]
    async fn a_directory_that_is_not_a_repo_yields_no_row() {
        let tmp = std::env::temp_dir().join(format!("amux-nonrepo-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(
            git_toplevel(tmp.to_str().unwrap()).await.is_none(),
            "a non-repo dir must not produce a repo"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
