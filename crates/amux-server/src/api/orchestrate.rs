//! POST /api/orchestrate/plan — the voice fleet-orchestrator's brain (AMUX-3074).
//!
//! A human speaks one command; it is transcribed by the existing /api/dictate,
//! and this endpoint turns the transcript into a ROUTING PLAN: which workers
//! should receive which messages. This is a pure composition of primitives —
//! dictation (transcript) + the fast helper model (the routing JUDGMENT) +
//! workers (the roster: names, groups, descriptions) + messages (the client
//! sends each plan entry through the existing send path). No new subsystem.
//!
//! Why the model and not keyword matching (ethos rule 2): choosing which worker
//! owns which slice of a spoken, run-on, homophone-ridden command IS judgment,
//! and it gets better as the helper model does. The endpoint only PLANS — it
//! never sends — so a mis-route is caught in the review step before any message
//! goes out, and every routed name is validated against the live roster so the
//! model can never invent a recipient.

use super::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::path::Path;

pub fn routes() -> Router<AppState> {
    Router::new().route("/plan", post(plan))
}

/// (name, groups, description) for every non-blocked, non-archived worker — read
/// from the same `~/.amux/sessions/*.env` files the fleet list uses, so the
/// router reasons over exactly the workers a human sees.
fn fleet_roster(home: &Path) -> Vec<(String, Vec<String>, String)> {
    let blocked = crate::api::groups::blocked_names(home);
    let Ok(entries) = std::fs::read_dir(home.join("sessions")) else {
        return vec![];
    };
    let mut rows: Vec<(String, Vec<String>, String)> = vec![];
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("env") {
            continue;
        }
        let Some(name) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };
        if blocked.contains(&name) {
            continue;
        }
        let env = crate::config::parse_env_file(&p);
        if env.get("CC_ARCHIVED").map(|v| v == "1").unwrap_or(false) {
            continue;
        }
        let groups: Vec<String> = env
            .get("CC_TAGS")
            .map(String::as_str)
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect();
        let desc = env.get("CC_DESC").cloned().unwrap_or_default();
        rows.push((name, groups, desc));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

fn build_prompt(roster: &[(String, Vec<String>, String)], transcript: &str) -> String {
    let mut roster_txt = String::new();
    for (name, groups, desc) in roster {
        let d = desc.trim();
        roster_txt.push_str(&format!(
            "- {name} [{}] — {}\n",
            groups.join(", "),
            if d.is_empty() { "(no description)" } else { d }
        ));
    }
    format!(
        "You are the amux fleet router. A human spoke a command and it was transcribed \
         roughly — expect dictation errors, run-ons, and homophones. Decide which WORKERS \
         should receive a message and what each message should say.\n\n\
         Rules:\n\
         - A command may fan out to several workers or target just one.\n\
         - Rewrite each worker's message as a clear, direct instruction to THAT worker \
           (imperative, second person), carrying ONLY the part of the command relevant to \
           it. Fix obvious transcription errors.\n\
         - Route by what each worker DOES (its description and groups), not by keyword \
           matching. Use EXACT worker names from the roster.\n\
         - If the command names a group, target the workers in that group.\n\
         - If nothing should be sent, return an empty array.\n\n\
         WORKERS:\n{roster_txt}\n\
         SPOKEN COMMAND:\n\"{transcript}\"\n\n\
         Return ONLY a JSON array, no prose and no markdown fence:\n\
         [{{\"worker\":\"<exact name>\",\"message\":\"<instruction to send>\",\"why\":\"<short reason>\"}}]"
    )
}

/// Pull the first balanced JSON array out of a model reply that may wrap it in
/// prose or a ```json fence. String-aware so a `]` inside a message value does
/// not close the array early.
fn extract_json_array(s: &str) -> Option<Value> {
    let trimmed = s.trim();
    if let Ok(v @ Value::Array(_)) = serde_json::from_str::<Value>(trimmed) {
        return Some(v);
    }
    let start = s.find('[')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, c) in s.char_indices().skip(start) {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    let cand = &s[start..=i];
                    return serde_json::from_str::<Value>(cand).ok().filter(Value::is_array);
                }
            }
            _ => {}
        }
    }
    None
}

pub async fn plan(State(_state): State<AppState>, Json(body): Json<Value>) -> Response {
    let transcript = body
        .get("transcript")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if transcript.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "transcript is required" })))
            .into_response();
    }
    let home = crate::api::groups::amux_home();
    let roster = fleet_roster(&home);
    if roster.is_empty() {
        return Json(json!({ "plan": [], "transcript": transcript, "note": "no workers available" }))
            .into_response();
    }
    let prompt = build_prompt(&roster, &transcript);
    let (via, answer) = match crate::api::lookup::helper_answer(&prompt).await {
        Ok(x) => x,
        Err((code, msg)) => {
            return (code, Json(json!({ "error": msg, "transcript": transcript }))).into_response()
        }
    };
    let Some(arr) = extract_json_array(&answer) else {
        // A router that cannot be parsed is a routing FAILURE, said plainly, with
        // the raw reply so the miss is diagnosable — never a silent empty plan
        // that reads as "nothing to route" (ethos rule 4).
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": "the router model did not return a parseable plan",
                "raw": answer.chars().take(1200).collect::<String>(),
                "via": via,
                "transcript": transcript,
            })),
        )
            .into_response();
    };
    // Validate every routed name against the live roster: the model may never
    // invent a recipient. Dropped names are REPORTED, not swallowed.
    let names: std::collections::HashSet<&str> = roster.iter().map(|(n, _, _)| n.as_str()).collect();
    let mut out_plan: Vec<Value> = vec![];
    let mut dropped: Vec<String> = vec![];
    for item in arr.as_array().into_iter().flatten() {
        let w = item.get("worker").and_then(Value::as_str).unwrap_or("").trim();
        let m = item.get("message").and_then(Value::as_str).unwrap_or("").trim();
        if w.is_empty() || m.is_empty() {
            continue;
        }
        if names.contains(w) {
            out_plan.push(json!({
                "worker": w,
                "message": m,
                "why": item.get("why").and_then(Value::as_str).unwrap_or(""),
            }));
        } else {
            dropped.push(w.to_string());
        }
    }
    Json(json!({
        "plan": out_plan,
        "transcript": transcript,
        "via": via,
        "dropped_unknown_workers": dropped,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_array_handles_fences_prose_and_nested_brackets() {
        // Bare array.
        assert!(extract_json_array(r#"[{"worker":"a","message":"go"}]"#).is_some());
        // Fenced + prose around it.
        let fenced = "Here is the plan:\n```json\n[{\"worker\":\"a\",\"message\":\"do [x]\"}]\n```\nDone.";
        let v = extract_json_array(fenced).expect("must find the fenced array");
        assert_eq!(v.as_array().unwrap().len(), 1);
        // A ']' inside a string value must not close the array early.
        let v = extract_json_array(r#"[{"worker":"a","message":"finish item [2]"}]"#).unwrap();
        assert_eq!(v[0]["message"], json!("finish item [2]"));
        // No array at all.
        assert!(extract_json_array("no json here").is_none());
    }

    #[test]
    fn build_prompt_lists_workers_with_groups_and_desc() {
        let roster = vec![
            ("backend".into(), vec!["ops".into()], "The backend API".into()),
            ("gtm".into(), vec![], "".into()),
        ];
        let p = build_prompt(&roster, "ship the thing");
        assert!(p.contains("- backend [ops] — The backend API"));
        assert!(p.contains("- gtm [] — (no description)"), "{p}");
        assert!(p.contains("ship the thing"));
    }
}
