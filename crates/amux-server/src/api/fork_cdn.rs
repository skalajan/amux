//! Fork-only: the dashboard's CDN libraries are served from this binary (vendored by fork/cdn-vendor.py), so the browser never fetches code from a third party.

use super::AppState;
use axum::body::Body;
use axum::extract::{Path, Request};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;

#[derive(rust_embed::RustEmbed)]
#[folder = "fork-cdn/files/"]
struct Vendored;

const HOSTS: &[&str] = &["cdn.jsdelivr.net", "unpkg.com", "cdnjs.cloudflare.com", "esm.sh"];

/// Scripts from any other origin are refused by the browser, so a CDN load upstream adds later fails instead of reaching out.
const CSP: &str = "script-src 'self' 'unsafe-inline' 'unsafe-eval' blob: data:";

/// Marks this fork's ETags so a browser copy cached before the rewrite is never revalidated as current.
const ETAG_MARK: &str = "-fc";

pub fn routes() -> Router<AppState> {
    Router::new().route("/cdn/{*path}", axum::routing::get(serve))
}

async fn serve(Path(path): Path<String>) -> Response {
    let Some(file) = Vendored::get(&path) else {
        return (StatusCode::NOT_FOUND, "not vendored: run fork/cdn-vendor.py").into_response();
    };
    let body = if is_text(&path) {
        rewrite(&String::from_utf8_lossy(&file.data)).into_bytes()
    } else {
        file.data.into_owned()
    };
    (
        [(header::CONTENT_TYPE, mime(&path)), (header::CACHE_CONTROL, "public, max-age=86400")],
        body,
    )
        .into_response()
}

pub fn rewrite(text: &str) -> String {
    let mut out = text.to_string();
    for host in HOSTS {
        out = out.replace(&format!("https://{host}/"), &format!("/cdn/{host}/"));
    }
    out
}

/// Rewrites the CDN addresses in dashboard HTML, JS and CSS and sets the script CSP on its pages.
pub async fn layer(mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if path.starts_with("/pub/") {
        return next.run(req).await;
    }
    if let Some(tags) = req.headers().get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) {
        let ours: Vec<String> = tags
            .split(',')
            .filter_map(|t| t.trim().strip_suffix(&format!("{ETAG_MARK}\"")).map(|t| format!("{t}\"")))
            .collect();
        match HeaderValue::from_str(&ours.join(", ")) {
            Ok(v) if !ours.is_empty() => {
                req.headers_mut().insert(header::IF_NONE_MATCH, v);
            }
            _ => {
                req.headers_mut().remove(header::IF_NONE_MATCH);
            }
        }
    }
    let resp = next.run(req).await;
    let (mut parts, body) = resp.into_parts();
    if let Some(tag) = parts.headers.get(header::ETAG).and_then(|v| v.to_str().ok()) {
        if let Some(stem) = tag.strip_suffix('"') {
            if let Ok(v) = HeaderValue::from_str(&format!("{stem}{ETAG_MARK}\"")) {
                parts.headers.insert(header::ETAG, v);
            }
        }
    }
    let ctype = parts.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if ctype.starts_with("text/html") {
        parts.headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    }
    if !(ctype.starts_with("text/html") || ctype.contains("javascript") || ctype.starts_with("text/css")) {
        return Response::from_parts(parts, body);
    }
    let Ok(bytes) = axum::body::to_bytes(body, 256 * 1024 * 1024).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "dashboard asset unreadable").into_response();
    };
    let text = String::from_utf8_lossy(&bytes);
    if !HOSTS.iter().any(|h| text.contains(&format!("https://{h}/"))) {
        return Response::from_parts(parts, Body::from(bytes));
    }
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(rewrite(&text)))
}

fn is_text(path: &str) -> bool {
    matches!(path.rsplit('.').next(), Some("js" | "mjs" | "css" | "html" | "json" | "map"))
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_every_cdn_host_and_leaves_the_rest() {
        let src = r#"<script src="https://cdn.jsdelivr.net/npm/a@1/a.js"></script> import('https://esm.sh/x') 'https://unpkg.com/' + p https://cdnjs.cloudflare.com/ajax/libs/h/1/h.js https://example.com/x"#;
        let out = rewrite(src);
        assert!(!HOSTS.iter().any(|h| out.contains(&format!("https://{h}/"))), "{out}");
        assert!(out.contains("/cdn/cdn.jsdelivr.net/npm/a@1/a.js") && out.contains("'/cdn/unpkg.com/' + p"));
        assert!(out.contains("https://example.com/x"));
    }

    #[test]
    fn every_url_the_dashboard_names_is_vendored() {
        let lock = include_str!("../../fork-cdn/lock.tsv");
        let locked: std::collections::HashSet<&str> =
            lock.lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.split('\t').next()).collect();
        assert!(locked.len() > 100, "the lock lists {} files", locked.len());
        for url in &locked {
            let rel = url.trim_start_matches("https://");
            assert!(Vendored::get(rel).is_some(), "{url} is in the lock but not embedded");
        }
    }
}
