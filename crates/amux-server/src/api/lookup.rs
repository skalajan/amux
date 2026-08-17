//! `POST /api/lookup` — "explain this selection" from the peek view.
//!
//! Ethan selected text in a pane, hit Look up, and got:
//!
//! ```text
//! Lookup failed: Failed to execute 'json' on 'Response': Unexpected end of JSON input
//! ```
//!
//! The endpoint was never ported from Python. Nothing was mounted at
//! `/api/lookup`, so the GET-only SPA catch-all answered the POST with 405 and
//! a ZERO-BYTE body, and the client's unconditional `r.json()` threw a parser
//! error that names neither the path nor the status.
//!
//! It WAS logged, and correctly: `GET /api/logs/analyze` had it as
//! `405 POST /api/lookup, count=10` with the verdict already written —
//! "no route exists at this path — the 405 is the GET-only SPA catch-all
//! answering a non-GET; treat as an unknown path (404-class)". The
//! instrumentation did its job; nobody was reading it. (Ethos rule 4: the tag
//! existed, the reader never opened the store.)
//!
//! # Deviations from the Python original, stated rather than hidden
//!
//! Python rode the peeked session's PROVIDER (gemini/ollama/codex) and fell
//! back to claude. This port always uses the configured helper CLI. That is a
//! real reduction for a gemini-only or ollama-only setup, and it is called out
//! here rather than discovered later — porting the provider-riding needs the
//! per-provider argv table, which does not exist in the Rust tree yet.
//!
//! The model is NOT pinned. `AMUX_HELPER_MODEL` selects it and an unset value
//! means "whatever the CLI defaults to", so this improves when the CLI does
//! (D3: a hardcoded weak-model helper is a bet that cannot improve).

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::AppState;

const MAX_TEXT: usize = 2000;
const LOOKUP_TIMEOUT_S: u64 = 45;

fn prompt_for(text: &str) -> String {
    format!(
        "Briefly explain what this means or refers to in 2-4 sentences. Be concise and \
         direct. If it's a technical term, code, error, or concept, explain it. If it's a \
         name, identify it.\n\n{text}"
    )
}


/// The local ollama endpoint, if one is running. `AMUX_OLLAMA_URL` overrides.
fn ollama_url() -> String {
    std::env::var("AMUX_OLLAMA_URL")
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:11434".into())
}

/// How long ollama should keep the weights resident. This is the whole
/// difference between a 3s lookup and a 19s one; the default (5m) evicts the
/// model between uses on a machine doing anything else.
fn ollama_keep_alive() -> String {
    std::env::var("AMUX_OLLAMA_KEEP_ALIVE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "30m".into())
}

/// SMALLEST model wins, because "fastest and cheapest" is what was asked for and
/// size is the only proxy for it that ollama reports without running anything.
/// Returns None when ollama is not running or has nothing pulled — both of which
/// are ordinary, and neither of which should cost the caller more than the
/// connect timeout.
async fn smallest_local_model(client: &reqwest::Client) -> Option<String> {
    let v: Value = client
        .get(format!("{}/api/tags", ollama_url()))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let mut models: Vec<(u64, String)> = v["models"]
        .as_array()?
        .iter()
        .filter_map(|m| {
            Some((
                m["size"].as_u64().unwrap_or(u64::MAX),
                m["name"].as_str()?.to_string(),
            ))
        })
        .collect();
    models.sort();
    models.into_iter().next().map(|(_, n)| n)
}

/// Ask a resident local model. Returns (model, answer) or None to fall through
/// to the CLI — never an error, because "no local model" is not a failure of the
/// lookup, it is a machine without one.
async fn try_local_model(prompt: &str) -> Option<(String, String)> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(LOOKUP_TIMEOUT_S))
        .build()
        .ok()?;
    let model = smallest_local_model(&client).await?;
    let v: Value = client
        .post(format!("{}/api/generate", ollama_url()))
        .json(&json!({
            "model": model,
            "prompt": prompt,
            "stream": false,
            "keep_alive": ollama_keep_alive(),
        }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let answer = v["response"].as_str()?.trim().to_string();
    if answer.is_empty() {
        return None; // fall through rather than serve an empty explanation
    }
    Some((model, answer))
}

/// Fastest, cheapest one-shot answer for a fully-formed prompt: a resident LOCAL
/// model when no `AMUX_HELPER_MODEL` is pinned, else the helper CLI (D3 — the one
/// knob still wins). This is the ONE place the "fastest cheapest model" seam
/// lives, shared by `/api/lookup` (explain-selection) and
/// `/api/sessions/<n>/simple` (the plain-English worker summary) so it cannot
/// drift into two spellings that must be kept in step forever (D6). Returns
/// `(via, text)` — `via` is `ollama:<model>` or the CLI name — or an HTTP
/// status + message. The caller supplies the WHOLE prompt (this does not wrap
/// it), so each caller keeps its own instruction.
pub(crate) async fn helper_answer(prompt: &str) -> Result<(String, String), (StatusCode, String)> {
    if std::env::var("AMUX_HELPER_MODEL").map(|m| m.trim().is_empty()).unwrap_or(true) {
        if let Some((model, answer)) = try_local_model(prompt).await {
            return Ok((format!("ollama:{model}"), answer));
        }
    }
    let cli = std::env::var("AMUX_HELPER_CLI").unwrap_or_else(|_| "claude".into());
    let mut cmd = tokio::process::Command::new(&cli);
    cmd.arg("--print").arg(prompt);
    if let Ok(m) = std::env::var("AMUX_HELPER_MODEL") {
        if !m.trim().is_empty() {
            cmd.arg("--model").arg(m.trim());
        }
    }
    cmd.stdin(std::process::Stdio::null());
    match tokio::time::timeout(std::time::Duration::from_secs(LOOKUP_TIMEOUT_S), cmd.output()).await {
        Err(_) => Err((
            StatusCode::GATEWAY_TIMEOUT,
            format!("{cli} did not answer within {LOOKUP_TIMEOUT_S}s"),
        )),
        Ok(Err(e)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not run {cli}: {e}"),
        )),
        Ok(Ok(out)) => {
            // Non-empty stdout wins even on a non-zero exit (the CLI prints its
            // answer then sometimes exits non-zero); only an EMPTY answer is a
            // failure — the whole lookup incident was a zero-byte body.
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if stdout.is_empty() {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    if err.is_empty() {
                        format!("{cli} exited without output")
                    } else {
                        err.chars().take(400).collect()
                    },
                ));
            }
            Ok((cli, stdout))
        }
    }
}

pub async fn lookup(
    State(_state): State<AppState>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let text: String = body
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .chars()
        .take(MAX_TEXT)
        .collect();
    if text.is_empty() {
        // A JSON body even on refusal. The whole incident was a zero-byte
        // response meeting an unconditional r.json(), so every exit from this
        // handler carries a parseable body.
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing text"})),
        );
    }

    // Fastest, cheapest, on this machine — a resident LOCAL model first, else
    // the CLI. That whole decision (and the measured 3s-ollama vs 7-24s-CLI
    // rationale, and the `AMUX_HELPER_MODEL` D3 override) now lives in ONE place,
    // `helper_answer`, shared with the Simple worker-summary endpoint (D6). The
    // client reads only `text`.
    match helper_answer(&prompt_for(&text)).await {
        Ok((via, answer)) => (StatusCode::OK, Json(json!({"text": answer, "via": via}))),
        Err((code, msg)) => (code, Json(json!({"error": msg}))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_carries_the_selection_verbatim() {
        let p = prompt_for("SIGPIPE");
        assert!(p.contains("SIGPIPE"));
        assert!(p.contains("2-4 sentences"), "the brevity instruction is the point");
    }

    /// The selection comes from a terminal pane, so it can be enormous. The cap
    /// is applied to CHARS, not bytes — slicing bytes would panic on a
    /// multi-byte boundary, which a pane full of box-drawing characters
    /// reliably produces.
    #[test]
    fn an_enormous_multibyte_selection_is_capped_without_panicking() {
        let huge: String = "├─╢".repeat(5000);
        let capped: String = huge.trim().chars().take(MAX_TEXT).collect();
        assert_eq!(capped.chars().count(), MAX_TEXT);
    }
}
