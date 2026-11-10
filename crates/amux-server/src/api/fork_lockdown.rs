//! Fork-only exposure controls: the listen address and owner-token gating for diagnostics upstream serves publicly.

use super::AppState;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::IpAddr;

/// Paths upstream mounts outside `require_bearer`; here they need the owner token like any other API call.
const LOCKED_PREFIXES: &[&str] = &[
    "/api/debug/",
    "/api/system-jobs",
    "/api/board/nudges",
    "/api/health/invariants",
    "/api/calendar.ics",
];

/// The address every amux listener binds: `AMUX_RS_BIND`, else upstream's 0.0.0.0.
pub fn bind_ip() -> IpAddr {
    parse_bind(std::env::var("AMUX_RS_BIND").ok().as_deref())
}

fn parse_bind(raw: Option<&str>) -> IpAddr {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        None => IpAddr::from([0, 0, 0, 0]),
        Some(v) => v.parse().unwrap_or_else(|_| {
            // An unreadable value must not fall back to every interface.
            tracing::warn!(value = v, verdict = "bind_address_invalid", "AMUX_RS_BIND is not an IP address; listening on 127.0.0.1");
            IpAddr::from([127, 0, 0, 1])
        }),
    }
}

fn is_locked(path: &str) -> bool {
    LOCKED_PREFIXES.iter().any(|p| path.starts_with(p))
}

pub async fn guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if is_locked(req.uri().path()) && state.auth_token.is_some() && !super::auth::has_owner_token(&state, req.headers(), req.uri()) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "unauthorized", "reason": "owner_token_required"})),
        )
            .into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_defaults_to_all_interfaces_and_fails_closed_on_garbage() {
        assert_eq!(parse_bind(None), IpAddr::from([0, 0, 0, 0]));
        assert_eq!(parse_bind(Some("  ")), IpAddr::from([0, 0, 0, 0]));
        assert_eq!(parse_bind(Some("127.0.0.1")), IpAddr::from([127, 0, 0, 1]));
        assert_eq!(parse_bind(Some("::1")), "::1".parse::<IpAddr>().unwrap());
        assert_eq!(parse_bind(Some("localhost")), IpAddr::from([127, 0, 0, 1]));
    }

    #[test]
    fn locks_the_public_diagnostics_and_nothing_the_shell_needs() {
        for p in ["/api/debug/tmux", "/api/system-jobs", "/api/system-jobs/x/run", "/api/board/nudges", "/api/health/invariants", "/api/calendar.ics"] {
            assert!(is_locked(p), "{p}");
        }
        for p in ["/health", "/api/health", "/", "/manifest.json", "/api/connection/session", "/api/share/x", "/api/bridge/hook/telegram"] {
            assert!(!is_locked(p), "{p}");
        }
    }
}
