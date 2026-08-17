//! `/health` (RR-0021): build hash, uptime, store status, global revision.
//!
//! The Python workflow's hard-won rule — "bracket any measurement with
//! /health's build" — carries over verbatim: `build` is a content hash of
//! the running binary, so a restart with the same code and a restart with
//! different code are distinguishable (ethos rule 4).

use super::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct Health {
    pub status: &'static str,
    pub build: String,
    pub uptime_s: u64,
    pub rev: Option<u64>,
    pub store: &'static str,
    pub pid: u32,
    pub server: &'static str,
    /// Open descriptors / the process's own RLIMIT_NOFILE soft limit.
    ///
    /// AMUX-2812: on 2026-08-10 this process hit macOS's launchd default of 256
    /// and every open and spawn began failing with EMFILE. `/health` then went
    /// SILENT — which from the outside is indistinguishable from a dead
    /// machine, so the dashboard simply read "offline" and the first stretch of
    /// the investigation went looking for a crash that had not happened.
    ///
    /// Answering "out of descriptors" while out of descriptors is not reliably
    /// possible: the reply needs a socket. So the useful move is the one BEFORE
    /// that — publish the number continuously, so the approach is visible in
    /// the instrument everyone already polls, and `degraded` arrives while the
    /// server can still say it. `None` when unmeasurable, which is honestly
    /// different from zero.
    pub fds: Option<FdHealth>,
    /// Invariant-monitor confidence (AMUX-2625): healthy | degraded | unknown |
    /// unhealthy, from the last monitor roll-up. `unknown` when the monitor has
    /// not run yet OR its evidence is stale — the four states this card is
    /// about, on the endpoint every consumer already polls. Separate from
    /// `status` on purpose: `status` gates request health (store/fds), this
    /// carries the fleet-invariant verdict, and one must not mask the other.
    pub confidence: &'static str,
    /// Age of the evidence behind `confidence`, seconds. `None` if the monitor
    /// has never recorded a verdict. A growing age with `confidence:unknown` is
    /// a wedged monitor — visible instead of a frozen `healthy`.
    pub confidence_age_s: Option<f64>,
}

#[derive(Serialize)]
pub struct FdHealth {
    pub open: usize,
    pub limit: u64,
    pub ratio: f64,
}

/// The same `AMUX_FD_MAX_RATIO` the autofix detector triggers on, read from the
/// same env var — so `/health` saying `degraded` and a card being filed are the
/// SAME condition, not two thresholds that drift apart. A view must share the
/// predicate of the mechanism it describes (ethos rule 1).
fn fd_ceiling() -> f64 {
    std::env::var("AMUX_FD_MAX_RATIO")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.7)
        .clamp(0.1, 0.99)
}

/// Costs one descriptor and NO subprocess — `lsof` would fail exactly when the
/// condition it reports is present, and each attempt would consume the resource
/// being measured (ethos rule 7: what does the detector cost, and is it paid in
/// the same resource as the fault?).
fn fd_health() -> Option<FdHealth> {
    let open = std::fs::read_dir("/dev/fd").ok()?.count();
    let mut rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit writes into a fully-initialised local; no aliasing.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } != 0 || rl.rlim_cur == 0 {
        return None;
    }
    let limit = rl.rlim_cur;
    Some(FdHealth { open, limit, ratio: open as f64 / limit as f64 })
}

pub async fn health(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    // A store that cannot answer the revision query is degraded — surface
    // that instead of a green lie (ethos rule 7: a check must be able to
    // fail).
    let (rev, store, code) = match state.store.current_rev() {
        Ok(rev) => (Some(rev.0), "ok", StatusCode::OK),
        Err(_) => (None, "hung", StatusCode::SERVICE_UNAVAILABLE),
    };
    let fds = fd_health();
    // Descriptor pressure degrades `status` but does NOT fail the request: the
    // server is still serving, and returning 503 here would take the fleet down
    // on a warning. `status` is the field that carries the warning; `store` is
    // the one that gates the code.
    let fd_tight = fds.as_ref().is_some_and(|f| f.ratio >= fd_ceiling());
    // Cheap read of the monitor's last cached verdict — no DB, no re-run, and
    // correct (unknown) even if the monitor is dead (AMUX-2625).
    let (conf, conf_age) =
        crate::invariants::health_confidence(chrono::Utc::now().timestamp() as f64);
    (
        code,
        Json(Health {
            status: if store != "ok" {
                "degraded"
            } else if fd_tight {
                "degraded-fds"
            } else {
                "ok"
            },
            build: state.build_hash.clone(),
            uptime_s: state.started.elapsed().as_secs(),
            rev,
            store,
            pid: std::process::id(),
            server: "amux-rust",
            fds,
            confidence: conf.as_str(),
            confidence_age_s: conf_age,
        }),
    )
}

/// GET /api/debug/tmux — runs the exact fleet-discovery command from INSIDE
/// this process and reports argv, exit status, output sizes, and the env
/// that determines the socket. Exists because the live launchd instance
/// served running=0 for the whole fleet while the same binary in a login
/// shell served 49, and no log line could say why (ethos rule 4: the
/// instrument must express the discriminator, from the consumer's vantage).
pub async fn debug_tmux() -> axum::Json<serde_json::Value> {
    let out = std::process::Command::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_activity}\t#{session_created}",
        ])
        .output();
    let which = std::process::Command::new("which").arg("tmux").output();
    axum::Json(match out {
        Ok(o) => serde_json::json!({
            "spawn": "ok",
            "exit": o.status.to_string(),
            "stdout_bytes": o.stdout.len(),
            "stdout_lines": String::from_utf8_lossy(&o.stdout).lines().count(),
            "stdout_head": String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or(""),
            "stderr": String::from_utf8_lossy(&o.stderr).trim(),
            "which_tmux": which.ok().map(|w| String::from_utf8_lossy(&w.stdout).trim().to_string()),
            "env_path": std::env::var("PATH").unwrap_or_default(),
            "env_tmux_tmpdir": std::env::var("TMUX_TMPDIR").ok(),
            "env_tmpdir": std::env::var("TMPDIR").ok(),
            "env_tmux": std::env::var("TMUX").ok(),
            "cwd": std::env::current_dir().ok().map(|p| p.display().to_string()),
        }),
        Err(e) => serde_json::json!({ "spawn": "failed", "error": e.to_string() }),
    })
}
