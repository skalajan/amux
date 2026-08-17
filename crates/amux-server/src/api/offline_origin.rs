//! `GET /api/offline-origin` (AMUX-2599) — which origin can actually run a
//! service worker.
//!
//! A browser refuses to fetch `/sw.js` over a self-signed certificate, so amux
//! reached at `https://localhost:8824` or a raw LAN IP can never cache anything
//! and goes BLANK offline; the same server at its Tailscale hostname carries a
//! real certificate and works fully. The client cannot determine this itself —
//! only the server knows which cert it is actually serving — so it asks.
//!
//! # Why this answers from the CERT DIRECTORY, not from `tailscale status`
//!
//! Python shelled out to `tailscale status --self --json` and reported the
//! MagicDNS name. But the question the client is really asking is "is there an
//! origin here whose TLS a browser will trust", and being ON a tailnet does not
//! imply this server has that tailnet's certificate. `tls::build_server_config`
//! already answers the true question when it builds the SNI resolver: it scans
//! the TLS dir for `<host>.ts.net.crt` + `.key` and only then serves a trusted
//! cert for that name. Reading the same directory here means `trusted_cert`
//! reports the cert we ACTUALLY serve rather than a hostname we merely have —
//! two sources of truth that could disagree, collapsed into one (ethos rule 4).
//!
//! # The proxied case is not a certificate problem (AC-294)
//!
//! When TLS was terminated by a gateway, a service-worker failure here has
//! nothing to do with our certificate and there is nothing for the reader to
//! install. Conflating the two once put a full-width red banner on EVERY cloud
//! workspace telling people to fix a cert they do not own. So the proxied
//! branch says what is true and what the reader can do, which here is nothing,
//! and sets `proxied: true` — the flag the SPA uses to suppress the banner
//! entirely.

use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

/// The tailnet hostname we hold a usable certificate for, or `None`.
///
/// Mirrors the scan in [`crate::tls::build_server_config`] — deliberately the
/// same predicate (a `.crt` AND its `.key` both present and readable), because
/// a view that disagrees with the mechanism it describes is worse than no view.
fn trusted_ts_hostname() -> Option<String> {
    let dir = crate::config::ServerConfig::from_process_env().tls_dir();
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(host) = name.strip_suffix(".crt") else {
            continue;
        };
        if !host.contains(".ts.net") {
            continue;
        }
        let key = e.path().with_file_name(format!("{host}.key"));
        if e.path().is_file() && key.is_file() {
            return Some(host.to_string());
        }
    }
    None
}

/// The port a browser should use for the good origin.
///
/// The CANONICAL port (`AMUX_RS_PORT`), never the legacy one. This used to
/// prefer `AMUX_RS_LEGACY_PORT` on the reasoning that 8822 was what every
/// client already had baked in — true at the time, and exactly backwards for
/// the thing this endpoint produces: a URL a human is told to **re-add to
/// their home screen**. That is a bookmark, it outlives the process that
/// generated it, and 8822 is a retired address whose compatibility bind is
/// being removed (see `crate::legacy_port`). Handing out a soon-dead origin as
/// the permanent one is the one output where "what everyone currently uses"
/// is the wrong answer.
fn self_port() -> u16 {
    crate::config::canonical_port()
}

pub async fn offline_origin(headers: HeaderMap) -> Json<Value> {
    let ts = trusted_ts_hostname().unwrap_or_default();

    // Python lowercased before comparing; a gateway sending `HTTPS` must not
    // fall through to the cert-advice branch.
    let proxied = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().eq_ignore_ascii_case("https"))
        .unwrap_or(false);

    if proxied {
        let host = headers
            .get("x-forwarded-host")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .or_else(|| headers.get("host").and_then(|v| v.to_str().ok()))
            .unwrap_or("");
        return Json(json!({
            "tailscale_hostname": ts,
            "good_origin": if host.is_empty() { String::new() } else { format!("https://{host}") },
            "trusted_cert": true,
            "proxied": true,
            "why": "This origin's TLS was terminated by a proxy with a real \
                    certificate, so a service-worker failure here is NOT a \
                    certificate problem and there is nothing to install. \
                    Offline mode is unavailable on this workspace.",
        }));
    }

    let good = if ts.is_empty() {
        String::new()
    } else {
        format!("https://{ts}:{}", self_port())
    };
    Json(json!({
        "tailscale_hostname": ts,
        "good_origin": good,
        "trusted_cert": !ts.is_empty(),
        "why": if good.is_empty() {
            "No Tailscale certificate found, so amux only has a self-signed cert. \
             Service workers will not install and offline mode cannot work. \
             Run `tailscale cert <host>.ts.net` into ~/.amux/tls, or `mkcert`, and restart amux."
        } else {
            "Service workers refuse to install over a self-signed certificate. \
             Install the PWA from this origin so offline mode can cache."
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(h: HeaderMap) -> Value {
        futures::executor::block_on(async { offline_origin(h).await.0 })
    }

    // The AC-294 regression: a proxied origin must NEVER be told to install a
    // certificate, and must set `proxied` so the SPA suppresses the banner.
    #[test]
    fn proxied_origin_reports_proxied_and_offers_no_cert_advice() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-proto", "https".parse().unwrap());
        h.insert("x-forwarded-host", "cloud.amux.io".parse().unwrap());
        let v = body(h);
        assert_eq!(v["proxied"], json!(true));
        assert_eq!(v["trusted_cert"], json!(true));
        assert_eq!(v["good_origin"], json!("https://cloud.amux.io"));
        let why = v["why"].as_str().unwrap();
        assert!(
            !why.contains("mkcert") && !why.contains("self-signed"),
            "proxied branch must not give certificate advice: {why}"
        );
    }

    // Case-insensitivity is load-bearing: a gateway sending `HTTPS` would
    // otherwise fall through to the cert-advice branch and re-raise the banner.
    #[test]
    fn forwarded_proto_is_case_insensitive() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-proto", "HTTPS".parse().unwrap());
        h.insert("host", "cloud.amux.io".parse().unwrap());
        assert_eq!(body(h)["proxied"], json!(true));
    }

    // Direct origin: `proxied` must be ABSENT (not false) — the SPA branches on
    // its presence, and python omitted the key entirely.
    #[test]
    fn direct_origin_omits_proxied_key() {
        let v = body(HeaderMap::new());
        assert!(
            v.get("proxied").is_none(),
            "direct branch must omit `proxied`, got {v}"
        );
        assert!(v["why"].as_str().unwrap().contains("service worker")
            || v["why"].as_str().unwrap().contains("Service worker"));
    }

    // trusted_cert must track the CERT, not merely tailnet membership.
    #[test]
    fn trusted_cert_tracks_good_origin() {
        let v = body(HeaderMap::new());
        let has_origin = !v["good_origin"].as_str().unwrap_or("").is_empty();
        assert_eq!(v["trusted_cert"], json!(has_origin));
    }
}
