//! Gmail REST integration (RR-0088), ported from the Python server's
//! `_gmail_*` helpers — same token files, same request shapes, same response
//! fields, so the two servers are interchangeable against one
//! `~/.amux/gmail-tokens/` directory.
//!
//! Ported flows:
//! - token refresh: `POST https://oauth2.googleapis.com/token` with the
//!   stored refresh token; the refreshed access token is written back to the
//!   token file in Python's exact JSON shape.
//! - send: `users/messages/send` with a base64url-encoded RFC822
//!   multipart/alternative message (text + HTML + the account's real Gmail
//!   send-as signature, fetched from `users/settings/sendAs`).
//! - reply: the EXACT `_gmail_reply_send` header logic — In-Reply-To /
//!   References chaining, account-local threadId only when the message
//!   lives in the sending account, external-recipient resolution that can
//!   never email ourselves without `allow_self`. The Python docstrings warn
//!   that a naive reply sends BLANK emails; this port never touches
//!   AppleScript, so that failure class is structurally absent — but the
//!   recipient/threading derivation is kept byte-comparable.
//! - inbox/search: `users/messages/list` + per-message metadata gets, with
//!   the authoritative `after:`/`newer_than:` date filters (AMUX-1886) and
//!   the `truncated` marker so a caller can tell a real 0 from a capped
//!   scan.
//!
//! NO credential values live in this file or its tests. Tokens come from
//! `<home>/gmail-tokens/<email>.json` at runtime; tests use temp dirs with
//! synthetic placeholder strings.
//!
//! All HTTP goes through the [`HttpTransport`] trait so tests mock the wire
//! and never touch the network.

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const GMAIL_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
pub const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// Python's own-domain exemption list (`_gmail_reply_send` / the new-thread
/// guard), ported verbatim.
pub const OUR_DOMAINS: [&str; 4] =
    ["mixpeek.com", "trymixpeek.com", "joinmixpeek.com", "getmixpeek.com"];

// ---------------------------------------------------------------------------
// base64 (std + urlsafe) — implemented here because the workspace forbids new
// deps; ~40 lines beats pulling a crate for two alphabets.
// ---------------------------------------------------------------------------

const B64_STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64_encode(data: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(alphabet[(n >> 18) as usize & 63] as char);
        out.push(alphabet[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(alphabet[(n >> 6) as usize & 63] as char);
        } else if pad {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(alphabet[n as usize & 63] as char);
        } else if pad {
            out.push('=');
        }
    }
    out
}

/// Python `base64.urlsafe_b64encode` (keeps padding) — the shape Gmail's
/// `raw` field expects.
pub fn base64url(data: &[u8]) -> String {
    b64_encode(data, B64_URL, true)
}

/// `secrets.token_urlsafe`-style: urlsafe, padding stripped.
pub fn base64url_nopad(data: &[u8]) -> String {
    b64_encode(data, B64_URL, false)
}

/// Standard base64 with padding (MIME body encoding).
pub fn base64_std(data: &[u8]) -> String {
    b64_encode(data, B64_STD, true)
}

/// Decode urlsafe base64 (padding optional). Used by tests to open the
/// `raw` payload and by future message-body reads.
pub fn base64url_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' | b'\r' | b'\n' => continue,
            _ => return Err(format!("invalid base64 byte {c}")),
        };
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

/// Wrap a base64 body at 76 chars (RFC 2045) with CRLF line ends.
fn wrap76(s: &str) -> String {
    s.as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).expect("base64 is ascii"))
        .collect::<Vec<_>>()
        .join("\r\n")
}

// ---------------------------------------------------------------------------
// Small ports of Python stdlib behavior the flows depend on
// ---------------------------------------------------------------------------

/// Python `html.escape(s)` with quote=True (the default used by
/// `_gmail_compose_send`).
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Python `_sig_html_to_text`: best-effort plain-text rendering of an HTML
/// signature — keeps line breaks, drops tags/images.
pub fn sig_html_to_text(sig_html: &str) -> String {
    if sig_html.is_empty() {
        return String::new();
    }
    let br = regex::Regex::new(r"(?i)<\s*br\s*/?>").expect("static regex");
    let blocks = regex::Regex::new(r"(?i)</\s*(p|div|tr|table|h[1-6])\s*>").expect("static regex");
    let tags = regex::Regex::new(r"<[^>]+>").expect("static regex");
    let trail_ws = regex::Regex::new(r"[ \t]+\n").expect("static regex");
    let many_nl = regex::Regex::new(r"\n{3,}").expect("static regex");
    let mut t = br.replace_all(sig_html, "\n").into_owned();
    t = blocks.replace_all(&t, "\n").into_owned();
    t = tags.replace_all(&t, "").into_owned();
    t = html_unescape(&t);
    t = trail_ws.replace_all(&t, "\n").into_owned();
    t = many_nl.replace_all(&t, "\n\n").into_owned();
    t.trim().to_string()
}

/// The handful of entities `html.unescape` sees in real signatures.
fn html_unescape(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Python `email.utils.getaddresses` — enough of it for real From/To/Cc
/// headers: quoted display names may contain commas; the address is the
/// `<...>` payload when present, else the bare token.
pub fn parse_addresses(hdr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut in_angle = false;
    for ch in hdr.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                cur.push(ch);
            }
            '<' if !in_quotes => {
                in_angle = true;
                cur.push(ch);
            }
            '>' if !in_quotes => {
                in_angle = false;
                cur.push(ch);
            }
            ',' if !in_quotes && !in_angle => {
                push_addr(&mut out, &cur);
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    push_addr(&mut out, &cur);
    out
}

fn push_addr(out: &mut Vec<String>, token: &str) {
    let t = token.trim();
    if t.is_empty() {
        return;
    }
    let addr = match (t.rfind('<'), t.rfind('>')) {
        (Some(a), Some(b)) if b > a => &t[a + 1..b],
        _ => t,
    };
    let addr = addr.trim();
    if !addr.is_empty() {
        out.push(addr.to_string());
    }
}

/// Percent-encode a query-string value (RFC 3986 unreserved set kept).
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// RFC 2047 encoded-word for non-ASCII header values (what Python's email
/// lib does implicitly for Subject etc.). ASCII passes through verbatim.
pub fn encode_header_value(s: &str) -> String {
    if s.is_ascii() {
        s.to_string()
    } else {
        format!("=?utf-8?b?{}?=", base64_std(s.as_bytes()))
    }
}

// ---------------------------------------------------------------------------
// RFC822 assembly (Python `_gmail_compose_send`'s MIME construction)
// ---------------------------------------------------------------------------

/// Everything needed to assemble one outgoing message. `boundary` is
/// injected so tests pin the full RFC822 output.
pub struct MimeSpec<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub cc: &'a str,
    pub subject: &'a str,
    pub in_reply_to: &'a str,
    pub references: &'a str,
    pub plain: &'a str,
    pub html: &'a str,
    pub boundary: &'a str,
}

/// Build the multipart/alternative RFC822 message (CRLF line ends, base64
/// body parts). Threading headers appear iff `in_reply_to` is non-empty,
/// mirroring Python: References defaults to In-Reply-To when unset.
pub fn build_rfc822(spec: &MimeSpec) -> String {
    let mut lines: Vec<String> = vec![
        "MIME-Version: 1.0".into(),
        format!("Content-Type: multipart/alternative; boundary=\"{}\"", spec.boundary),
        format!("To: {}", spec.to),
        format!("From: {}", spec.from),
        format!("Subject: {}", encode_header_value(spec.subject)),
    ];
    if !spec.cc.is_empty() {
        lines.push(format!("Cc: {}", spec.cc));
    }
    if !spec.in_reply_to.is_empty() {
        lines.push(format!("In-Reply-To: {}", spec.in_reply_to));
        let refs = if spec.references.is_empty() { spec.in_reply_to } else { spec.references };
        lines.push(format!("References: {refs}"));
    }
    lines.push(String::new());
    for (ctype, body) in
        [("text/plain", spec.plain), ("text/html", spec.html)]
    {
        lines.push(format!("--{}", spec.boundary));
        lines.push(format!("Content-Type: {ctype}; charset=\"utf-8\""));
        lines.push("MIME-Version: 1.0".into());
        lines.push("Content-Transfer-Encoding: base64".into());
        lines.push(String::new());
        lines.push(wrap76(&base64_std(body.as_bytes())));
    }
    lines.push(format!("--{}--", spec.boundary));
    lines.push(String::new());
    lines.join("\r\n")
}

/// Python `_gmail_compose_send`'s body derivation: escaped-HTML rendering of
/// the plain body, signature appended to both alternatives. Returns
/// `(plain_full, html_full, signature_included)`.
pub fn compose_bodies(body: &str, sig_html: &str) -> (String, String, bool) {
    // The HTML alternative must MIRROR the text/plain part, which Gmail renders
    // only as a fallback (primis, 2026-08-13: a delivered reply came out cramped
    // — paragraph spacing collapsed, bullet list flattened). The old
    // `white-space:normal` COLLAPSED the sender's whitespace (indentation, blank
    // lines) and `\n -> <br>` discarded structure. `white-space:pre-wrap` on the
    // ESCAPED body — newlines KEPT, not turned into <br> — renders exactly the
    // spacing the text/plain part has: blank-line paragraph breaks and indented
    // list items survive. (Rich markdown -> <ul>/<strong> is a separate opt-in;
    // this faithful-whitespace fix is the safe default and cannot misrender.)
    let body_html = html_escape(body);
    let mut html_full = format!("<div style=\"white-space:pre-wrap;\">{body_html}</div>");
    if !sig_html.is_empty() {
        html_full.push_str(&format!("<br><br>{sig_html}"));
    }
    let sig_text = sig_html_to_text(sig_html);
    let plain_full =
        if sig_text.is_empty() { body.to_string() } else { format!("{body}\n\n{sig_text}") };
    (plain_full, html_full, !sig_html.is_empty())
}

// ---------------------------------------------------------------------------
// Reply derivation (Python `_gmail_reply_send`, the pure part)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ReplyPlan {
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub in_reply_to: String,
    pub references: String,
}

/// Derive recipients + threading headers for a reply, given the original
/// message's headers (LOWERCASE keys). Ported exactly, including:
/// - the reply goes to the EXTERNAL party, never one of our own accounts
///   (the reply-to-self bug fix);
/// - `allow_self` is the sanctioned threading self-test between owned
///   accounts (AMUX-1739 follow-up);
/// - the error string when no recipient survives is Python's, verbatim.
pub fn derive_reply_plan(
    headers: &HashMap<String, String>,
    fallback_msgid: &str,
    account: &str,
    connected: &[String],
    reply_all: bool,
    allow_self: bool,
) -> Result<ReplyPlan, String> {
    let hdr = |k: &str| headers.get(k).map(String::as_str).unwrap_or("");
    let mut orig_msgid = if hdr("message-id").is_empty() {
        fallback_msgid.to_string()
    } else {
        hdr("message-id").to_string()
    };
    if !orig_msgid.starts_with('<') {
        // Python: prepends '<' only (no trailing '>' fixup) — kept identical
        // so both servers send byte-identical headers for the same input.
        orig_msgid = format!("<{orig_msgid}>");
    }
    let subject_raw = hdr("subject");
    let subject = if subject_raw.to_lowercase().starts_with("re:") {
        subject_raw.to_string()
    } else {
        format!("Re: {subject_raw}")
    };

    let connected_lc: Vec<String> = connected
        .iter()
        .map(|a| a.to_lowercase())
        .chain(std::iter::once(account.to_lowercase()))
        .collect();
    let external = |h: &str| -> Vec<String> {
        parse_addresses(h)
            .into_iter()
            .filter(|a| {
                let al = a.to_lowercase();
                !(connected_lc.contains(&al)
                    || OUR_DOMAINS.iter().any(|d| al.ends_with(&format!("@{d}"))))
            })
            .collect()
    };
    let from_ext = external(hdr("from"));
    let to_ext = external(hdr("to"));
    let cc_ext = external(hdr("cc"));

    let (mut to_addr, cc) = if reply_all {
        let mut all_ext: Vec<String> = Vec::new();
        for a in from_ext.iter().chain(&to_ext).chain(&cc_ext) {
            if !all_ext.contains(a) {
                all_ext.push(a.clone());
            }
        }
        (all_ext.join(", "), String::new())
    } else {
        // The party who last spoke (From) if external, else the original
        // recipient(s).
        let pick = if from_ext.is_empty() { &to_ext } else { &from_ext };
        (pick.join(", "), String::new())
    };
    if to_addr.is_empty() && allow_self {
        let not_me = |h: &str| -> Vec<String> {
            parse_addresses(h)
                .into_iter()
                .filter(|a| a.to_lowercase() != account.to_lowercase())
                .collect()
        };
        let raw_from = not_me(hdr("from"));
        let raw_to = not_me(hdr("to"));
        to_addr = if raw_from.is_empty() { raw_to.join(", ") } else { raw_from.join(", ") };
    }
    if to_addr.is_empty() {
        // NEVER fall back to emailing ourselves (without explicit allow_self).
        return Err("no external recipient on this thread (would email ourselves) — \
                    pass an explicit 'to' via /api/email/send, or use \
                    {\"allow_self\": true} to run a threading self-test between owned accounts"
            .into());
    }
    let references = format!("{} {}", hdr("references"), orig_msgid).trim().to_string();
    Ok(ReplyPlan { to: to_addr, cc, subject, in_reply_to: orig_msgid, references })
}

// ---------------------------------------------------------------------------
// HTTP transport seam
// ---------------------------------------------------------------------------

#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn get(&self, url: &str, bearer: Option<&str>) -> Result<(u16, Value), String>;
    async fn post_json(
        &self,
        url: &str,
        bearer: Option<&str>,
        body: &Value,
    ) -> Result<(u16, Value), String>;
    async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
    ) -> Result<(u16, Value), String>;
}

/// Production transport (reqwest, 30s timeout — the Python side bounds each
/// account at 20s so one wedged token cannot hang the unified inbox).
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

async fn to_pair(res: reqwest::Response) -> Result<(u16, Value), String> {
    let status = res.status().as_u16();
    let text = res.text().await.map_err(|e| e.to_string())?;
    let v = serde_json::from_str(&text).unwrap_or(Value::String(text));
    Ok((status, v))
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn get(&self, url: &str, bearer: Option<&str>) -> Result<(u16, Value), String> {
        let mut req = self.client.get(url);
        if let Some(t) = bearer {
            req = req.bearer_auth(t);
        }
        to_pair(req.send().await.map_err(|e| e.to_string())?).await
    }
    async fn post_json(
        &self,
        url: &str,
        bearer: Option<&str>,
        body: &Value,
    ) -> Result<(u16, Value), String> {
        let mut req = self.client.post(url).json(body);
        if let Some(t) = bearer {
            req = req.bearer_auth(t);
        }
        to_pair(req.send().await.map_err(|e| e.to_string())?).await
    }
    async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
    ) -> Result<(u16, Value), String> {
        let req = self.client.post(url).form(form);
        to_pair(req.send().await.map_err(|e| e.to_string())?).await
    }
}

// ---------------------------------------------------------------------------
// Token files (~/.amux/gmail-tokens/<email>.json)
// ---------------------------------------------------------------------------

pub fn default_amux_home() -> PathBuf {
    std::env::var("AMUX_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/")).join(".amux")
    })
}

/// Accounts with stored tokens (Python `_gmail_connected_accounts`): sorted
/// stems of `<home>/gmail-tokens/*.json`.
pub fn connected_accounts_in(home: &Path) -> Vec<String> {
    let dir = home.join("gmail-tokens");
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            (p.extension().and_then(|x| x.to_str()) == Some("json"))
                .then(|| p.file_stem()?.to_str().map(String::from))
                .flatten()
        })
        .collect();
    out.sort();
    out
}

#[derive(Debug, Clone, Default)]
struct TokenFile {
    token: Option<String>,
    refresh_token: Option<String>,
    token_uri: String,
    client_id: String,
    client_secret: String,
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

pub struct GmailClient {
    http: Arc<dyn HttpTransport>,
    home: PathBuf,
    /// account -> live access token (in-memory; the file may hold a stale
    /// one, which the 401-retry path replaces).
    token_cache: Mutex<HashMap<String, String>>,
}

impl GmailClient {
    pub fn new(http: Arc<dyn HttpTransport>, home: PathBuf) -> Self {
        Self { http, home, token_cache: Mutex::new(HashMap::new()) }
    }

    pub fn new_default() -> Self {
        Self::new(Arc::new(ReqwestTransport::new()), default_amux_home())
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn connected_accounts(&self) -> Vec<String> {
        connected_accounts_in(&self.home)
    }

    fn token_path(&self, account: &str) -> PathBuf {
        self.home.join("gmail-tokens").join(format!("{account}.json"))
    }

    /// Load the token file, merging client id/secret from
    /// `gmail-oauth-client.json` (`installed` or `web`) when the token file
    /// lacks them — Python `_gmail_load_creds`.
    fn load_token_file(&self, account: &str) -> Option<TokenFile> {
        let data: Value =
            serde_json::from_str(&std::fs::read_to_string(self.token_path(account)).ok()?).ok()?;
        let s = |k: &str| data.get(k).and_then(Value::as_str).map(String::from);
        let mut tf = TokenFile {
            token: s("token").filter(|t| !t.is_empty()),
            refresh_token: s("refresh_token").filter(|t| !t.is_empty()),
            token_uri: s("token_uri").unwrap_or_else(|| DEFAULT_TOKEN_URI.into()),
            client_id: s("client_id").unwrap_or_default(),
            client_secret: s("client_secret").unwrap_or_default(),
        };
        if tf.client_id.is_empty() || tf.client_secret.is_empty() {
            if let Ok(raw) = std::fs::read_to_string(self.home.join("gmail-oauth-client.json")) {
                if let Ok(cfg) = serde_json::from_str::<Value>(&raw) {
                    let node = cfg.get("installed").or_else(|| cfg.get("web"));
                    if let Some(n) = node {
                        if tf.client_id.is_empty() {
                            tf.client_id = n
                                .get("client_id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                        }
                        if tf.client_secret.is_empty() {
                            tf.client_secret = n
                                .get("client_secret")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                        }
                    }
                }
            }
        }
        Some(tf)
    }

    /// Current access token; `force_refresh` bypasses cache + stored token
    /// (the 401-retry path). Persists the refreshed token in Python's exact
    /// file shape so both servers keep working off one file.
    async fn access_token(&self, account: &str, force_refresh: bool) -> Result<String, String> {
        if !force_refresh {
            if let Some(t) = self.token_cache.lock().expect("token cache").get(account) {
                return Ok(t.clone());
            }
        }
        let tf = self.load_token_file(account).ok_or_else(|| "not_connected".to_string())?;
        if !force_refresh {
            if let Some(t) = &tf.token {
                self.token_cache.lock().expect("token cache").insert(account.into(), t.clone());
                return Ok(t.clone());
            }
        }
        let refresh = tf
            .refresh_token
            .clone()
            .ok_or_else(|| "not_connected (no refresh_token stored)".to_string())?;
        let form = vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), refresh.clone()),
            ("client_id".to_string(), tf.client_id.clone()),
            ("client_secret".to_string(), tf.client_secret.clone()),
        ];
        let (status, body) = self.http.post_form(&tf.token_uri, &form).await?;
        if status >= 400 {
            // invalid_grant (revoked/expired) must stay visible in the error
            // text — it is the discriminator between "re-auth needed" and
            // "not connected" (the 2026-08-07 wrong-probe incident).
            return Err(format!("token refresh failed ({status}): {body}"));
        }
        let access = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("token refresh response missing access_token: {body}"))?
            .to_string();
        self.token_cache.lock().expect("token cache").insert(account.into(), access.clone());
        // Best-effort persist, Python's exact shape (a failed write must not
        // fail the send).
        let persisted = json!({
            "token": access,
            "refresh_token": refresh,
            "token_uri": tf.token_uri,
            "client_id": tf.client_id,
            "client_secret": tf.client_secret,
        });
        let _ = std::fs::write(self.token_path(account), persisted.to_string());
        Ok(access)
    }

    /// One authenticated Gmail call with the 401-refresh-retry that
    /// google-auth's AuthorizedSession does implicitly for Python.
    async fn api(
        &self,
        account: &str,
        method: &str,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let mut token = self.access_token(account, false).await?;
        for attempt in 0..2 {
            let (status, v) = match (method, body) {
                ("GET", _) => self.http.get(url, Some(&token)).await?,
                ("POST", Some(b)) => self.http.post_json(url, Some(&token), b).await?,
                _ => return Err(format!("unsupported method {method}")),
            };
            if status == 401 && attempt == 0 {
                token = self.access_token(account, true).await?;
                continue;
            }
            if status >= 400 {
                return Err(format!("gmail api {status}: {v}"));
            }
            return Ok(v);
        }
        unreachable!("loop always returns");
    }

    fn metadata_url(&self, id: &str, headers: &[&str]) -> String {
        let hs: String = headers.iter().map(|h| format!("&metadataHeaders={h}")).collect();
        format!("{GMAIL_BASE}/messages/{id}?format=metadata{hs}")
    }

    async fn list_ids(
        &self,
        account: &str,
        q: &str,
        max_results: usize,
        page_token: Option<&str>,
    ) -> Result<Value, String> {
        let mut url = format!("{GMAIL_BASE}/messages?q={}&maxResults={max_results}", urlencode(q));
        if let Some(pt) = page_token {
            url.push_str(&format!("&pageToken={}", urlencode(pt)));
        }
        self.api(account, "GET", &url, None).await
    }

    /// Python `_gmail_get_signature`: the account's configured send-as HTML
    /// signature; "" on any failure.
    pub async fn get_signature(&self, account: &str) -> String {
        let url = format!("{GMAIL_BASE}/settings/sendAs");
        let Ok(v) = self.api(account, "GET", &url, None).await else { return String::new() };
        let empty = vec![];
        let sendas = v.get("sendAs").and_then(Value::as_array).unwrap_or(&empty);
        let target = account.to_lowercase();
        let chosen = sendas
            .iter()
            .find(|sa| {
                sa.get("sendAsEmail").and_then(Value::as_str).unwrap_or("").to_lowercase() == target
            })
            .or_else(|| {
                sendas.iter().find(|sa| sa.get("isPrimary").and_then(Value::as_bool) == Some(true))
            })
            .or_else(|| sendas.first());
        chosen
            .and_then(|sa| sa.get("signature"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// Python `_gmail_compose_send`: multipart/alternative with signature,
    /// optional threading headers + threadId. Returns
    /// `{ok, id, thread_id, signature_included}`.
    #[allow(clippy::too_many_arguments)]
    pub async fn compose_send(
        &self,
        account: &str,
        to: &str,
        subject: &str,
        body: &str,
        cc: &str,
        in_reply_to: &str,
        references: &str,
        thread_id: &str,
        include_signature: bool,
    ) -> Result<Value, String> {
        let sig_html =
            if include_signature { self.get_signature(account).await } else { String::new() };
        let (plain, html, sig_included) = compose_bodies(body, &sig_html);
        // Boundary needs no cryptographic strength, only absence from the
        // payload; nanos + a fixed tag matches Python's uniqueness level.
        let boundary = format!(
            "=_amux_{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let rfc822 = build_rfc822(&MimeSpec {
            from: account,
            to,
            cc,
            subject,
            in_reply_to,
            references,
            plain: &plain,
            html: &html,
            boundary: &boundary,
        });
        let mut send_body = Map::new();
        send_body.insert("raw".into(), json!(base64url(rfc822.as_bytes())));
        if !thread_id.is_empty() {
            send_body.insert("threadId".into(), json!(thread_id));
        }
        let url = format!("{GMAIL_BASE}/messages/send");
        let res = self.api(account, "POST", &url, Some(&Value::Object(send_body))).await?;
        Ok(json!({
            "ok": true,
            "id": res.get("id").cloned().unwrap_or(Value::Null),
            "thread_id": res.get("threadId").cloned().unwrap_or(Value::Null),
            "signature_included": sig_included,
        }))
    }

    /// Python `_gmail_find_message_by_rfc822`: indexed `rfc822msgid:` lookup
    /// -> metadata resource (or None).
    pub async fn find_message_by_rfc822(&self, account: &str, rfc822_id: &str) -> Option<Value> {
        let rid = rfc822_id.trim().trim_start_matches('<').trim_end_matches('>');
        let list = self.list_ids(account, &format!("rfc822msgid:{rid}"), 1, None).await.ok()?;
        let mid = list.get("messages")?.as_array()?.first()?.get("id")?.as_str()?.to_string();
        let url = self.metadata_url(
            &mid,
            &["From", "To", "Cc", "Subject", "Message-ID", "References", "In-Reply-To"],
        );
        self.api(account, "GET", &url, None).await.ok()
    }

    /// Python `_gmail_list_messages` (AMUX-2883): header-level summaries for
    /// the Mail view. `q` overrides `label` (python's exact precedence). The
    /// N metadata fetches after the id list are python's own shape — one
    /// round trip per message — kept for contract fidelity; a batch endpoint
    /// exists but changes error semantics per message.
    pub async fn list_messages(
        &self,
        account: &str,
        label: &str,
        page_token: &str,
        q: &str,
        max_results: usize,
    ) -> Value {
        let mut url = format!("{GMAIL_BASE}/messages?maxResults={max_results}");
        if !q.is_empty() {
            url.push_str(&format!("&q={}", urlencode(q)));
        } else {
            url.push_str(&format!("&labelIds={}", urlencode(label)));
        }
        if !page_token.is_empty() {
            url.push_str(&format!("&pageToken={}", urlencode(page_token)));
        }
        let listed = match self.api(account, "GET", &url, None).await {
            Ok(v) => v,
            Err(e) => return self.gmail_error_shape(account, &e),
        };
        let empty = vec![];
        let ids = listed.get("messages").and_then(Value::as_array).unwrap_or(&empty);
        let next = listed.get("nextPageToken").and_then(Value::as_str).unwrap_or("");
        let mut summaries = vec![];
        for m in ids {
            let Some(mid) = m.get("id").and_then(Value::as_str) else { continue };
            let murl = self.metadata_url(mid, &["From", "To", "Subject", "Date"]);
            let Ok(hdr) = self.api(account, "GET", &murl, None).await else { continue };
            let h = header_map(&hdr);
            let lids: Vec<&str> = hdr
                .get("labelIds")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            summaries.push(json!({
                "id": mid,
                "thread_id": hdr.get("threadId").and_then(Value::as_str).unwrap_or(mid),
                "from": h.get("from").cloned().unwrap_or_default(),
                "to": h.get("to").cloned().unwrap_or_default(),
                "subject": h.get("subject").cloned().unwrap_or_else(|| "(no subject)".into()),
                "date": h.get("date").cloned().unwrap_or_default(),
                "snippet": hdr.get("snippet").and_then(Value::as_str).unwrap_or(""),
                "unread": lids.contains(&"UNREAD"),
                "starred": lids.contains(&"STARRED"),
                "internal_date": internal_date(&hdr),
            }));
        }
        json!({ "messages": summaries, "next_page_token": next })
    }

    /// Python `_gmail_get_thread`: full thread with decoded bodies; every
    /// unread message in it is marked read (python's read-on-open behavior,
    /// best-effort — a failed modify never fails the read).
    pub async fn get_thread(&self, account: &str, thread_id: &str) -> Value {
        let url = format!("{GMAIL_BASE}/threads/{}?format=full", urlencode(thread_id));
        let thread = match self.api(account, "GET", &url, None).await {
            Ok(v) => v,
            Err(e) => return self.gmail_error_shape(account, &e),
        };
        let empty = vec![];
        let msgs = thread.get("messages").and_then(Value::as_array).unwrap_or(&empty);
        let mut out = vec![];
        let mut unread_ids = vec![];
        for msg in msgs {
            let payload = msg.get("payload").cloned().unwrap_or(json!({}));
            let h = header_map_of(&payload);
            let (html_body, text_body) = decode_body(&payload);
            let lids: Vec<&str> = msg
                .get("labelIds")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let mid = msg.get("id").and_then(Value::as_str).unwrap_or("");
            if lids.contains(&"UNREAD") {
                unread_ids.push(mid.to_string());
            }
            out.push(json!({
                "id": mid,
                "thread_id": msg.get("threadId").and_then(Value::as_str).unwrap_or(""),
                "from": h.get("from").cloned().unwrap_or_default(),
                "to": h.get("to").cloned().unwrap_or_default(),
                "cc": h.get("cc").cloned().unwrap_or_default(),
                "subject": h.get("subject").cloned().unwrap_or_else(|| "(no subject)".into()),
                "date": h.get("date").cloned().unwrap_or_default(),
                "message_id_header": h.get("message-id").cloned().unwrap_or_default(),
                "html_body": html_body,
                "text_body": text_body,
                "unread": lids.contains(&"UNREAD"),
                "labels": lids,
                "internal_date": internal_date(msg),
            }));
        }
        for uid in unread_ids {
            let murl = format!("{GMAIL_BASE}/messages/{uid}/modify");
            let _ = self
                .api(account, "POST", &murl, Some(&json!({"removeLabelIds": ["UNREAD"]})))
                .await;
        }
        json!({ "thread_id": thread_id, "messages": out })
    }

    /// Python `_gmail_list_labels`: system labels first (INBOX, STARRED,
    /// SENT, DRAFTS, SPAM, TRASH — python's exact order), then user labels
    /// alphabetically. `[]` on any failure (python parity — the Mail view
    /// renders an empty label rail rather than an error).
    pub async fn list_labels(&self, account: &str) -> Vec<Value> {
        let url = format!("{GMAIL_BASE}/labels");
        let Ok(v) = self.api(account, "GET", &url, None).await else { return vec![] };
        let mut labels: Vec<Value> =
            v.get("labels").and_then(Value::as_array).cloned().unwrap_or_default();
        const PRIO: [&str; 6] = ["INBOX", "STARRED", "SENT", "DRAFTS", "SPAM", "TRASH"];
        labels.sort_by_key(|l| {
            let n = l.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            match PRIO.iter().position(|p| *p == n) {
                Some(i) => (0, i.to_string()),
                None => (1, n.to_lowercase()),
            }
        });
        labels
    }

    /// Python's undifferentiated `{"error": str(e)}` sent readers to check
    /// whether the account was connected when it WAS — this is the improved
    /// shape python later grew for list_messages, applied to every entry
    /// point: name the state, and when it is re-authable, name the fix.
    fn gmail_error_shape(&self, account: &str, err: &str) -> Value {
        if err.contains("invalid_grant") {
            json!({
                "error": "needs_reauth",
                "account": account,
                "fix": format!("GET /api/gmail/auth?account={account} — open the url and approve"),
            })
        } else if err.contains("not_connected") {
            json!({ "error": "not_connected", "account": account })
        } else {
            json!({ "error": err, "account": account })
        }
    }

    /// Python `_gmail_reply_send`: reply in-thread (clean body + signature,
    /// correct In-Reply-To/References/threadId) to the message identified by
    /// its RFC822 Message-ID. Cross-account: a thread living on another
    /// connected account threads via headers, not the account-local
    /// threadId.
    pub async fn reply_send(
        &self,
        account: &str,
        rfc822_message_id: &str,
        body: &str,
        include_signature: bool,
        reply_all: bool,
        allow_self: bool,
    ) -> Result<Value, String> {
        let mut orig = self.find_message_by_rfc822(account, rfc822_message_id).await;
        let mut thread_account = account.to_string();
        if orig.is_none() {
            for acct in self.connected_accounts() {
                if acct == account {
                    continue;
                }
                if let Some(found) = self.find_message_by_rfc822(&acct, rfc822_message_id).await {
                    orig = Some(found);
                    thread_account = acct;
                    break;
                }
            }
        }
        let Some(orig) = orig else {
            return Err("message not found in any connected account — check message_id".into());
        };
        let headers = header_map(&orig);
        // threadId is account-local; only valid if the message lives in the
        // SENDING account.
        let orig_thread_id =
            orig.get("threadId").and_then(Value::as_str).unwrap_or("").to_string();
        let thread_id =
            if thread_account == account { orig_thread_id.clone() } else { String::new() };
        let connected = self.connected_accounts();
        let plan = derive_reply_plan(
            &headers,
            rfc822_message_id,
            account,
            &connected,
            reply_all,
            allow_self,
        )?;
        let mut res = self
            .compose_send(
                account,
                &plan.to,
                &plan.subject,
                body,
                &plan.cc,
                &plan.in_reply_to,
                &plan.references,
                &thread_id,
                include_signature,
            )
            .await?;
        // Threading proof for the caller (assertable evidence, ethos rule 4).
        let threaded = !thread_id.is_empty()
            && res.get("thread_id").and_then(Value::as_str) == Some(thread_id.as_str());
        res["orig_thread_id"] =
            json!(if thread_id.is_empty() { orig_thread_id } else { thread_id });
        res["threaded"] = json!(threaded);
        Ok(res)
    }

    /// Python `_gmail_inbox_messages` minus the 30s cache: unified inbox
    /// shape with the RFC822 Message-ID as `message_id` so it round-trips
    /// into /reply. `days` is AUTHORITATIVE when set (an `after:` filter,
    /// AMUX-1886); `truncated` distinguishes a real 0 from a capped slice.
    pub async fn inbox_messages(
        &self,
        account: &str,
        count: usize,
        q: &str,
        days: f64,
    ) -> Result<Value, String> {
        let want = count.max(1);
        let mut query = q.to_string();
        if query.is_empty() {
            query = "in:inbox".into();
            if days > 0.0 {
                let after = chrono::Utc::now().timestamp() - (days * 86400.0) as i64;
                query.push_str(&format!(" after:{after}"));
            }
        }
        let mut ids: Vec<Value> = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            if ids.len() >= want {
                break;
            }
            let resp = self
                .list_ids(account, &query, (want - ids.len()).min(100), page_token.as_deref())
                .await?;
            if let Some(msgs) = resp.get("messages").and_then(Value::as_array) {
                ids.extend(msgs.iter().cloned());
            }
            page_token =
                resp.get("nextPageToken").and_then(Value::as_str).map(String::from);
            if page_token.is_none() {
                break;
            }
        }
        let truncated = page_token.is_some() && ids.len() >= want;
        ids.truncate(want);
        let mut out = Vec::with_capacity(ids.len());
        for m in &ids {
            let Some(mid) = m.get("id").and_then(Value::as_str) else { continue };
            let url =
                self.metadata_url(mid, &["From", "To", "Subject", "Date", "Message-ID"]);
            let Ok(full) = self.api(account, "GET", &url, None).await else { continue };
            let h = header_map(&full);
            let hv = |k: &str| h.get(k).cloned().unwrap_or_default();
            let unread = full
                .get("labelIds")
                .and_then(Value::as_array)
                .map(|l| l.iter().any(|x| x.as_str() == Some("UNREAD")))
                .unwrap_or(false);
            out.push(json!({
                "account": account,
                "from": hv("from"),
                "to": hv("to"),
                "date": hv("date"),
                "subject": if hv("subject").is_empty() { "(no subject)".to_string() } else { hv("subject") },
                "message_id": hv("message-id"),
                "thread_id": full.get("threadId").cloned().unwrap_or(json!("")),
                "gmail_id": mid,
                "read": !unread,
                "body": full.get("snippet").cloned().unwrap_or(json!("")),
            }));
        }
        Ok(json!({ "messages": out, "truncated": truncated }))
    }

    /// Python `_gmail_latest_matching`: resolve "the latest message
    /// from/with X" to its RFC822 Message-ID + meta. Fail-open (None) on
    /// any API error — callers use this as a guard, not a gate.
    pub async fn latest_matching(
        &self,
        account: &str,
        from_addr: &str,
        with_addr: &str,
        subject_contains: &str,
        newer_days: i64,
    ) -> Option<Value> {
        let mut q = if !from_addr.is_empty() {
            format!("from:{from_addr}")
        } else if !with_addr.is_empty() {
            format!("(from:{with_addr} OR to:{with_addr})")
        } else {
            return None;
        };
        if newer_days > 0 {
            q.push_str(&format!(" newer_than:{newer_days}d"));
        }
        let res = self.list_ids(account, &q, 10, None).await.ok()?;
        for m in res.get("messages")?.as_array()? {
            let mid = m.get("id")?.as_str()?;
            let url =
                self.metadata_url(mid, &["From", "To", "Subject", "Message-ID", "Date"]);
            let Ok(meta) = self.api(account, "GET", &url, None).await else { continue };
            let h = header_map(&meta);
            let hv = |k: &str| h.get(k).cloned().unwrap_or_default();
            if !subject_contains.is_empty()
                && !hv("subject").to_lowercase().contains(&subject_contains.to_lowercase())
            {
                continue;
            }
            return Some(json!({
                "message_id": hv("message-id"),
                "subject": hv("subject"),
                "from": hv("from"),
                "to": hv("to"),
                "date": hv("date"),
                "gmail_message_id": mid,
                "thread_id": meta.get("threadId").cloned().unwrap_or(json!("")),
                "account": account,
            }));
        }
        None
    }
}

/// Lowercased header-name -> value map from a Gmail message resource.
fn header_map(msg: &Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(hs) = msg.pointer("/payload/headers").and_then(Value::as_array) {
        for h in hs {
            if let (Some(n), Some(v)) =
                (h.get("name").and_then(Value::as_str), h.get("value").and_then(Value::as_str))
            {
                out.insert(n.to_lowercase(), v.to_string());
            }
        }
    }
    out
}

/// Same map from a bare PAYLOAD object (a thread's per-message payloads have
/// no `/payload` prefix — `header_map` above reads a full message resource).
fn header_map_of(payload: &Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(hs) = payload.get("headers").and_then(Value::as_array) {
        for h in hs {
            if let (Some(n), Some(v)) =
                (h.get("name").and_then(Value::as_str), h.get("value").and_then(Value::as_str))
            {
                out.insert(n.to_lowercase(), v.to_string());
            }
        }
    }
    out
}

/// Gmail sends `internalDate` as a STRING of epoch millis; python coerced
/// with int(). Accept both encodings.
fn internal_date(msg: &Value) -> i64 {
    match msg.get("internalDate") {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// Python `_gmail_decode_body`: recursively extract (html, text) from a
/// message payload — first text/html and first text/plain part win.
fn decode_body(payload: &Value) -> (String, String) {
    let mime = payload.get("mimeType").and_then(Value::as_str).unwrap_or("");
    let data = |p: &Value| -> String {
        let d = p.pointer("/body/data").and_then(Value::as_str).unwrap_or("");
        if d.is_empty() {
            return String::new();
        }
        base64url_decode(d)
            .map(|b| String::from_utf8_lossy(&b).to_string())
            .unwrap_or_default()
    };
    if mime == "text/html" {
        return (data(payload), String::new());
    }
    if mime == "text/plain" {
        return (String::new(), data(payload));
    }
    let mut html = String::new();
    let mut text = String::new();
    if let Some(parts) = payload.get("parts").and_then(Value::as_array) {
        for part in parts {
            let (h, t) = decode_body(part);
            if !h.is_empty() && html.is_empty() {
                html = h;
            }
            if !t.is_empty() && text.is_empty() {
                text = t;
            }
        }
    }
    (html, text)
}

// ---------------------------------------------------------------------------
// Send-audit ledger (Python `_email_log` / GET /api/email/log, AMUX-1897)
// ---------------------------------------------------------------------------

pub fn email_log_path(home: &Path) -> PathBuf {
    home.join("logs").join("email-sent.jsonl")
}

/// Append one JSON audit record per SENT email. Fail-safe: a logging error
/// must never break or block a send (Python parity). Only sends through
/// this API appear here.
pub fn email_log(home: &Path, mut record: Value) {
    let mut write = || -> std::io::Result<()> {
        if record.get("ts").is_none() {
            record["ts"] = json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true));
        }
        let p = email_log_path(home);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
        writeln!(f, "{record}")?;
        Ok(())
    };
    let _ = write();
}

/// Read the ledger (GET /api/email/log): `days` window, `limit` cap,
/// optional session filter (`unattributed` matches records sent without the
/// header). Response shape identical to Python.
pub fn read_email_log(home: &Path, days: i64, limit: usize, session_filter: &str) -> Value {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(days))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let mut out: Vec<Value> = Vec::new();
    if let Ok(content) = std::fs::read_to_string(email_log_path(home)) {
        for line in content.lines() {
            let Ok(mut rec) = serde_json::from_str::<Value>(line) else { continue };
            let ts = rec.get("ts").and_then(Value::as_str).unwrap_or("");
            if ts < cutoff.as_str() {
                continue;
            }
            let rsess = match rec.get("session").and_then(Value::as_str) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => "unattributed".to_string(),
            };
            if !session_filter.is_empty() && rsess != session_filter {
                continue;
            }
            rec["session"] = json!(rsess);
            out.push(rec);
        }
    }
    let keep: Vec<Value> = out.iter().rev().take(limit).cloned().collect();
    json!({ "count": keep.len(), "days": days, "log": keep })
}

// ---------------------------------------------------------------------------
// Tests — fixtures + mocked transport only, no network, no live files.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        // RFC 4648 vectors.
        assert_eq!(base64_std(b""), "");
        assert_eq!(base64_std(b"f"), "Zg==");
        assert_eq!(base64_std(b"fo"), "Zm8=");
        assert_eq!(base64_std(b"foo"), "Zm9v");
        assert_eq!(base64_std(b"foobar"), "Zm9vYmFy");
        // urlsafe alphabet: 0xfb 0xff -> "-_8=" (would be "+/8=" in std).
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8=");
        assert_eq!(base64url_nopad(&[0xfb, 0xff]), "-_8");
        // round trip
        let data = b"amux \xf0\x9f\x93\xa7 bytes";
        assert_eq!(base64url_decode(&base64url(data)).unwrap(), data.to_vec());
    }

    #[test]
    fn html_escape_matches_python_quote_true() {
        assert_eq!(html_escape(r#"<a href="x">&'b'</a>"#), "&lt;a href=&quot;x&quot;&gt;&amp;&#x27;b&#x27;&lt;/a&gt;");
    }

    #[test]
    fn sig_html_to_text_keeps_breaks_drops_tags() {
        let sig = "<div><b>Ethan</b><br>Founder, Mixpeek</div><p>ethan&amp;co</p>";
        assert_eq!(sig_html_to_text(sig), "Ethan\nFounder, Mixpeek\nethan&co");
        assert_eq!(sig_html_to_text(""), "");
    }

    #[test]
    fn address_parsing_handles_quoted_names_and_lists() {
        assert_eq!(
            parse_addresses(r#""Howard, Mark" <mhoward@lucihub.com>, jane@x.co, Bob <bob@y.z>"#),
            vec!["mhoward@lucihub.com", "jane@x.co", "bob@y.z"]
        );
        assert_eq!(parse_addresses(""), Vec::<String>::new());
    }

    #[test]
    fn compose_bodies_escapes_and_appends_signature() {
        let (plain, html, inc) = compose_bodies("hi <b>\nline2", "<b>Sig</b>");
        assert_eq!(plain, "hi <b>\nline2\n\nSig");
        // pre-wrap + KEEP the newline (not <br>): the HTML mirrors the plain
        // part's spacing exactly (BACKE/primis 2026-08-13). A `<br>` here would
        // DOUBLE the break under pre-wrap.
        assert_eq!(
            html,
            "<div style=\"white-space:pre-wrap;\">hi &lt;b&gt;\nline2</div><br><br><b>Sig</b>"
        );
        assert!(html.contains("pre-wrap") && !html.contains("<br>line2"), "newline preserved, not <br>-ified: {html}");
        assert!(inc);
        let (plain2, html2, inc2) = compose_bodies("hi", "");
        assert_eq!(plain2, "hi");
        assert_eq!(html2, "<div style=\"white-space:pre-wrap;\">hi</div>");
        assert!(!inc2);
    }

    #[test]
    fn rfc822_fixture_with_threading_headers() {
        let spec = MimeSpec {
            from: "sender@example.com",
            to: "rcpt@example.org",
            cc: "cc@example.org",
            subject: "Hello",
            in_reply_to: "<orig@id>",
            references: "<root@id> <orig@id>",
            plain: "plain body",
            html: "<div>plain body</div>",
            boundary: "=_amux_test",
        };
        let msg = build_rfc822(&spec);
        let want = concat!(
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/alternative; boundary=\"=_amux_test\"\r\n",
            "To: rcpt@example.org\r\n",
            "From: sender@example.com\r\n",
            "Subject: Hello\r\n",
            "Cc: cc@example.org\r\n",
            "In-Reply-To: <orig@id>\r\n",
            "References: <root@id> <orig@id>\r\n",
            "\r\n",
            "--=_amux_test\r\n",
            "Content-Type: text/plain; charset=\"utf-8\"\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "cGxhaW4gYm9keQ==\r\n",
            "--=_amux_test\r\n",
            "Content-Type: text/html; charset=\"utf-8\"\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "PGRpdj5wbGFpbiBib2R5PC9kaXY+\r\n",
            "--=_amux_test--\r\n",
        );
        assert_eq!(msg, want);
    }

    #[test]
    fn rfc822_new_message_has_no_threading_headers() {
        let spec = MimeSpec {
            from: "a@b.c",
            to: "d@e.f",
            cc: "",
            subject: "S",
            in_reply_to: "",
            references: "",
            plain: "p",
            html: "h",
            boundary: "=_b",
        };
        let msg = build_rfc822(&spec);
        assert!(!msg.contains("In-Reply-To"));
        assert!(!msg.contains("References"));
        assert!(!msg.contains("Cc:"));
    }

    #[test]
    fn rfc822_references_defaults_to_in_reply_to() {
        let spec = MimeSpec {
            from: "a@b.c",
            to: "d@e.f",
            cc: "",
            subject: "S",
            in_reply_to: "<only@id>",
            references: "",
            plain: "p",
            html: "h",
            boundary: "=_b",
        };
        let msg = build_rfc822(&spec);
        assert!(msg.contains("References: <only@id>\r\n"), "{msg}");
    }

    #[test]
    fn non_ascii_subject_is_rfc2047_encoded() {
        assert_eq!(encode_header_value("plain"), "plain");
        let enc = encode_header_value("héllo");
        assert!(enc.starts_with("=?utf-8?b?") && enc.ends_with("?="), "{enc}");
        assert_eq!(base64url_decode(&enc[10..enc.len() - 2]).unwrap(), "héllo".as_bytes());
    }

    // ---- reply derivation -------------------------------------------------

    fn hdrs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    const ME: &str = "owner@mixpeek.com";
    fn connected() -> Vec<String> {
        vec!["owner@mixpeek.com".into(), "info@mixpeek.com".into()]
    }

    #[test]
    fn reply_targets_external_sender_and_chains_references() {
        let h = hdrs(&[
            ("message-id", "<orig@ext>"),
            ("subject", "Deal"),
            ("from", "Prospect <p@customer.com>"),
            ("to", "owner@mixpeek.com"),
            ("references", "<root@ext>"),
        ]);
        let plan = derive_reply_plan(&h, "<orig@ext>", ME, &connected(), false, false).unwrap();
        assert_eq!(plan.to, "p@customer.com");
        assert_eq!(plan.subject, "Re: Deal");
        assert_eq!(plan.in_reply_to, "<orig@ext>");
        assert_eq!(plan.references, "<root@ext> <orig@ext>");
        assert_eq!(plan.cc, "");
    }

    #[test]
    fn reply_to_own_sent_message_goes_to_original_recipients() {
        // The reply-to-self bug: replying to our OWN sent message must
        // target the external To, not us.
        let h = hdrs(&[
            ("message-id", "<sent@us>"),
            ("subject", "Re: Deal"),
            ("from", "Us <owner@mixpeek.com>"),
            ("to", "p@customer.com, other@customer.com"),
        ]);
        let plan = derive_reply_plan(&h, "<sent@us>", ME, &connected(), false, false).unwrap();
        assert_eq!(plan.to, "p@customer.com, other@customer.com");
        // subject already Re: — not doubled.
        assert_eq!(plan.subject, "Re: Deal");
    }

    #[test]
    fn reply_all_dedupes_and_excludes_all_owned() {
        let h = hdrs(&[
            ("message-id", "<m@x>"),
            ("subject", "s"),
            ("from", "a@ext.com"),
            ("to", "owner@mixpeek.com, b@ext.com, a@ext.com"),
            ("cc", "info@mixpeek.com, c@ext.com, teammate@trymixpeek.com"),
        ]);
        let plan = derive_reply_plan(&h, "<m@x>", ME, &connected(), true, false).unwrap();
        assert_eq!(plan.to, "a@ext.com, b@ext.com, c@ext.com");
    }

    #[test]
    fn no_external_recipient_is_a_hard_error_with_pythons_message() {
        let h = hdrs(&[
            ("message-id", "<m@x>"),
            ("subject", "s"),
            ("from", "owner@mixpeek.com"),
            ("to", "info@mixpeek.com"),
        ]);
        let err = derive_reply_plan(&h, "<m@x>", ME, &connected(), false, false).unwrap_err();
        assert!(err.starts_with("no external recipient on this thread (would email ourselves)"), "{err}");
        assert!(err.contains("allow_self"), "{err}");
    }

    #[test]
    fn allow_self_enables_owned_account_threading_test() {
        let h = hdrs(&[
            ("message-id", "<m@x>"),
            ("subject", "s"),
            ("from", "info@mixpeek.com"),
            ("to", "owner@mixpeek.com"),
        ]);
        let plan = derive_reply_plan(&h, "<m@x>", ME, &connected(), false, true).unwrap();
        assert_eq!(plan.to, "info@mixpeek.com"); // the other owned account, never the sender
    }

    #[test]
    fn missing_angle_bracket_gets_pythons_exact_fixup() {
        let h = hdrs(&[("subject", "s"), ("from", "x@ext.com"), ("message-id", "bare@id>")]);
        let plan = derive_reply_plan(&h, "bare@id>", ME, &connected(), false, false).unwrap();
        // Python wraps unconditionally when no leading '<' (amux-server.py:26957
        // f"<{id}>"), so a trailing '>' doubles. Parity beats prettiness: the
        // doubled form is what the Python server puts on the wire today.
        assert_eq!(plan.in_reply_to, "<bare@id>>");
    }

    // ---- mocked transport: token refresh + API flows ----------------------

    /// Scripted transport: matches on URL substrings, records every call.
    struct MockHttp {
        calls: Mutex<Vec<(String, String, Option<Value>)>>,
        /// (method, url_substring, status, response) — first match wins,
        /// single-use entries pop so a retry can get a different answer.
        script: Mutex<Vec<(String, String, u16, Value)>>,
    }

    impl MockHttp {
        fn new(script: Vec<(&str, &str, u16, Value)>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                script: Mutex::new(
                    script
                        .into_iter()
                        .map(|(m, u, s, v)| (m.to_string(), u.to_string(), s, v))
                        .collect(),
                ),
            })
        }
        fn answer(&self, method: &str, url: &str, body: Option<&Value>) -> Result<(u16, Value), String> {
            self.calls.lock().unwrap().push((method.into(), url.into(), body.cloned()));
            let mut script = self.script.lock().unwrap();
            if let Some(pos) = script
                .iter()
                .position(|(m, sub, _, _)| m == method && url.contains(sub.as_str()))
            {
                let (_, _, status, v) = script.remove(pos);
                return Ok((status, v));
            }
            Err(format!("mock has no answer for {method} {url}"))
        }
    }

    #[async_trait]
    impl HttpTransport for MockHttp {
        async fn get(&self, url: &str, _bearer: Option<&str>) -> Result<(u16, Value), String> {
            self.answer("GET", url, None)
        }
        async fn post_json(
            &self,
            url: &str,
            _bearer: Option<&str>,
            body: &Value,
        ) -> Result<(u16, Value), String> {
            self.answer("POST", url, Some(body))
        }
        async fn post_form(
            &self,
            url: &str,
            form: &[(String, String)],
        ) -> Result<(u16, Value), String> {
            let v = Value::Object(
                form.iter().map(|(k, val)| (k.clone(), json!(val))).collect(),
            );
            self.answer("FORM", url, Some(&v))
        }
    }

    /// Temp home with a synthetic token file. PLACEHOLDER strings only — no
    /// credential values anywhere in tests.
    fn temp_home_with_token(account: &str, with_access_token: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("gmail-tokens");
        std::fs::create_dir_all(&tokens).unwrap();
        let mut tf = json!({
            "refresh_token": "PLACEHOLDER_REFRESH",
            "token_uri": "https://oauth2.googleapis.com/token",
            "client_id": "PLACEHOLDER_CLIENT_ID",
            "client_secret": "PLACEHOLDER_CLIENT_SECRET",
        });
        if with_access_token {
            tf["token"] = json!("PLACEHOLDER_STALE_ACCESS");
        }
        std::fs::write(tokens.join(format!("{account}.json")), tf.to_string()).unwrap();
        dir
    }

    #[tokio::test]
    async fn token_refresh_posts_correct_form_and_persists() {
        let home = temp_home_with_token("acct@example.com", false);
        let http = MockHttp::new(vec![
            ("FORM", "oauth2.googleapis.com/token", 200, json!({ "access_token": "FRESH", "expires_in": 3599 })),
            ("GET", "/settings/sendAs", 200, json!({ "sendAs": [] })),
        ]);
        let client = GmailClient::new(http.clone(), home.path().to_path_buf());
        // get_signature forces one authenticated call -> refresh first.
        let sig = client.get_signature("acct@example.com").await;
        assert_eq!(sig, "");
        let calls = http.calls.lock().unwrap();
        let (_, _, form) = &calls[0];
        let form = form.as_ref().unwrap();
        assert_eq!(form["grant_type"], json!("refresh_token"));
        assert_eq!(form["refresh_token"], json!("PLACEHOLDER_REFRESH"));
        assert_eq!(form["client_id"], json!("PLACEHOLDER_CLIENT_ID"));
        assert_eq!(form["client_secret"], json!("PLACEHOLDER_CLIENT_SECRET"));
        // Refreshed token persisted back in Python's file shape.
        let persisted: Value = serde_json::from_str(
            &std::fs::read_to_string(
                home.path().join("gmail-tokens").join("acct@example.com.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(persisted["token"], json!("FRESH"));
        assert_eq!(persisted["refresh_token"], json!("PLACEHOLDER_REFRESH"));
    }

    #[tokio::test]
    async fn stale_access_token_is_refreshed_on_401_and_retried() {
        let home = temp_home_with_token("acct@example.com", true);
        let http = MockHttp::new(vec![
            // First API call uses the stale stored token -> 401.
            ("POST", "/messages/send", 401, json!({ "error": { "code": 401 } })),
            ("FORM", "oauth2.googleapis.com/token", 200, json!({ "access_token": "FRESH2" })),
            // Retry succeeds.
            ("POST", "/messages/send", 200, json!({ "id": "m1", "threadId": "t1" })),
        ]);
        let client = GmailClient::new(http.clone(), home.path().to_path_buf());
        let res = client
            .compose_send("acct@example.com", "x@ext.com", "S", "B", "", "", "", "", false)
            .await
            .unwrap();
        assert_eq!(res["ok"], json!(true));
        assert_eq!(res["id"], json!("m1"));
        assert_eq!(res["thread_id"], json!("t1"));
        assert_eq!(res["signature_included"], json!(false));
        // Exactly one refresh happened, after the 401.
        let calls = http.calls.lock().unwrap();
        let kinds: Vec<&str> = calls.iter().map(|(m, _, _)| m.as_str()).collect();
        assert_eq!(kinds, vec!["POST", "FORM", "POST"]);
    }

    #[tokio::test]
    async fn invalid_grant_surfaces_in_the_error_text() {
        let home = temp_home_with_token("acct@example.com", false);
        let http = MockHttp::new(vec![(
            "FORM",
            "oauth2.googleapis.com/token",
            400,
            json!({ "error": "invalid_grant", "error_description": "Token has been revoked." }),
        )]);
        let client = GmailClient::new(http, home.path().to_path_buf());
        let err = client
            .compose_send("acct@example.com", "x@ext.com", "S", "B", "", "", "", "", false)
            .await
            .unwrap_err();
        assert!(err.contains("invalid_grant"), "{err}");
    }

    #[tokio::test]
    async fn compose_send_raw_decodes_to_rfc822_with_signature_and_thread() {
        let home = temp_home_with_token("acct@example.com", true);
        let http = MockHttp::new(vec![
            ("GET", "/settings/sendAs", 200, json!({ "sendAs": [
                { "sendAsEmail": "acct@example.com", "signature": "<b>Sig</b>" },
            ]})),
            ("POST", "/messages/send", 200, json!({ "id": "m2", "threadId": "T9" })),
        ]);
        let client = GmailClient::new(http.clone(), home.path().to_path_buf());
        let res = client
            .compose_send(
                "acct@example.com",
                "x@ext.com",
                "Subj",
                "hello",
                "c@ext.com",
                "<orig@id>",
                "<root@id> <orig@id>",
                "T9",
                true,
            )
            .await
            .unwrap();
        assert_eq!(res["signature_included"], json!(true));
        let calls = http.calls.lock().unwrap();
        let (_, _, body) = calls.iter().find(|(m, u, _)| m == "POST" && u.contains("/messages/send")).unwrap();
        let body = body.as_ref().unwrap();
        assert_eq!(body["threadId"], json!("T9"));
        let raw = body["raw"].as_str().unwrap();
        let decoded = String::from_utf8(base64url_decode(raw).unwrap()).unwrap();
        assert!(decoded.contains("To: x@ext.com\r\n"), "{decoded}");
        assert!(decoded.contains("Cc: c@ext.com\r\n"));
        assert!(decoded.contains("In-Reply-To: <orig@id>\r\n"));
        assert!(decoded.contains("References: <root@id> <orig@id>\r\n"));
        // Signature reached both alternatives (encoded in base64 parts).
        assert!(decoded.contains(&wrap76(&base64_std("hello\n\nSig".as_bytes()))));
        assert!(decoded.contains(&wrap76(&base64_std(
            "<div style=\"white-space:pre-wrap;\">hello</div><br><br><b>Sig</b>".as_bytes()
        ))));
    }

    #[tokio::test]
    async fn reply_send_threads_in_account_and_reports_proof() {
        let home = temp_home_with_token("acct@example.com", true);
        let orig = json!({
            "id": "g1", "threadId": "T1",
            "payload": { "headers": [
                { "name": "Message-ID", "value": "<orig@ext>" },
                { "name": "Subject", "value": "Deal" },
                { "name": "From", "value": "P <p@customer.com>" },
                { "name": "To", "value": "acct@example.com" },
                { "name": "References", "value": "<root@ext>" },
            ]},
        });
        let http = MockHttp::new(vec![
            ("GET", "q=rfc822msgid", 200, json!({ "messages": [{ "id": "g1" }] })),
            ("GET", "/messages/g1", 200, orig),
            ("POST", "/messages/send", 200, json!({ "id": "m3", "threadId": "T1" })),
        ]);
        let client = GmailClient::new(http.clone(), home.path().to_path_buf());
        let res = client
            .reply_send("acct@example.com", "<orig@ext>", "thanks!", false, false, false)
            .await
            .unwrap();
        assert_eq!(res["threaded"], json!(true));
        assert_eq!(res["orig_thread_id"], json!("T1"));
        let calls = http.calls.lock().unwrap();
        let (_, _, body) = calls.iter().find(|(m, u, _)| m == "POST" && u.contains("/messages/send")).unwrap();
        let body = body.as_ref().unwrap();
        assert_eq!(body["threadId"], json!("T1"));
        let decoded =
            String::from_utf8(base64url_decode(body["raw"].as_str().unwrap()).unwrap()).unwrap();
        assert!(decoded.contains("To: p@customer.com\r\n"), "{decoded}");
        assert!(decoded.contains("Subject: Re: Deal\r\n"));
        assert!(decoded.contains("In-Reply-To: <orig@ext>\r\n"));
        assert!(decoded.contains("References: <root@ext> <orig@ext>\r\n"));
    }

    #[tokio::test]
    async fn reply_send_missing_message_is_pythons_error() {
        let home = temp_home_with_token("acct@example.com", true);
        let http = MockHttp::new(vec![(
            "GET",
            "q=rfc822msgid",
            200,
            json!({ "messages": [] }),
        )]);
        let client = GmailClient::new(http, home.path().to_path_buf());
        let err = client
            .reply_send("acct@example.com", "<gone@id>", "b", false, false, false)
            .await
            .unwrap_err();
        assert_eq!(err, "message not found in any connected account — check message_id");
    }

    #[tokio::test]
    async fn inbox_builds_authoritative_after_query_and_marks_truncation() {
        let home = temp_home_with_token("acct@example.com", true);
        let meta = |mid: &str, msgid: &str| {
            json!({
                "id": mid, "threadId": "T", "snippet": "snip", "labelIds": ["INBOX", "UNREAD"],
                "payload": { "headers": [
                    { "name": "From", "value": "a@ext.com" },
                    { "name": "To", "value": "acct@example.com" },
                    { "name": "Subject", "value": "s" },
                    { "name": "Date", "value": "Sun, 09 Aug 2026 10:00:00 -0400" },
                    { "name": "Message-ID", "value": msgid },
                ]},
            })
        };
        let http = MockHttp::new(vec![
            ("GET", "/messages?q=", 200, json!({
                "messages": [{ "id": "g1" }, { "id": "g2" }],
                "nextPageToken": "more",
            })),
            ("GET", "/messages/g1", 200, meta("g1", "<m1@x>")),
            ("GET", "/messages/g2", 200, meta("g2", "<m2@x>")),
        ]);
        let client = GmailClient::new(http.clone(), home.path().to_path_buf());
        let res = client.inbox_messages("acct@example.com", 2, "", 3.0).await.unwrap();
        // Window held more than the cap: a caller can tell 0 from capped.
        assert_eq!(res["truncated"], json!(true));
        let msgs = res["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["message_id"], json!("<m1@x>"));
        assert_eq!(msgs[0]["read"], json!(false));
        assert_eq!(msgs[0]["body"], json!("snip"));
        assert_eq!(msgs[0]["account"], json!("acct@example.com"));
        // The list call carried in:inbox + an epoch after: filter.
        let calls = http.calls.lock().unwrap();
        let url = &calls.iter().find(|(m, u, _)| m == "GET" && u.contains("/messages?q=")).unwrap().1;
        assert!(url.contains(&urlencode("in:inbox after:")[..20]), "{url}");
    }

    #[tokio::test]
    async fn latest_matching_filters_by_subject_and_fails_open() {
        let home = temp_home_with_token("acct@example.com", true);
        let meta = json!({
            "id": "g9", "threadId": "T7",
            "payload": { "headers": [
                { "name": "From", "value": "ceo@customer.com" },
                { "name": "To", "value": "acct@example.com" },
                { "name": "Subject", "value": "Pilot kickoff" },
                { "name": "Message-ID", "value": "<pilot@x>" },
                { "name": "Date", "value": "Sun, 09 Aug 2026 10:00:00 -0400" },
            ]},
        });
        let http = MockHttp::new(vec![
            ("GET", "/messages?q=", 200, json!({ "messages": [{ "id": "g9" }] })),
            ("GET", "/messages/g9", 200, meta),
        ]);
        let client = GmailClient::new(http.clone(), home.path().to_path_buf());
        let hit = client
            .latest_matching("acct@example.com", "", "ceo@customer.com", "pilot", 14)
            .await
            .unwrap();
        assert_eq!(hit["message_id"], json!("<pilot@x>"));
        assert_eq!(hit["thread_id"], json!("T7"));
        // ONE guard, values CLONED out before the next await: `&lock()[i]`
        // extends the temporary guard to end of scope — a second lock()
        // self-deadlocks and a held guard across an await blocks the runtime.
        let (url, list_url) = {
            let calls = http.calls.lock().unwrap();
            (calls[1].1.clone(), calls[0].1.clone())
        };
        assert!(list_url.contains(&urlencode("(from:ceo@customer.com OR to:ceo@customer.com) newer_than:14d")), "{list_url} {url}");
        // API error -> None (fail-open guard, never a gate).
        let http2 = MockHttp::new(vec![("GET", "/messages?q=", 500, json!({ "error": "boom" }))]);
        let client2 = GmailClient::new(http2, home.path().to_path_buf());
        assert!(client2.latest_matching("acct@example.com", "x@y.z", "", "", 14).await.is_none());
    }

    // ---- send-audit ledger ------------------------------------------------

    #[test]
    fn email_log_appends_and_reads_with_session_filter() {
        let dir = tempfile::tempdir().unwrap();
        email_log(
            dir.path(),
            json!({ "endpoint": "send", "via": "gmail", "from": "a@b.c", "to": "x@y.z",
                    "subject": "s", "session": "sess-1" }),
        );
        email_log(
            dir.path(),
            json!({ "endpoint": "reply", "via": "gmail", "from": "a@b.c",
                    "in_reply_to": "<m@x>", "session": null }),
        );
        let all = read_email_log(dir.path(), 7, 50, "");
        assert_eq!(all["count"], json!(2));
        // Newest first (Python: out[-limit:][::-1]).
        assert_eq!(all["log"][0]["endpoint"], json!("reply"));
        assert_eq!(all["log"][0]["session"], json!("unattributed"));
        let mine = read_email_log(dir.path(), 7, 50, "sess-1");
        assert_eq!(mine["count"], json!(1));
        assert_eq!(mine["log"][0]["session"], json!("sess-1"));
        let unattr = read_email_log(dir.path(), 7, 50, "unattributed");
        assert_eq!(unattr["count"], json!(1));
        assert_eq!(unattr["log"][0]["endpoint"], json!("reply"));
    }

    #[test]
    fn connected_accounts_lists_token_stems_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("gmail-tokens");
        std::fs::create_dir_all(&tokens).unwrap();
        std::fs::write(tokens.join("b@x.com.json"), "{}").unwrap();
        std::fs::write(tokens.join("a@x.com.json"), "{}").unwrap();
        std::fs::write(tokens.join("notes.txt"), "").unwrap();
        assert_eq!(connected_accounts_in(dir.path()), vec!["a@x.com", "b@x.com"]);
        assert!(connected_accounts_in(&dir.path().join("missing")).is_empty());
    }
}
