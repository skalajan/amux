//! Every tmux `-t` target in the crate must come from the exact-match L2
//! helpers (`session_target` / `pane_target`, reached as `st`/`pt`/`stq`/`ptq`).
//!
//! This is a SOURCE audit rather than a behavioural test on purpose: the
//! failure it guards cannot be reproduced on demand. tmux resolves a bare `-t
//! foo` by PREFIX, and `amux-amux` is a prefix of `amux-amux-frustrations`,
//! `amux-amux-rust`, `amux-amux-cloud` and five more. A non-exact target is
//! therefore correct every single time the exact session exists, and silently
//! addresses a SIBLING's pane only in the window where it does not — which is
//! precisely a restart, a rename, or a start/stop race. The 2026-08-09
//! `amux-frustrations.log` carried another session's launch command and a
//! third session's nudge text from exactly such a window (AMUX-1888 is the
//! same hazard class in the CLI).
//!
//! So the check is: you cannot merge a hand-spelled target. If you need a new
//! target shape, add it to the helpers in `backend/tmux.rs` and it is covered
//! everywhere at once.

use std::path::{Path, PathBuf};

/// Identifiers that are, by construction, `session_target()`/`pane_target()`
/// output. Deliberately a SHORT closed list — the point is that the set of
/// ways to name a pane stays small enough to audit by eye.
const ALLOWED: &[&str] = &["st", "pt", "stq", "ptq"];

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            // tests/ may legitimately build literal targets for throwaway
            // sessions it created itself.
            if p.file_name().and_then(|s| s.to_str()) == Some("tests") {
                continue;
            }
            rust_sources(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// The expression following a `"-t",` in an argv array, normalised: leading
/// `&`, and a trailing `.as_str()` / `.clone()` accessor, are not part of the
/// identity of the value.
fn normalise(expr: &str) -> String {
    let e = expr.trim().trim_start_matches('&').trim();
    let e = e.split('.').next().unwrap_or(e);
    e.trim().to_string()
}

fn offenders() -> Vec<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&root, &mut files);
    assert!(!files.is_empty(), "found no sources to audit under {}", root.display());

    let mut bad = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap_or_default();
        let mut from = 0usize;
        while let Some(rel) = src[from..].find("\"-t\"") {
            let at = from + rel;
            from = at + 4;
            // Skip the separator after the literal, then take the argument up
            // to the next `,` / `]` / `)` at this nesting level.
            let rest = &src[from..];
            let Some(comma) = rest.find(',') else { continue };
            let tail = &rest[comma + 1..];
            let mut depth = 0i32;
            let mut end = tail.len();
            for (i, c) in tail.char_indices() {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => {
                        if depth == 0 {
                            end = i;
                            break;
                        }
                        depth -= 1;
                    }
                    ',' if depth == 0 => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            let expr = normalise(&tail[..end]);
            if ALLOWED.contains(&expr.as_str()) {
                continue;
            }
            let line = src[..at].matches('\n').count() + 1;
            bad.push(format!(
                "{}:{line}: tmux -t target `{expr}` is not one of {ALLOWED:?} \
                 (build it with session_target()/pane_target())",
                f.display()
            ));
        }
    }
    bad
}

#[test]
fn every_tmux_target_uses_the_exact_match_helpers() {
    let bad = offenders();
    assert!(
        bad.is_empty(),
        "hand-spelled tmux target(s) found — a non-exact `-t` lands in a \
         SIBLING session's pane whenever the exact session is briefly absent:\n{}",
        bad.join("\n")
    );
}

/// The audit is only worth having if it can fail, and a source-scanning check
/// is exactly the kind that silently matches nothing after a refactor renames
/// something (ethos rule 7 — "can your check actually fail?"). So prove the
/// scanner FINDS a planted offender rather than trusting that it would.
#[test]
fn the_audit_detects_a_planted_non_exact_target() {
    // Same text shape the scanner walks in a real source file, including the
    // prefix-matching target that motivated the rule.
    let planted = r#"
        let _ = tmux(&["pipe-pane", "-t", &format!("amux-{name}"), &cmd]).await;
        let _ = tmux(&["send-keys", "-t", "amux-amux", "Enter"]).await;
        let _ = tmux(&["kill-session", "-t", &stq]).await;
    "#;
    let mut found = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = planted[from..].find("\"-t\"") {
        let at = from + rel;
        from = at + 4;
        let rest = &planted[from..];
        let Some(comma) = rest.find(',') else { continue };
        let tail = &rest[comma + 1..];
        let mut depth = 0i32;
        let mut end = tail.len();
        for (i, c) in tail.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    if depth == 0 {
                        end = i;
                        break;
                    }
                    depth -= 1;
                }
                ',' if depth == 0 => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        let expr = normalise(&tail[..end]);
        if !ALLOWED.contains(&expr.as_str()) {
            found.push(expr);
        }
    }
    assert_eq!(
        found.len(),
        2,
        "the scanner must flag BOTH planted offenders and leave `stq` alone; got {found:?}"
    );
    assert!(found.iter().any(|f| f.contains("format!")), "missed the format! target: {found:?}");
    assert!(found.iter().any(|f| f.contains("\"amux-amux\"")), "missed the literal prefix target: {found:?}");
}
