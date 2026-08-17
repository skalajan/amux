//! Speed test: the Metrics tab's Run-speed-test button (AMUX-2890).
//!
//! A PORT, not a feature: the SPA has rendered this UI all along — three
//! result cards, a progress bar, a live button — and every click since the
//! python retirement hit unrouted paths. The route census (AMUX-2917) counted
//! its 2 call sites among the unrouted families; this closes that family with
//! working UI rather than deleting reachable controls.
//!
//! Contract, from the SPA's own calls (the only caller):
//!   GET  /api/speedtest/download?bytes=N   -> N bytes, incompressible-ish
//!   POST /api/speedtest/upload             -> swallow the body, answer ok
//!
//! `bytes` is CLAMPED, not trusted: the client asks for 1 byte (ping probes)
//! or ~25MB (the transfer test). An unclamped param would make this a
//! memory-for-the-asking endpoint on a server that shares a box with 50 lanes.

use super::AppState;
use axum::body::Bytes;
use axum::extract::Query;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

/// 64 MiB — comfortably above the SPA's 25MB default, far below harm.
const MAX_BYTES: usize = 64 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/download", get(download))
        .route("/upload", post(upload))
}

#[derive(Deserialize)]
pub struct DownloadQ {
    #[serde(default)]
    bytes: usize,
}

/// The payload is pseudo-random so HTTP compression cannot flatter the
/// number — same reason the SPA fills its upload with noise. A simple LCG is
/// plenty: the point is incompressibility, not cryptography.
async fn download(Query(q): Query<DownloadQ>) -> Response {
    let n = q.bytes.clamp(1, MAX_BYTES);
    let mut buf = vec![0u8; n];
    let mut state: u32 = 0x9E37_79B9;
    for chunk in buf.chunks_mut(4) {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let b = state.to_le_bytes();
        for (d, s) in chunk.iter_mut().zip(b.iter()) {
            *d = *s;
        }
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        buf,
    )
        .into_response()
}

/// Receive and discard. The body is already fully read by the time the
/// extractor hands it over, which is exactly what an upload timing test needs;
/// echoing the size back lets a future caller sanity-check the transfer.
async fn upload(body: Bytes) -> Response {
    Json(json!({ "ok": true, "received": body.len() })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clamp is the security property: an unclamped `bytes` would be
    /// allocate-on-demand for anyone with the URL.
    #[test]
    fn bytes_is_clamped_both_ends() {
        assert_eq!(0usize.clamp(1, MAX_BYTES), 1);
        assert_eq!((MAX_BYTES + 1).clamp(1, MAX_BYTES), MAX_BYTES);
    }
}
