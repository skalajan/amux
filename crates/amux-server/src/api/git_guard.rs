//! `POST /api/git/staged-guard` — the shared-checkout staged-state guard.
//!
//! # Why this file exists at all
//!
//! The endpoint is called by `.git/hooks/amux-staged-guard`, a script the
//! Python server generated per work_dir and which is therefore **installed on
//! machines and checkouts this repo cannot see**. Python is retired; the
//! generator went with it; the hooks did not. So the contract below is not a
//! design — it is a transcription of what the installed clients already speak,
//! recovered from `git show 792ce1f^:amux-server.py` (`_staged_guard_check`
//! py:19400, `_staged_guard_window` py:19187, route py:71813) and from the
//! generated hook on disk. **The server adapts to the hooks, never the
//! reverse.**
//!
//! Between the rust cutover and this file, `POST /api/git/staged-guard`
//! answered 405 — 1,147 times an hour — and the hook's `except Exception:
//! return 0  # fail open` swallowed every one of them. The guard printed
//! nothing and exited 0 on every commit, fleet-wide, which is precisely the
//! silent-permissiveness failure it exists to prevent (it was already
//! silently skipped once before, AC-261). That is the reason this module
//! reports `undecided` / `degraded` instead of quietly returning an empty
//! verdict: an empty `foreign` list and "I could not see half the fleet" are
//! the same bytes on the wire, and the hook cannot tell them apart unless the
//! server says so (ethos rule 4).
//!
//! # Request (from the hook)
//!
//! ```json
//! {"session": "<name>", "dir": "<git toplevel>", "paths": ["<repo-relative>"],
//!  "op": "commit", "guard_version": 2}
//! ```
//! plus `X-Amux-Session` (or `X-Amux-Worker`). The **header wins** over
//! `body.session`: provenance is the server-verified origin, never body text
//! (AMUX-1768). `op` and `guard_version` are optional — old hooks send
//! neither.
//!
//! # Response (what the installed hooks read)
//!
//! ```json
//! {"ok": true,
//!  "foreign":   [{"path","owner","age_secs","has_unstaged_changes","why"}],
//!  "shared":    [{"path","owner","peer","age_secs","has_unstaged_changes"}],
//!  "unclaimed": [{"path","has_unstaged_changes"}],
//!  "cotenants": ["<session>"], "window_secs": 21600}
//! ```
//! Every key is present on EVERY return path, including the disabled and
//! no-cotenant short circuits — python's own comment on that: a caller that
//! reads `d["shared"]` should get `[]`, not `None`. `foreign` non-empty means
//! the hook exits 1 and the commit is blocked.
//!
//! Fields this server ADDS (old hooks ignore them; the v2 hook prints them):
//! `undecided` + `reason` (no verdict was computable — do not read the empty
//! lists as "all clear"), `degraded` (a verdict was computed but may
//! UNDER-report, with the reasons), and `hook_outdated` (the caller sent no
//! `guard_version`, so it is a pre-rust hook that swallows server errors).
//!
//! # The classification, verbatim from python
//!
//! Attribution comes from each session's own JSONL transcript, not from git
//! state — git state is exactly what is ambiguous when N agents share one
//! index (AMUX-1730). Sessions are paired by **git repo root**, not by CC_DIR
//! string (AMUX-2337: in a monorepo those differ, and pairing by string left
//! 23 of 36 lanes invisible to the guard while it returned 200).
//!
//! - `foreign`  — a peer's first-hand write, no first-hand write of yours:
//!   BLOCKED. Also fires when you have no record of the path at all.
//! - `shared`   — you edited it too (or it is yours with unstaged changes):
//!   warned, never blocked. Blocking would deadlock a genuinely shared file.
//! - `unclaimed` — staged, and NO session has an edit record inside the
//!   window: warned. Not blockable (no owner to defer to) but silence here is
//!   what let 762e06e sweep a peer's staged work.

use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::session_verbs::{
    all_session_workdirs, iter_jsonl_tail, session_jsonl_path, session_work_dir,
};

/// py:19187 `_staged_guard_window` — floor 600s, default 6h.
const WINDOW_DEFAULT: f64 = 21600.0;
const WINDOW_FLOOR: f64 = 600.0;
/// AF-27: a self-edit counts as "strictly fresher" than a peer's only if it beats
/// it by MORE than this. NOT a tuning knob (ethos would object to one) — it is the
/// UNIT CONVERSION between the two clocks being compared: the committer's inferred
/// ts is a file MTIME (disk), the peer's is a transcript ts (process wall-clock),
/// and below the skew between them the ordering carries no information. Confirmed
/// by amux-frustrations' forensic: the AF-27 verdicts were bimodal — one coincident
/// near-tie at 0.2s (which flips sign across the two clocks — a coin toss) and five
/// real wins at 3462-14331s. Set at the skew floor: comfortably above sub-second
/// noise, three orders of magnitude below the real gap, so it blocks the near-tie
/// (762e06e reopens otherwise) without touching a genuine win.
const RECENCY_SKEW_MARGIN_S: f64 = 5.0;
/// py `_SHELL_WRITE_SLACK` — how far a file's mtime may sit from a Bash
/// command's timestamp and still count as that command's write.
const SHELL_WRITE_SLACK_DEFAULT: f64 = 180.0;
/// py `_iter_jsonl_tail(jf, max_bytes=4_000_000)`.
const JSONL_TAIL_BYTES: u64 = 4_000_000;
/// py `_edit_paths_cache` — 30s per (session, window, provenance).
const EDIT_CACHE_TTL: f64 = 30.0;
/// py `rel_paths[:2000]`. Truncation is REPORTED (see `degraded`): a silently
/// dropped path is an unguarded path.
const MAX_PATHS: usize = 2000;
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// True the first time a notification key is seen in the last hour.
///
/// In-memory ON PURPOSE, with the cost named rather than hidden: this process
/// re-execs whenever the builder installs a new binary, so a restart forgets
/// and an owner may get one duplicate notice. That is the right trade here —
/// the failure this dedupes is spam from a retried pre-commit hook, and the
/// failure a persistent store would prevent is one extra message. Compare
/// ethos D1, where in-memory state silently DISABLED an optimisation; here the
/// worst case is a message arriving twice, which is self-evident to the reader.
pub fn notify_once(key: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static SEEN: std::sync::OnceLock<Mutex<HashMap<String, f64>>> = std::sync::OnceLock::new();
    let m = SEEN.get_or_init(|| Mutex::new(HashMap::new()));
    let now = now_epoch();
    let Ok(mut g) = m.lock() else { return true };
    g.retain(|_, t| now - *t < 3600.0);
    if g.contains_key(key) {
        return false;
    }
    g.insert(key.to_string(), now);
    true
}

fn now_epoch() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

use crate::config::env_f64;

fn window_secs() -> f64 {
    env_f64("AMUX_STAGED_GUARD_WINDOW_SECS", WINDOW_DEFAULT).max(WINDOW_FLOOR)
}

fn guard_enabled() -> bool {
    !matches!(
        std::env::var("AMUX_STAGED_GUARD").unwrap_or_default().trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// `os.path.realpath` semantics: resolve symlinks, but NEVER require the path
/// to exist. `Path::canonicalize` requires existence, and a staged path that
/// no longer exists is not an edge case here — it is a staged DELETION, one of
/// the two sweeps that happened on this checkout tonight. Falling back to the
/// unresolved string would key deletions under a different realpath than the
/// transcript's, so a peer's deletion would classify as `unclaimed` instead of
/// `foreign`: the guard would go quiet on exactly the case it was reported for.
fn realpath(p: &Path) -> String {
    if let Ok(c) = p.canonicalize() {
        return c.to_string_lossy().into_owned();
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p.to_path_buf();
    while let Some(parent) = cur.parent().map(Path::to_path_buf) {
        let Some(fname) = cur.file_name().map(|f| f.to_os_string()) else { break };
        tail.push(fname);
        if let Ok(c) = parent.canonicalize() {
            let mut out = c;
            for seg in tail.iter().rev() {
                out.push(seg);
            }
            return out.to_string_lossy().into_owned();
        }
        if parent.as_os_str().is_empty() {
            break;
        }
        cur = parent;
    }
    p.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// git
// ---------------------------------------------------------------------------

async fn git_out(dir: &str, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Has `owner` already COMMITTED their work on `path`, more recently than the
/// edit record that triggered the notice?
///
/// The victim-side notice fires on EDIT RECORDS, which cannot tell "edited and
/// committed" from "edited and still staged". Measured 2026-08-15: four notices
/// in one day for files whose owner had already committed, against one real
/// absorption. A recipient has to run `git log` every time to find out which.
///
/// This does not SUPPRESS the notice — the asymmetry forbids it. A false alarm
/// costs a `git log`; a missed alarm costs work that the sweep exists to save.
/// So the notice keeps firing and gains the discriminator instead, which is the
/// ethos answer: make the instrument express the difference rather than guess.
///
/// Authorship is by the Amux-Session TRAILER, not `%an` — every lane on this
/// machine commits as the same person, so `%an` cannot discriminate (CLAUDE.md
/// deploy section).
async fn owner_committed_since(dir: &str, path: &str, owner: &str, edit_age_secs: i64) -> Option<String> {
    let out = git_out(
        dir,
        &["log", "-8", "--format=%h%x09%ct%x09%(trailers:key=Amux-Session,valueonly,separator=)", "--", path],
    )
    .await?;
    let now = now_epoch() as i64;
    for line in out.lines() {
        let mut it = line.split('\t');
        let (sha, ts, who) = (it.next()?, it.next()?, it.next().unwrap_or("").trim());
        if who != owner {
            continue;
        }
        let ts: i64 = ts.trim().parse().ok()?;
        // Their commit is STRICTLY NEWER than the edit that triggered this notice,
        // so the work in question is already theirs and in history.
        //
        // Strict, not `<=`: at equal age the two are indistinguishable at
        // one-second resolution, and the tie must fall to "not settled". Getting
        // this backwards tells a victim "nothing at risk" about work that is
        // still only staged — the one direction this function must never get
        // wrong. Caught by the 0-second case in the test below, not by review.
        if (now - ts) < edit_age_secs {
            return Some(sha.to_string());
        }
    }
    None
}

/// What actually happened to a path, for the victim notice. Three states,
/// because two of them need OPPOSITE responses and the first version of this
/// collapsed them (amux, 2026-08-15).
#[derive(Debug, PartialEq)]
pub(crate) enum PathFate {
    /// The owner committed it themselves. Nothing to do.
    SettledByOwner(String),
    /// The BYTES are in HEAD, but under someone else's commit — an absorption
    /// that already happened cleanly. The CODE is safe; only the REASONING is
    /// lost, so the response is "record it on the card", never "check this one".
    /// Keying only on the owner's own trailer reported this as at-risk, which
    /// cries wolf on every absorption that already went fine.
    AbsorbedBy(String, String),
    /// The path differs from HEAD and the owner has no commit for it: the work
    /// is genuinely uncommitted and a sweep would take it. This is the state
    /// the AC-355 block exists to prevent.
    AtRisk,
}

/// Decide between the three. Content-in-HEAD is checked with `git diff HEAD`,
/// which answers "are these bytes committed by ANYONE" — the question the
/// trailer-only check could not ask.
pub(crate) async fn path_fate(dir: &str, path: &str, owner: &str, edit_age_secs: i64) -> PathFate {
    if let Some(sha) = owner_committed_since(dir, path, owner, edit_age_secs).await {
        return PathFate::SettledByOwner(sha);
    }
    // Empty `git diff HEAD -- path` means the working tree matches HEAD, i.e.
    // whatever was written is committed — by someone.
    let dirty = git_out(dir, &["diff", "HEAD", "--name-only", "--", path])
        .await
        .map(|o| !o.trim().is_empty())
        .unwrap_or(true); // unreadable -> assume at risk, never reassure
    if dirty {
        return PathFate::AtRisk;
    }
    let last = git_out(
        dir,
        &["log", "-1", "--format=%h%x09%(trailers:key=Amux-Session,valueonly,separator=)", "--", path],
    )
    .await
    .unwrap_or_default();
    let mut it = last.trim().split('\t');
    let sha = it.next().unwrap_or("").trim().to_string();
    let who = it.next().unwrap_or("").trim().to_string();
    if sha.is_empty() {
        return PathFate::AtRisk;
    }
    PathFate::AbsorbedBy(sha, if who.is_empty() { "(untrailered)".into() } else { who })
}

/// py:19380 `_repo_root`, memoized — a commit is latency-sensitive and this is
/// called once per session per check.
///
/// Deliberately diverges on ONE point: python memoized the empty string too,
/// so a directory probed before its repo existed stayed permanently
/// unresolvable, and unresolvable means the cotenant pairing silently falls
/// back to string equality (the AMUX-2337 blindness). Failures are not cached.
static REPO_ROOT_MEMO: Mutex<Option<BTreeMap<String, String>>> = Mutex::new(None);

async fn repo_root(dir: &str) -> Option<String> {
    if dir.is_empty() {
        return None;
    }
    if let Ok(g) = REPO_ROOT_MEMO.lock() {
        if let Some(hit) = g.as_ref().and_then(|m| m.get(dir)).cloned() {
            return Some(hit);
        }
    }
    let root = git_out(dir, &["rev-parse", "--show-toplevel"])
        .await
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    if let Ok(mut g) = REPO_ROOT_MEMO.lock() {
        g.get_or_insert_with(BTreeMap::new).insert(dir.to_string(), root.clone());
    }
    Some(root)
}

// ---------------------------------------------------------------------------
// Transcript-derived edit records (py:19207 `_session_recent_edit_paths`)
// ---------------------------------------------------------------------------

const EDIT_TOOL_NAMES: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// py `_PATHLIKE_RE`. Deliberately loose — the mtime check is what decides
/// ownership, not this regex.
fn pathlike_re() -> &'static regex::Regex {
    static RE: Mutex<Option<&'static regex::Regex>> = Mutex::new(None);
    let mut g = RE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(r) = *g {
        return r;
    }
    // The pattern is a compile-time constant and known good; the leak is a
    // one-time 'static promotion, not per-call.
    let r: &'static regex::Regex = Box::leak(Box::new(
        regex::Regex::new(r"[\w./~-]*[\w-]+\.[A-Za-z0-9]{1,8}").expect("static regex"),
    ));
    *g = Some(r);
    r
}

/// One session's edit records plus whether we could actually SEE its
/// transcript. `transcript_found=false` is the difference between "this
/// session edited nothing" and "this session is invisible to the guard" —
/// identical `paths` maps, opposite meanings, and only the second one can let
/// a sweep through.
#[derive(Clone, Default)]
struct EditScan {
    paths: HashMap<String, f64>,
    transcript_found: bool,
}

type EditCacheKey = (String, u64, bool);
static EDIT_CACHE: Mutex<Option<HashMap<EditCacheKey, (f64, EditScan)>>> = Mutex::new(None);

fn parse_ts(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_millis()) / 1000.0);
    }
    // `datetime.fromisoformat` also accepts a naive stamp; python then read it
    // as local time. Claude's transcripts are always Z-suffixed, so this is a
    // fallback, and UTC is the safe reading: guessing local could shift a
    // record by hours and silently drop it out of the window.
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|n| n.and_utc().timestamp() as f64)
}

/// AMUX-3128: does a shell command write a file via an output redirection?
/// `> f`, `>> f`, `&> f` write a file; fd dups (`2>&1`, `>&2`) do not. Used to
/// keep `is_pure_read_command` from treating `echo x > f` as a read.
fn has_output_redirection(cmd: &str) -> bool {
    let b = cmd.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'>' {
            let mut j = i + 1;
            if j < b.len() && b[j] == b'>' {
                j += 1;
            }
            while j < b.len() && matches!(b[j], b' ' | b'\t') {
                j += 1;
            }
            // `>&...` is a file-descriptor dup/merge, not a write to a file.
            if j < b.len() && b[j] != b'&' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// AMUX-3128: a purely read-only shell command must not mint an INFERRED edit
/// record. `recent_edit_paths` claims a Bash-named path when the file's mtime
/// moved within the slack window, but that cannot tell "I wrote foo.md" from "I
/// READ foo.md while a peer wrote it": a lane that ran `head -40 digests/x.md`
/// (a read) was flagged as a co-author of the digest because its mtime happened
/// to move under the digest producer's write. So if EVERY segment of the command
/// leads with a known read-only tool AND there is no output redirection, claim
/// nothing. Conservative on purpose: anything unrecognized (`sed -i`, `cp`, `mv`,
/// python heredocs, a prefixed `sudo head`) falls through to the mtime gate
/// exactly as before, so no real inferred WRITE loses its attribution — only the
/// unambiguous readers are excluded.
fn is_pure_read_command(cmd: &str) -> bool {
    if has_output_redirection(cmd) {
        return false;
    }
    const READ_ONLY_VERBS: &[&str] = &[
        "ls", "cat", "head", "tail", "less", "more", "grep", "egrep", "fgrep",
        "rg", "ag", "wc", "stat", "file", "find", "cmp", "diff", "sort", "uniq",
        "cut", "column", "od", "xxd", "hexdump", "tree", "du", "basename",
        "dirname", "realpath", "readlink", "sha256sum", "md5sum", "nl", "tac",
        "pwd", "echo", "printf",
    ];
    // `git <read-subcommand>` is how a careful VERIFIER reads a file — `git show`,
    // `git log --stat`, `git diff`, `git grep`, `git blame` to chase a specific
    // row (AMUX-3128 follow-up, gtm-ticker). The verb is `git`, which is NOT in
    // READ_ONLY_VERBS, so these minted an inferred edit record and flagged the
    // reader as co-author of the file they were only inspecting — blocking the
    // real committer, and worse, punishing careful verification: the harder a peer
    // checks your output, the more it blocks you, which trains toward GN=1 where
    // the guard stops protecting anything. WRITE subcommands (add/commit/checkout/
    // reset/restore/rm/mv/apply/stash/merge/rebase/pull/am/…) are ABSENT on
    // purpose, so a real working-tree mutation still falls through to the mtime
    // gate and keeps its attribution.
    const GIT_READ_SUBCMDS: &[&str] = &[
        "show", "log", "diff", "status", "blame", "grep", "cat-file", "shortlog",
        "describe", "rev-parse", "rev-list", "ls-files", "ls-tree", "reflog",
        "whatchanged", "annotate", "name-rev", "show-ref", "for-each-ref",
    ];
    let mut saw = false;
    for seg in cmd.split(['|', ';', '&', '\n', '(', ')', '`']) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        saw = true;
        let Some(tok) = seg.split_whitespace().next() else {
            return false;
        };
        let verb = Path::new(tok)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(tok);
        if verb == "git" {
            // Find the subcommand, skipping global flags and the argument of the
            // arg-taking ones (`-C <dir>`, `-c <cfg>`), then require it be read-only.
            // A bare `git` or a write/unknown subcommand falls through (return
            // false) exactly as before — conservative by design.
            let mut rest = seg.split_whitespace().skip(1);
            let mut sub = None;
            while let Some(t) = rest.next() {
                if t == "-C" || t == "-c" {
                    rest.next(); // consume its argument
                    continue;
                }
                if t.starts_with('-') {
                    continue; // other global flag, e.g. --no-pager
                }
                sub = Some(t);
                break;
            }
            match sub {
                Some(s) if GIT_READ_SUBCMDS.contains(&s) => continue,
                _ => return false,
            }
        }
        if !READ_ONLY_VERBS.contains(&verb) {
            return false;
        }
    }
    saw
}

/// AMUX-3128 surfacing half: every INFERRED edit record (a Bash command whose
/// named path moved mtime within slack) is logged so the class stays countable.
/// A future false co-authorship now leaves a trace naming the verb — and if a
/// sweep sees a READ verb here (head/cat/ls...), a reader slipped
/// `is_pure_read_command` and the exclusion list is missing it. Deduped per
/// (session, path basename, verb) for an hour so legit `sed -i` churn cannot spam.
static INFERRED_WARNED: Mutex<Option<HashMap<String, f64>>> = Mutex::new(None);
fn warn_inferred_edit(session: &str, abs_path: &str, cmd: &str) {
    let verb = cmd
        .split(['|', ';', '&', '\n', '(', ')', '`'])
        .filter_map(|s| s.split_whitespace().next())
        .next()
        .map(|t| Path::new(t).file_name().and_then(|s| s.to_str()).unwrap_or(t))
        .unwrap_or("?");
    let base = Path::new(abs_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(abs_path);
    let now = now_epoch();
    let key = format!("{session}\u{1}{base}\u{1}{verb}");
    if let Ok(mut g) = INFERRED_WARNED.lock() {
        let m = g.get_or_insert_with(HashMap::new);
        if m.get(&key).is_some_and(|at| now - at < 3600.0) {
            return;
        }
        m.insert(key, now);
    }
    tracing::warn!(
        target: "staged_guard",
        "[staged-guard/inferred-edit AMUX-3128] session={} path={} verb={} — ownership \
         INFERRED from a bash command, not a firsthand Edit/Write. A READ verb here means \
         is_pure_read_command missed a reader and it may be minting false co-authorship.",
        if session.is_empty() { "(none)" } else { session },
        base,
        verb,
    );
}

/// py:19207. `{abs realpath: epoch ts}` of files this session edited inside
/// the window, read from its own JSONL transcript.
///
/// Two provenances, and callers must choose (AMUX-2456): FIRST-HAND is an
/// Edit/Write/MultiEdit `tool_use` naming the file — the transcript says this
/// session wrote it. INFERRED is a Bash command naming a path whose mtime
/// moved within the slack window; that catches `sed -i` and heredocs, but
/// mtime is a SHARED resource on a shared checkout, so it can claim a peer's
/// write. `firsthand_only` drops the inferred half.
fn recent_edit_paths(name: &str, since_secs: f64, firsthand_only: bool) -> EditScan {
    let now = now_epoch();
    let key: EditCacheKey = (name.to_string(), since_secs.to_bits(), firsthand_only);
    if let Ok(g) = EDIT_CACHE.lock() {
        if let Some((at, scan)) = g.as_ref().and_then(|m| m.get(&key)) {
            if now - at < EDIT_CACHE_TTL {
                return scan.clone();
            }
        }
    }
    let mut scan = EditScan::default();
    let slack = env_f64("AMUX_SHELL_WRITE_SLACK", SHELL_WRITE_SLACK_DEFAULT);
    // Base for RELATIVE paths named in shell commands. Python's comment: without
    // it the per-block `except` swallowed a NameError and the shell-write
    // detection was silently inert — shipped and doing nothing.
    let work_hint = session_work_dir(name);
    if let Some(jf) = session_jsonl_path(name) {
        scan.transcript_found = true;
        let cutoff = now - since_secs;
        for e in iter_jsonl_tail(&jf, JSONL_TAIL_BYTES) {
            let Some(ts) = e.get("timestamp").and_then(Value::as_str).and_then(parse_ts) else {
                continue;
            };
            if ts < cutoff {
                continue;
            }
            let Some(content) = e.get("message").and_then(|m| m.get("content")).and_then(Value::as_array)
            else {
                continue;
            };
            for blk in content {
                if blk.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                let tool = blk.get("name").and_then(Value::as_str).unwrap_or("");
                if tool == "Bash" {
                    if firsthand_only {
                        continue;
                    }
                    let cmd = blk
                        .get("input")
                        .and_then(|i| i.get("command"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if cmd.is_empty() {
                        continue;
                    }
                    // AMUX-3128: a read-only command (head/ls/cat/grep...) names a
                    // path but writes nothing. The mtime gate below cannot tell a
                    // read from a write, so a PEER's write moving the mtime under
                    // a reader would mint a false co-author. Reads attribute nothing.
                    if is_pure_read_command(cmd) {
                        continue;
                    }
                    for m in pathlike_re().find_iter(cmd) {
                        let cand = m.as_str().trim_matches(|c| "'\"),;:".contains(c));
                        if cand.len() < 4 {
                            continue;
                        }
                        let abs = if Path::new(cand).is_absolute() {
                            PathBuf::from(cand)
                        } else if work_hint.is_empty() {
                            continue
                        } else {
                            Path::new(&work_hint).join(cand)
                        };
                        // MTIME IS WHAT KEEPS THIS HONEST: naming a path is not
                        // writing it. `grep x foo.rs` mentions foo.rs and changes
                        // nothing, so a path is only claimed if the file actually
                        // moved while the command ran.
                        let Ok(md) = std::fs::metadata(&abs) else { continue };
                        if !md.is_file() {
                            continue;
                        }
                        let Some(mt) = md
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64())
                        else {
                            continue;
                        };
                        if (mt - ts).abs() <= slack {
                            let ap = realpath(&abs);
                            if mt > *scan.paths.get(&ap).unwrap_or(&0.0) {
                                warn_inferred_edit(name, &ap, cmd);
                                scan.paths.insert(ap, mt);
                            }
                        }
                    }
                    continue;
                }
                if !EDIT_TOOL_NAMES.contains(&tool) {
                    continue;
                }
                let inp = blk.get("input");
                let fp = inp
                    .and_then(|i| i.get("file_path"))
                    .or_else(|| inp.and_then(|i| i.get("notebook_path")))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !fp.is_empty() {
                    scan.paths.insert(realpath(Path::new(fp)), ts);
                }
            }
        }
    }
    if let Ok(mut g) = EDIT_CACHE.lock() {
        g.get_or_insert_with(HashMap::new).insert(key, (now, scan.clone()));
    }
    scan
}

// ---------------------------------------------------------------------------
// Classification — pure, so the FIRING path is testable (ethos rule 7)
// ---------------------------------------------------------------------------

/// Everything the verdict is computed from. Split out so `classify` takes no
/// filesystem, no git and no clock: the tests below fire the BLOCK path
/// directly, because a guard that has never been observed refusing is theatre.
#[derive(Default)]
pub(crate) struct GuardInputs {
    /// abs realpath -> ts, requesting session, both provenances.
    pub mine: HashMap<String, f64>,
    /// abs realpath, requesting session, first-hand only.
    pub mine_firsthand: HashSet<String>,
    /// abs realpath -> (owner session, newest ts), all cotenants.
    pub theirs: HashMap<String, (String, f64)>,
    /// abs realpath, any cotenant, first-hand only.
    pub theirs_firsthand: HashSet<String>,
    /// abs realpath of files with UNSTAGED changes right now.
    pub dirty: HashSet<String>,
    /// Is ANY cotenant invisible to this verdict (transcript unreadable)?
    ///
    /// AC-355's block is scoped to this, on AMUX-2936's own measurement: the
    /// absorption vector IS blind cotenants — an invisible lane cannot produce a
    /// `foreign` row, so its staged work lands in `unclaimed`. With full
    /// attribution, `unclaimed` means something different and much less
    /// dangerous: nobody touched it in the window, most often the committer's
    /// own pre-window or tool-generated work. Blocking that too would add a
    /// blocking class to a guard already refusing ~18/hour, and AMUX-2936 named
    /// exactly that as what gets the guard switched off (ethos rule 3 — a
    /// constraint people cannot live with is one they disable).
    pub blind_cotenant: bool,
}

#[derive(Default)]
pub(crate) struct Verdict {
    pub foreign: Vec<Value>,
    pub shared: Vec<Value>,
    pub unclaimed: Vec<Value>,
}

/// py:19470-19545. `paths` is (repo-relative, absolute realpath) pairs.
pub(crate) fn classify(
    paths: &[(String, String)],
    now: f64,
    window: f64,
    inp: &GuardInputs,
) -> Verdict {
    let mut v = Verdict::default();
    for (rel, ap) in paths {
        if rel.trim().is_empty() {
            continue;
        }
        let hit = inp.theirs.get(ap);
        let is_dirty = inp.dirty.contains(ap);
        // PROVENANCE STRENGTH, not mere presence (AF-19). `mine` includes the
        // INFERRED half, and this branch `continue`s — so an inferred
        // self-claim used to SUPPRESS the block below and downgrade it to a
        // note. Measured on the incident: a staged test file amux had never
        // opened classified `shared` because their `git add` named it while
        // its mtime was moving, and 762e06e swept it. A first-hand peer claim
        // outranks an inferred self-claim; both first-hand or both inferred is
        // still genuinely shared.
        if !inp.mine_firsthand.contains(ap) {
            if let Some((owner, ts)) = hit {
                // AF-27: a first-hand peer claim outranks an INFERRED self-claim,
                // but ownership was RECENCY-BLIND — a peer's stale first-hand edit
                // (measured: 14,355s / 4h old) outranked the committer's OWN edit
                // made 23.8s ago on the same path, blocking a commit whose every
                // staged hunk was the committer's (in_mine=true, in_firsthand=false).
                // The forensic split was decisive: of 405 per-path verdicts, 399 were
                // true positives (no self-record) and only the 6 with a self-record
                // AND a fresher self-edit were wrong. So: if the committer HAS an edit
                // record here and it is NEWER than the peer's, the fresher edit wins —
                // fall through to `shared` (warned, never blocked). The direction the
                // card carried was right ("you commit seconds after editing"); the
                // mechanism was backwards — not "your fresh edit is missing" but "the
                // peer's stale claim wins anyway". The 399 shape (no self-record, or a
                // genuinely fresher peer) still blocks.
                // Block UNLESS the committer's own edit is STRICTLY fresher than
                // the peer's. Ties block, and that is deliberate: the 762e06e sweep
                // (AF-19) is a TIE — the committer's inferred claim there is the
                // peer's own concurrent write caught by the same mtime event, so
                // the two timestamps coincide. Only a clear gap (AF-27's real case:
                // committer 23.8s vs peer 14,355s) proves the committer edited
                // AFTER the peer and owns the current content. No self-record at
                // all (the 399 shape) also blocks.
                // Per-path both sides: inp.mine[ap] is the committer's edit ts to
                // THIS path, ts is the peer's to THIS path (not a set-wide newest —
                // that asymmetry would under-block). Fresher by MORE than the
                // clock-skew margin only; a within-skew lead is a coin toss and blocks.
                let committer_fresher =
                    inp.mine.get(ap).map(|m| *m > *ts + RECENCY_SKEW_MARGIN_S).unwrap_or(false);
                if inp.theirs_firsthand.contains(ap) && !committer_fresher {
                    v.foreign.push(json!({
                        "path": rel,
                        "owner": owner,
                        "age_secs": (now - ts).max(0.0) as i64,
                        "has_unstaged_changes": is_dirty,
                        // Python emitted "your claim is inferred" here
                        // unconditionally, including when the committer had NO
                        // claim on the path at all — a false statement about
                        // the reader's own work, printed at the moment they are
                        // deciding whether to override. Same verdict, honest
                        // sentence: only say the claim was inferred when there
                        // actually is one (AF-26 applied to its own wording).
                        "why": if inp.mine.contains_key(ap) {
                            "they wrote it (transcript); your claim is inferred".to_string()
                        } else {
                            format!(
                                "they wrote it (transcript); you have no edit record on this path in the last {}m — basis is edit records, not the staged diff",
                                (window / 60.0) as i64
                            )
                        },
                    }));
                    continue;
                }
            }
        }
        if inp.mine.contains_key(ap) {
            if hit.is_some() || is_dirty {
                // `peer` distinguishes the two reasons this fires, which the
                // owner field alone cannot (AF-24): a real co-editor, versus
                // YOUR OWN file that merely has unstaged changes. The renderer
                // said "also edited by session '(unknown)'" for the second
                // case — a phantom co-editor on a solo edit, which is how a
                // real co-edit warning stops being read.
                v.shared.push(json!({
                    "path": rel,
                    "owner": hit.map(|(o, _)| o.as_str()).unwrap_or("(unknown)"),
                    "peer": hit.is_some(),
                    "age_secs": hit.map(|(_, ts)| (now - ts).max(0.0) as i64).unwrap_or(0),
                    "has_unstaged_changes": is_dirty,
                }));
            }
            continue;
        }
        if let Some((owner, ts)) = hit {
            v.foreign.push(json!({
                "path": rel,
                "owner": owner,
                "age_secs": (now - ts).max(0.0) as i64,
                "has_unstaged_changes": is_dirty,
                // WHY, recorded on the verdict (AF-26): a block that says only
                // "edited by X" cannot be told apart from a block that is
                // wrong. The basis is edit RECORDS in a window — this never
                // reads the staged patch — so say that where the reader is.
                "why": format!(
                    "no edit of yours on this path in the last {}m; basis is edit records, not the staged diff",
                    (window / 60.0) as i64
                ),
            }));
            continue;
        }
        // AC-355: UNCLAIMED BLOCKS **WHEN A COTENANT IS INVISIBLE**. It used to warn, on the reasoning that
        // there is "no owner to defer to" — which treated UNKNOWN as SAFE, and
        // that is the whole bug. Staged, and no session can account for it,
        // means someone put it in the shared index and the guard cannot see
        // who; on a shared checkout the likeliest who is a peer whose
        // transcript is unreadable.
        //
        // Measured twice in one day, both PURE sweeps where the committer had
        // no record of the file at all: 7d0e95d took a peer's invariants/
        // checks.rs + monitor.rs, and 6217dc0 — MY OWN commit improving this
        // very guard — took a peer's api/alerts.rs + the amux CLI. The guard
        // warned on both and blocked neither. A guard-improvement commit being
        // itself a sweep is the argument.
        //
        // Emitted as `foreign` rather than a new key on purpose: installed
        // hooks live on checkouts this repo cannot see and block on exactly one
        // thing, `foreign` being non-empty (module docs — the server adapts to
        // the hooks, never the reverse). A new field would be ignored by every
        // hook already on disk, which is the same silence in a new spelling.
        //
        // NOT CLOSED by this, and named so a reader does not over-trust the
        // block: the BOTH-EDITED case. When two sessions edit the same file the
        // shared index holds one blob — whoever `git add`-ed last — and the
        // committer HAS a record for that path, so this never fires while the
        // staged bytes may still be the peer's. That needs content-level
        // detection (staged blob vs the committer's last-written bytes) or
        // per-lane worktrees (MI-4183).
        if !inp.blind_cotenant {
            // Fully-attributed checkout: nobody's, but nobody is hidden either.
            // Stays a warning, as before.
            v.unclaimed.push(json!({"path": rel, "has_unstaged_changes": is_dirty}));
            continue;
        }
        v.foreign.push(json!({
            "path": rel,
            "owner": "",
            "age_secs": 0,
            "has_unstaged_changes": is_dirty,
            "why": format!(
                "staged, but NO session has an edit record for it in the last {}m — including \
                 you — AND a cotenant on this checkout is invisible to the guard right now, so \
                 the likeliest owner is the lane we cannot see. Committing it ships their work \
                 under your message. If it is genuinely yours, AMUX_VERIFIED_SOLO=1 after \
                 checking `git diff --cached -- {}`",
                (window / 60.0) as i64,
                rel
            ),
        }));
    }
    v
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Every response carries EVERY key python's did, on every path — including
/// the short circuits. A caller reading `d["shared"]` must get `[]`, never
/// `None`; shape-inconsistent returns are how a consumer ends up special-casing
/// one branch and silently mishandling the other.
#[derive(Default)]
struct Envelope {
    verdict: Verdict,
    cotenants: Vec<String>,
    window: f64,
    /// A verdict WAS computed but may UNDER-report: a peer we could not see, a
    /// git call that failed, paths truncated.
    degraded: Vec<String>,
    /// Set when NO verdict was computable. The empty lists are then not "all
    /// clear", and the v2 hook says so out loud rather than exiting 0 silently.
    undecided: Option<String>,
    hook_outdated: bool,
}

impl Envelope {
    fn json(self) -> Value {
        json!({
            "ok": true,
            "foreign": self.verdict.foreign,
            "shared": self.verdict.shared,
            "unclaimed": self.verdict.unclaimed,
            "cotenants": self.cotenants,
            "window_secs": self.window as i64,
            "undecided": self.undecided.is_some(),
            "reason": self.undecided.unwrap_or_default(),
            "degraded": self.degraded,
            "hook_outdated": self.hook_outdated,
        })
    }
}

/// Sessions still running a pre-rust hook — the one whose `except: return 0`
/// hid the 405 for the entire cutover. Warned once per session per hour: the
/// hook fires ~1,147 times an hour fleet-wide, and a detector that logs on
/// every call spends the same resource (log volume) as the faults it is
/// hunting (ethos rule 7).
static OUTDATED_WARNED: Mutex<Option<HashMap<String, f64>>> = Mutex::new(None);

fn warn_outdated_hook(session: &str, dir: &str) {
    let now = now_epoch();
    let key = format!("{session}\u{1}{dir}");
    if let Ok(mut g) = OUTDATED_WARNED.lock() {
        let m = g.get_or_insert_with(HashMap::new);
        if m.get(&key).is_some_and(|at| now - at < 3600.0) {
            return;
        }
        m.insert(key, now);
    }
    tracing::warn!(
        target: "staged_guard",
        "[staged-guard] OUTDATED HOOK: {} in {} sent no guard_version — that hook swallows \
         server errors (`except Exception: return 0`) and printed nothing for the whole \
         405 window. Reinstall: scripts/install-hooks.sh",
        if session.is_empty() { "(no session)" } else { session },
        dir
    );
}

/// The HTTP entry point — a REAL commit attempt, so owners get notified.
pub async fn staged_guard(
    axum::extract::State(state): axum::extract::State<super::AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, Json<Value>) {
    staged_guard_inner(Some(state), headers, body).await
}

/// The verdict itself. `state` is `None` for INTERNAL callers, and that is the
/// whole reason it is optional rather than threaded: `commit_nudge` calls this
/// to ask "who owns these paths", which is a background probe and not a commit.
/// Notifying an owner from it would tell them their file was being swept every
/// time a nudge tick ran — a notice that is false, repeated, and precisely the
/// noise that gets a channel muted (ethos rule 5).
pub async fn staged_guard_inner(
    state: Option<super::AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, Json<Value>) {
    // Parsed by hand rather than via `Json<T>`: axum's rejection is a
    // PLAIN-TEXT 400, which the hook's `json.loads` throws on — straight into
    // the `except` that started this incident. Every answer from here is JSON.
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let obj = parsed.as_object();
    let get_str = |k: &str| -> String {
        obj.and_then(|o| o.get(k)).and_then(Value::as_str).unwrap_or("").trim().to_string()
    };

    // Server-verified origin wins over the claimed body session (AMUX-1768).
    let origin = super::alerts::hdr_worker(&headers);
    let session: String =
        if origin.is_empty() { get_str("session") } else { origin }.chars().take(64).collect();
    let wd_raw = get_str("dir");
    let op: String = {
        let o = get_str("op");
        if o.is_empty() { "commit".into() } else { o.chars().take(24).collect() }
    };
    let guard_version =
        obj.and_then(|o| o.get("guard_version")).and_then(Value::as_i64).unwrap_or(0);
    let hook_outdated = guard_version < 2;
    if hook_outdated {
        warn_outdated_hook(&session, &wd_raw);
    }

    if wd_raw.is_empty() {
        // py:71821 — the one non-200, kept byte-compatible.
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "dir required"})));
    }
    let window = window_secs();
    if !guard_enabled() {
        let mut v = Envelope { window, hook_outdated, ..Default::default() }.json();
        v["enabled"] = json!(false);
        return (StatusCode::OK, Json(v));
    }

    let raw_paths: Vec<String> = obj
        .and_then(|o| o.get("paths"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();

    let wd = std::fs::canonicalize(&wd_raw)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| wd_raw.clone());

    let mut degraded: Vec<String> = Vec::new();

    // ---- cotenants, paired by REPO ROOT (AMUX-2337) -----------------------
    let all_dirs: BTreeMap<String, String> = all_session_workdirs();
    if all_dirs.is_empty() {
        // The one genuinely undecidable state: with no session inventory there
        // are no cotenants to compare against, so the verdict would be an
        // empty `foreign` — indistinguishable from "your commit is clean". Say
        // so instead of answering it.
        return (
            StatusCode::OK,
            Json(
                Envelope {
                    window,
                    degraded,
                    hook_outdated,
                    undecided: Some(format!(
                        "no session inventory readable ({}/sessions) — cotenants unknown, so an \
                         empty verdict here means NOTHING was compared",
                        amux_home().display()
                    )),
                    ..Default::default()
                }
                .json(),
            ),
        );
    }
    let wd_root = match repo_root(&wd).await {
        Some(r) => r,
        None => {
            degraded.push(format!(
                "`git -C {wd} rev-parse --show-toplevel` failed — falling back to the raw \
                 directory for cotenant pairing, which misses lanes whose CC_DIR is a \
                 subdirectory of the repo (AMUX-2337)"
            ));
            wd.clone()
        }
    };
    let mut cotenants: Vec<String> = Vec::new();
    for (other, od) in &all_dirs {
        if other == &session {
            continue;
        }
        let root = repo_root(od).await.unwrap_or_else(|| od.clone());
        if root == wd_root {
            cotenants.push(other.clone());
        }
    }

    if cotenants.is_empty() || raw_paths.is_empty() {
        return (
            StatusCode::OK,
            Json(Envelope { cotenants, window, degraded, hook_outdated, ..Default::default() }.json()),
        );
    }

    let truncated = raw_paths.len() > MAX_PATHS;
    if truncated {
        degraded.push(format!(
            "{} staged paths, only the first {MAX_PATHS} were classified — the remainder are \
             UNGUARDED in this verdict",
            raw_paths.len()
        ));
    }
    let rel_paths: Vec<String> = raw_paths.into_iter().take(MAX_PATHS).collect();

    // ---- unstaged-right-now (AC-297): the working tree is ground truth ----
    let mut dirty: HashSet<String> = HashSet::new();
    match git_out(&wd, &["diff", "--name-only", "-z"]).await {
        Some(out) => {
            for p in out.split('\0').filter(|p| !p.trim().is_empty()) {
                dirty.insert(realpath(&Path::new(&wd).join(p)));
            }
        }
        None => degraded.push(format!(
            "`git -C {wd} diff --name-only` failed — `has_unstaged_changes` is unknown, so \
             co-edit notices may render as NOTE where they should be WARNING"
        )),
    }

    // ---- transcripts (blocking IO, off the async executor) ----------------
    let scan_session = session.clone();
    let scan_cotenants = cotenants.clone();
    let scan = tokio::task::spawn_blocking(move || {
        let mine = if scan_session.is_empty() {
            EditScan::default()
        } else {
            recent_edit_paths(&scan_session, window, false)
        };
        let mine_fh = if scan_session.is_empty() {
            EditScan::default()
        } else {
            recent_edit_paths(&scan_session, window, true)
        };
        let mut theirs: HashMap<String, (String, f64)> = HashMap::new();
        let mut theirs_fh: HashSet<String> = HashSet::new();
        let mut blind: Vec<String> = Vec::new();
        for other in &scan_cotenants {
            let fh = recent_edit_paths(other, window, true);
            if !fh.transcript_found {
                blind.push(other.clone());
            }
            theirs_fh.extend(fh.paths.keys().cloned());
            for (p, ts) in recent_edit_paths(other, window, false).paths {
                match theirs.get(&p) {
                    Some((_, cur)) if *cur >= ts => {}
                    _ => {
                        theirs.insert(p, (other.clone(), ts));
                    }
                }
            }
        }
        (mine, mine_fh, theirs, theirs_fh, blind)
    })
    .await;

    let (mine, mine_fh, theirs, theirs_fh, blind) = match scan {
        Ok(t) => t,
        Err(e) => {
            // NEVER a 500. A 5xx reaches the hook as an exception and the whole
            // point of this file is that an exception there is invisible.
            return (
                StatusCode::OK,
                Json(
                    Envelope {
                        cotenants,
                        window,
                        degraded,
                        hook_outdated,
                        undecided: Some(format!(
                            "transcript scan did not complete ({e}) — nothing was compared"
                        )),
                        ..Default::default()
                    }
                    .json(),
                ),
            );
        }
    };

    if !mine.transcript_found && !session.is_empty() {
        // Not undecided — a missing self-transcript makes the guard STRICTER
        // (nothing is claimed as yours), so the verdict is still safe. But it
        // is the reason a commit can be blocked on your own work, which is
        // exactly the AF-26 false positive, so name it where the operator is.
        degraded.push(format!(
            "no transcript found for '{session}' — none of the staged paths can be claimed as \
             yours, so this verdict is stricter than it should be"
        ));
    }
    // PARTITION THE BLIND BY LIVENESS (AMUX-2936's measurement, acted on).
    // The signal below ran for ~5h and answered the card's question with a
    // number nobody guessed: 881 warnings, dominated by the mixpeek checkout
    // where THIRTY-TWO cotenants are blind on every commit — nearly all of
    // them long-retired project lanes. A warning that fires 32 names at every
    // committer on the busiest repo is alarm fatigue by construction; the one
    // real signal (a LIVE lane whose transcript cannot be read) drowns in it.
    //
    // Liveness is the discriminator because it is checkable without file
    // mtimes (proven unreliable for this exact question earlier tonight:
    // amux-helper's env mtime said 98 days idle while it committed the same
    // day). A RUNNING blind cotenant can be mid-edit right now — that stays
    // the loud case. A STOPPED one can only contribute pre-stop edits inside
    // the window; possible, so it is still disclosed, but compactly and
    // without the per-name klaxon.
    // ABSENT IS NOT BLIND (AMUX-2936, decided 2026-08-15 with amux).
    //
    // `blind` already means exactly one thing: no resolvable transcript. Split
    // on liveness and the two halves are different FACTS, not degrees:
    //
    //   no transcript + RUNNING  -> BLIND. A live lane we cannot see. It may be
    //                               staging work right now, so it gates the
    //                               AC-355 block and stays the loud case.
    //   no transcript + stopped  -> ABSENT. Not a lane the guard cannot see —
    //                               a lane that is not there. No recorded
    //                               activity at all, so there is no invisible
    //                               work for it to hide.
    //
    // THE EXCLUSION KEYS ON "NO RESOLVABLE TRANSCRIPT", never on liveness alone
    // and never on env mtime. A STOPPED lane WITH a transcript is a real
    // cotenant and keeps blocking — its pre-stop staged work is precisely the
    // sweep target AC-355 exists for. Folding this into a "stopped" or
    // "stale-env" flag re-breaks that case, which is why the two are separate
    // verdicts and separate messages.
    //
    // The fact that settled it: amux-helper made every commit on this checkout
    // report PARTIAL forever. Measured — newest commit 2026-07-28 (18 days, by
    // author AND committer date, so no rebase is hiding it), not running, no
    // pane, env file 85 bytes from May 5, no resolvable transcript. AMUX-2936
    // had rejected the env-mtime proxy on the grounds that it "committed to this
    // repo TODAY"; that turned out to be false, so the card was stuck on a fact
    // rather than a judgement.
    let mut blind_live: Vec<String> = Vec::new();
    let mut absent: Vec<String> = Vec::new();
    for b in &blind {
        if crate::api::session_verbs::is_running(b).await {
            blind_live.push(b.clone());
        } else {
            absent.push(b.clone());
        }
    }
    if !absent.is_empty() {
        // Reported, not silent — but as what it is. "Unreadable transcript"
        // invited the reader to imagine hidden work; "no such lane" does not.
        degraded.push(format!(
            "{} cotenant(s) with no recorded activity at all — {} — treated as ABSENT, not \
             blind: not running, no resolvable transcript, so there is no invisible work to \
             hide. These do NOT gate the unattributable-path block.",
            absent.len(),
            absent.join(", ")
        ));
    }
    if !blind_live.is_empty() {
        let blind = &blind_live;
        // The dangerous direction: their edits are invisible, so the verdict
        // UNDER-reports and a sweep of their work would pass silently.
        degraded.push(format!(
            "no transcript for RUNNING cotenant(s) {} — their edits are INVISIBLE to this \
             verdict; an empty result does not clear their files",
            blind.join(", ")
        ));
        // AND SAY SO WHERE IT CAN BE COUNTED (AMUX-2936). Until now this
        // verdict was returned to the hook and nowhere else: the operator saw
        // "PARTIAL — no transcript for cotenant(s) X" on their own terminal and
        // the server recorded nothing, so the rate was unmeasurable from the
        // logs. That is the reason AMUX-2936 could not be decided — its own
        // instruction was "measure the base rate before choosing a design", and
        // the base rate had no trace to measure.
        //
        // It matters more than a missing counter usually would, because this is
        // the ONLY class through which an absorption passes silently. `foreign`
        // already exits 1 and blocks the commit (740 blocks in 40 hours, so the
        // blocking half demonstrably works). A blind cotenant cannot produce a
        // `foreign` row at all — their edits are invisible — so their files land
        // in `unclaimed`, which is explicitly not blockable, and the sweep goes
        // through with a warning nobody is obliged to read.
        //
        // Coverage also DECAYS: a session that has stopped has no live
        // transcript, so every retired lane permanently widens the blind set
        // while the guard keeps answering 200.
        tracing::warn!(
            target: "staged_guard",
            "[staged-guard/AMUX-2936] blind-cotenant verdict for {} in {}: {} invisible ({}), \
             {} staged path(s) — an absorption of THEIR work would pass silently here",
            if session.is_empty() { "(no session)" } else { &session },
            wd_root,
            blind.len(),
            blind.join(","),
            rel_paths.len()
        );
    }

    let now = now_epoch();
    let pairs: Vec<(String, String)> = rel_paths
        .iter()
        .map(|rel| (rel.clone(), realpath(&Path::new(&wd).join(rel))))
        .collect();
    let inputs = GuardInputs {
        mine: mine.paths,
        mine_firsthand: mine_fh.paths.keys().cloned().collect(),
        theirs,
        theirs_firsthand: theirs_fh,
        dirty,
            // Live or stopped alike: a stopped lane's PRE-STOP staged work is exactly
        // what gets swept, and the degraded message already admits those edits are
        // invisible. Liveness answers "can they be editing now", which is a
        // different question from "could this staged file be theirs".
        // BLIND only — absent lanes are excluded (see the partition above).
        blind_cotenant: !blind_live.is_empty(),
};
    let v = classify(&pairs, now, window, &inputs);

    if !v.foreign.is_empty() {
        // BLOCK-TIME FORENSICS (AF-27). A block is the expensive verdict and
        // the only one that was undiagnosable after the fact — by the time
        // anyone looked, the claim set had been recomputed and the trail was
        // gone. Logged AT the decision: what the committer's claim set held,
        // how it was sourced, how old its newest entry was.
        let newest = inputs.mine.values().copied().fold(0.0_f64, f64::max);
        tracing::warn!(
            target: "staged_guard",
            // `newest_any`, not `newest` (renamed 2026-08-14). It is the newest entry
            // across the committer's WHOLE claim set, which is NOT what classify()
            // compares — classify uses inputs.mine[path], per-path on both sides. The
            // old name invited exactly that conflation: reading these lines, the margin
            // between a peer's per-path claim and a SET-WIDE newest looked like the
            // decision the code makes, and a reviewer could not tell from the log alone
            // whether an under-block was possible. It was not, but confirming that meant
            // reading the Rust, which is the failure this forensic exists to prevent.
            // mine_age below is the per-path value the comparison actually uses.
            "[staged-guard/AF-27] {} blocked {} in {} — window={}s mine={} (firsthand={}, newest_any={}) \
             cotenants={}; per-path: {}",
            if session.is_empty() { "(no session)" } else { &session },
            op,
            wd_root,
            window as i64,
            inputs.mine.len(),
            inputs.mine_firsthand.len(),
            if newest > 0.0 { format!("{:.1}s ago", now - newest) } else { "none".into() },
            cotenants.len(),
            v.foreign
                .iter()
                .take(8)
                .map(|f| {
                    let rel = f["path"].as_str().unwrap_or("");
                    let ap = realpath(&Path::new(&wd).join(rel));
                    format!(
                        "{rel} in_mine={} in_firsthand={} mine_age={} owner={} age={}s",
                        inputs.mine.contains_key(&ap),
                        inputs.mine_firsthand.contains(&ap),
                        // THE value classify() weighs against the peer's per-path ts.
                        // Without it a reader can only see the set-wide newest_any and
                        // cannot compute the real margin — which is why the 6 defect
                        // verdicts of 2026-08-14 had to be escalated as a question
                        // rather than answered from the log.
                        match inputs.mine.get(&ap) {
                            Some(ts) => format!("{:.1}s", now - ts),
                            None => "absent".into(),
                        },
                        f["owner"].as_str().unwrap_or(""),
                        f["age_secs"].as_i64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        );

        // TELL THE OWNER, NOT ONLY THE COMMITTER (AMUX-2923).
        //
        // Everything above is addressed to the session doing the absorbing,
        // and that half works — it warned me by name the day this card was
        // filed. The half that does not exist is the mirror: the session whose
        // staged file is about to be swept learns NOTHING, because the guard
        // runs in the committer's process and the victim is not in the room.
        //
        // On 2026-08-11 an e2e change of mine landed inside a peer's commit
        // titled "fix(status): a codex picker reads waiting, not idle". The
        // code survived; the reasoning did not, and proving it was not LOST
        // cost ~10 minutes of archaeology. CLAUDE.md already records the worse
        // version — a peer's commit reverting another agent's uncommitted
        // rewrite of that very file.
        //
        // The server is the only party that can see both sides, and it sees
        // them right here. `steer_enqueue` is the same durable path board_drive
        // uses, so this is the existing messages primitive rather than a new
        // channel (delivered at the owner's next turn boundary, which is the
        // soonest they could act on it anyway).
        if let Some(st) = state.as_ref() {
            let mut by_owner: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
            for f in &v.foreign {
                let owner = f["owner"].as_str().unwrap_or("").to_string();
                let path = f["path"].as_str().unwrap_or("").to_string();
                if owner.is_empty() || owner == session || path.is_empty() {
                    continue;
                }
                let age = f["age_secs"].as_i64().unwrap_or(0);
                by_owner.entry(owner).or_default().push((path, age));
            }
            for (owner, paths) in by_owner {
                // DEDUPE, because a pre-commit hook fires on every attempt and
                // a session that keeps retrying a blocked commit would
                // otherwise message the owner once per keystroke-adjacent
                // retry. Keyed on (owner, committer, path-set) for an hour.
                let path_names: Vec<String> = paths.iter().map(|(p, _)| p.clone()).collect();
                let key = format!("{owner}|{session}|{}", path_names.join(","));
                if !notify_once(&key) {
                    continue;
                }
                // Per path, say whether the owner's work is ALREADY COMMITTED. The
                // notice fires on edit records, which cannot distinguish "edited and
                // committed" from "edited and still staged" — so it used to hand the
                // recipient a `git log` to run every time. Now it runs it for them.
                let mut lines: Vec<String> = Vec::new();
                let mut all_settled = true;
                for (pth, age) in paths.iter().take(10) {
                    match path_fate(&wd, pth, &owner, *age).await {
                        PathFate::SettledByOwner(sha) => lines
                            .push(format!("  {pth}  — already committed by you in {sha}; nothing at risk")),
                        // Absorbed but SAFE. The bytes are in HEAD under someone
                        // else's commit, so the code is fine and only the
                        // reasoning is stranded — point at the card, do not send
                        // anyone hunting for lost work. Reporting this as at-risk
                        // is what cries wolf on every absorption that went fine.
                        PathFate::AbsorbedBy(sha, who) => lines.push(format!(
                            "  {pth}  — absorbed into {sha} under `{who}`; your CODE is safe, \
                             record the REASONING on the card"
                        )),
                        PathFate::AtRisk => {
                            all_settled = false;
                            lines.push(format!(
                                "  {pth}  — differs from HEAD and you have no commit for it; \
                                 the WORK ITSELF is at risk — CHECK THIS ONE"
                            ));
                        }
                    }
                }
                let list = lines.join("\n");
                let more = paths.len().saturating_sub(10);
                let verdict = if all_settled {
                    "\n\nEVERY path above is already committed by you, so this is almost certainly \
                     noise — the notice fires on EDIT RECORDS and cannot tell \"edited and committed\" \
                     from \"edited and staged\" on its own. Kept rather than suppressed because a \
                     false alarm costs you a glance and a missed one costs work."
                } else {
                    "\n\nAt least one path has no commit of yours since the edit — that is the one to \
                     reconcile."
                };
                let text = format!(
                    "[amux staged-guard] Session `{}` is committing in {} and the staged set \
                     includes {} file(s) whose edit records are YOURS:\n{}{}\n\n\
                     This is the mirror of the warning they got. If those are edits you had \
                     staged or in flight, they may land under THEIR commit message — the code \
                     usually survives, the reasoning does not.\n\n\
                     Check with:  git log -2 --stat -- {}\n\
                     If your work was absorbed, do not rewrite shared history — record the \
                     reasoning where it belongs (a follow-up commit, or the card) and say so.{}",
                    session,
                    wd_root,
                    paths.len(),
                    list,
                    if more > 0 { format!("\n  … and {more} more") } else { String::new() },
                    path_names.first().cloned().unwrap_or_default(),
                    verdict,
                );
                crate::api::session_verbs::steer_enqueue(st, &owner, &text, "staged-guard", "")
                    .await;
                tracing::warn!(
                    target: "staged_guard",
                    "[staged-guard/AMUX-2923] notified {owner}: {session} is staging {} of their path(s) in {}",
                    paths.len(), wd_root
                );
            }
        }
    }

    (
        StatusCode::OK,
        Json(
            Envelope { verdict: v, cotenants, window, degraded, hook_outdated, ..Default::default() }
                .json(),
        ),
    )
}

use crate::config::amux_home;

// ---------------------------------------------------------------------------
// Tests — every one of these fires the BLOCK path or a bypass of it. A guard
// whose refusal has never been observed is theatre, and this one has already
// regressed to silence twice (AC-261, then the rust cutover's 405).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    /// The victim-notice discriminator (2026-08-15). This decides whether a
    /// recipient is told "nothing at risk", so a WRONG `Some` is the dangerous
    /// direction — it tells someone to relax about work that was actually
    /// absorbed. Both directions are asserted against a real git repo, because
    /// the whole function is `git log` behaviour and a mock would only prove the
    /// mock.
    ///
    /// Authorship is the Amux-Session TRAILER, never %an: every lane on this
    /// machine commits as the same person, so %an cannot discriminate at all.
    /// The three-state refinement (amux, 2026-08-15). The first version keyed
    /// only on the VICTIM'S OWN trailer, so an absorption that had already
    /// happened cleanly — bytes safely in HEAD under the absorber's commit —
    /// reported as "NO commit of yours; CHECK THIS ONE". That is a false alarm
    /// on the most common outcome, and it points the reader at recovering work
    /// that was never lost. Absorbed-but-safe and about-to-be-swept need
    /// OPPOSITE responses: one is "record the reasoning on the card", the other
    /// is "your work is at risk".
    #[tokio::test]
    async fn path_fate_separates_absorbed_but_safe_from_genuinely_at_risk() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_string_lossy().to_string();
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&d).output().unwrap()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "Shared Name"]);

        // alice's work, committed by BOB (the absorption that already happened).
        std::fs::write(dir.path().join("absorbed.txt"), "alice wrote this\n").unwrap();
        git(&["add", "absorbed.txt"]);
        git(&["commit", "-q", "-m", "bob's message\n\nAmux-Session: bob"]);

        match path_fate(&d, "absorbed.txt", "alice", 3600).await {
            PathFate::AbsorbedBy(_, who) => assert_eq!(who, "bob",
                "must name WHO absorbed it, so the victim knows where the reasoning went"),
            other => panic!("expected AbsorbedBy, got {other:?} — this is the cry-wolf case"),
        }

        // alice's work that is NOT in HEAD: genuinely at risk.
        std::fs::write(dir.path().join("atrisk.txt"), "uncommitted\n").unwrap();
        assert_eq!(path_fate(&d, "atrisk.txt", "alice", 3600).await, PathFate::AtRisk);

        // and a file alice committed herself stays settled.
        std::fs::write(dir.path().join("mine.txt"), "alice\n").unwrap();
        git(&["add", "mine.txt"]);
        git(&["commit", "-q", "-m", "alice\n\nAmux-Session: alice"]);
        assert!(matches!(path_fate(&d, "mine.txt", "alice", 3600).await,
                         PathFate::SettledByOwner(_)));
    }

    #[tokio::test]
    async fn owner_committed_since_distinguishes_committed_from_still_staged() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_string_lossy().to_string();
        let git = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&d).output().unwrap()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "Shared Name"]);

        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "mine\n\nAmux-Session: alice"]);
        std::fs::write(dir.path().join("b.txt"), "one\n").unwrap();
        git(&["add", "b.txt"]);
        git(&["commit", "-q", "-m", "theirs\n\nAmux-Session: bob"]);

        // alice edited a.txt 3600s ago and HAS committed since -> settled.
        assert!(
            owner_committed_since(&d, "a.txt", "alice", 3600).await.is_some(),
            "alice committed a.txt after her edit; the notice must say nothing is at risk"
        );
        // bob has no commit touching a.txt at all -> unsettled, must NOT reassure.
        assert!(
            owner_committed_since(&d, "a.txt", "bob", 3600).await.is_none(),
            "bob never committed a.txt — reassuring him would be the dangerous direction"
        );
        // alice's commit is OLDER than a 0-second-old edit: she edited again
        // just now and has not committed it. Must not reassure.
        assert!(
            owner_committed_since(&d, "a.txt", "alice", 0).await.is_none(),
            "an edit newer than the last commit is exactly the at-risk case"
        );
        // %an is identical for both lanes, so a name-based check would pass all
        // three above; the trailer is what makes this discriminate.
        let log = String::from_utf8(git(&["log", "--format=%an"]).stdout).unwrap();
        assert!(log.lines().all(|l| l == "Shared Name"), "fixture must share %an: {log:?}");
    }

    use super::*;

    fn pair(rel: &str) -> (String, String) {
        (rel.to_string(), format!("/repo/{rel}"))
    }

    fn peer_wrote(path: &str, owner: &str, ts: f64) -> GuardInputs {
        let mut g = GuardInputs::default();
        g.theirs.insert(format!("/repo/{path}"), (owner.into(), ts));
        g.theirs_firsthand.insert(format!("/repo/{path}"));
        g
    }

    /// AMUX-3128 FIRING PATH: a read-only command must not mint an inferred edit,
    /// so a lane inspecting a peer's file is never flagged as its co-author. The
    /// reported specimen (`ls -t digests/`, `head -40 digests/<f>`) leads this list.
    #[test]
    fn read_only_commands_attribute_nothing() {
        for cmd in [
            "ls -t digests/",
            "head -40 digests/2026-08-15.md",
            "cat crates/foo.rs",
            "grep -n needle src/lib.rs",
            "tail -20 logs/server.log",
            "wc -l a.py",
            "stat file.json",
            "diff a.txt b.txt",
            "find . -name x.rs",
            "head foo.md | grep x",
            "cat a.md | head -5 | wc -l",
        ] {
            assert!(is_pure_read_command(cmd), "should be pure read: {cmd}");
        }
    }

    /// The other half of the discriminator, and the anti-regression guarantee:
    /// a real inferred WRITE (the case the mechanism exists for) is NOT pure-read,
    /// so it still falls through to the mtime gate and keeps attributing.
    #[test]
    fn writers_and_unrecognized_commands_fall_through() {
        for cmd in [
            "sed -i 's/a/b/' f.rs",
            "cat template > out.md",
            "echo hi >> log.txt",
            "cmd &> out.log",
            "cp a.txt b.txt",
            "mv x.md y.md",
            "tee report.md",
            "python3 - <<'PY'\nopen('x.md','w')\nPY",
            "head foo.md && sed -i s/a/b/ bar.rs",
            "sudo head secret.md",
        ] {
            assert!(!is_pure_read_command(cmd), "should NOT be pure read: {cmd}");
        }
    }

    /// AMUX-3128 RECURRED (gtm-ticker): a careful VERIFIER reads a digest with
    /// `git show` / `git log --stat` / `git diff` / `git grep` / `git blame`, whose
    /// verb is `git` (absent from READ_ONLY_VERBS), so each minted an inferred edit
    /// and flagged the reader as co-author — blocking the committer and PUNISHING
    /// verification (the harder they check, the more they block; trains to GN=1).
    /// The exact commands the reporting verifier uses lead this list; git WRITES
    /// must still fall through so a real mutation keeps its attribution.
    #[test]
    fn git_read_subcommands_attribute_nothing_but_writes_fall_through() {
        for cmd in [
            "git show HEAD:digests/2026-08-15.md",
            "git log --stat -- digests/2026-08-15.md",
            "git diff HEAD~1 -- digests/2026-08-15.md",
            "git grep -n breach digests/",
            "git blame digests/2026-08-15.md",
            "git -C /repo show abc123",
            "git --no-pager log --oneline -5",
            "git cat-file -p HEAD:a.md",
            "git show X | grep -n row",
        ] {
            assert!(is_pure_read_command(cmd), "git read should attribute nothing: {cmd}");
        }
        for cmd in [
            "git add digests/2026-08-15.md",
            "git commit -m x",
            "git checkout -- digests/2026-08-15.md",
            "git reset --hard HEAD",
            "git restore digests/2026-08-15.md",
            "git apply patch.diff",
            "git stash pop",
            "git",
            "git show X && git add Y",
        ] {
            assert!(!is_pure_read_command(cmd), "git write/unknown must fall through: {cmd}");
        }
    }

    #[test]
    fn output_redirection_is_a_write_but_fd_dup_is_not() {
        assert!(has_output_redirection("echo x > f.txt"));
        assert!(has_output_redirection("cat a >> b"));
        assert!(has_output_redirection("cmd &> out.log"));
        assert!(has_output_redirection("foo > $LOG"));
        assert!(!has_output_redirection("make 2>&1"));
        assert!(!has_output_redirection("cmd >&2"));
        assert!(!has_output_redirection("head -40 digests/x.md"));
        assert!(!has_output_redirection("ls -t digests/"));
    }

    /// THE FIRING PATH. A staged file a peer wrote and this session never
    /// touched must BLOCK — this is the case the guard exists for and the case
    /// that has been silently unenforced since the cutover.
    #[test]
    fn peer_edited_path_blocks() {
        let inp = peer_wrote("amux-server.py", "peer-lane", 1000.0);
        let v = classify(&[pair("amux-server.py")], 1600.0, 21600.0, &inp);
        assert_eq!(v.foreign.len(), 1, "a peer's staged file MUST block");
        assert_eq!(v.foreign[0]["owner"], json!("peer-lane"));
        assert_eq!(v.foreign[0]["age_secs"], json!(600));
        // The `why` must not assert a claim the committer does not have. This
        // assertion failed on the first cut — python's wording said "your claim
        // is inferred" even with an EMPTY claim set — which is what the message
        // is for: telling the reader what was actually compared.
        let why = v.foreign[0]["why"].as_str().unwrap();
        assert!(why.contains("no edit record on this path"), "misleading why: {why}");
        assert!(why.contains("edit records, not the staged diff"), "why: {why}");
        assert!(v.shared.is_empty() && v.unclaimed.is_empty());
    }

    /// A staged DELETION of a peer's file blocks identically — the path no
    /// longer exists on disk, which is why `realpath` must not require it to.
    #[test]
    fn peer_deleted_path_still_blocks() {
        let inp = peer_wrote("gone.rs", "peer-lane", 1500.0);
        let v = classify(&[pair("gone.rs")], 1600.0, 21600.0, &inp);
        assert_eq!(v.foreign.len(), 1, "a staged deletion of a peer's file MUST block");
    }

    /// AF-19, the regression that let 762e06e through: an INFERRED self-claim (a
    /// Bash command whose mtime move coincided with the peer's write) must not
    /// outrank a peer's first-hand write. Reconciled with AF-27: the real 762e06e
    /// is a TIE — the inferred claim IS the peer's concurrent write, same mtime
    /// event, so committer_ts == peer_ts. A claim no fresher than the peer's does
    /// not prove ownership, so the tie still BLOCKS. (The original fixture used a
    /// clearly-fresher committer, which AF-27 showed is a different case that must
    /// NOT block — see `a_clearly_fresher_self_edit_beats_a_stale_firsthand_peer`.)
    #[test]
    fn inferred_self_claim_no_fresher_than_the_peer_still_blocks() {
        let mut inp = peer_wrote("test_x.py", "peer-lane", 1000.0);
        inp.mine.insert("/repo/test_x.py".into(), 1000.0); // inferred, same instant as the peer
        let v = classify(&[pair("test_x.py")], 1600.0, 21600.0, &inp);
        assert_eq!(v.foreign.len(), 1, "an inferred self-claim no fresher than the peer must still block");
        assert!(v.foreign[0]["why"].as_str().unwrap().contains("your claim is inferred"));
    }

    /// AF-27: the committer's OWN edit is clearly fresher than the peer's stale
    /// first-hand one (real case: committer 23.8s ago, peer 14,355s / 4h ago). The
    /// committer edited AFTER the peer and owns the current content, so this must
    /// be SHARED (warned), never foreign. amux-frustrations' per-path forensic
    /// isolated exactly this shape — 6 of 405 verdicts (in_mine=true,
    /// in_firsthand=false, committer fresher) — from the 399 true positives.
    #[test]
    fn a_clearly_fresher_self_edit_beats_a_stale_firsthand_peer() {
        let mut inp = peer_wrote("hand_raiser.py", "peer-lane", 1000.0); // peer, older
        inp.mine.insert("/repo/hand_raiser.py".into(), 1580.0); // committer, clearly fresher
        let v = classify(&[pair("hand_raiser.py")], 1600.0, 21600.0, &inp);
        assert!(v.foreign.is_empty(), "a clearly fresher self-edit must not block: {:?}", v.foreign);
        assert_eq!(v.shared.len(), 1, "it is shared (warned), not foreign");
        assert_eq!(v.shared[0]["peer"], json!(true));
    }

    /// AF-27 margin (amux-frustrations forensic): the one coincident near-tie was
    /// 0.2s, a sign that flips across the disk-mtime/transcript-ts clocks. A self
    /// edit fresher by LESS than the skew margin is not meaningfully fresher and
    /// must STILL block — else the 762e06e tie reopens on the next coincident write.
    #[test]
    fn a_within_skew_fresher_self_edit_still_blocks() {
        let mut inp = peer_wrote("coincident.rs", "peer-lane", 1000.0);
        // Fresher by (margin - 1)s — inside the skew, so it does NOT count.
        inp.mine.insert("/repo/coincident.rs".into(), 1000.0 + RECENCY_SKEW_MARGIN_S - 1.0);
        let v = classify(&[pair("coincident.rs")], 1600.0, 21600.0, &inp);
        assert_eq!(v.foreign.len(), 1, "a within-skew freshness lead must still block");
    }

    /// The legitimate claim: both sessions really edited it. Warned, never
    /// blocked — blocking would deadlock a genuinely shared file.
    #[test]
    fn firsthand_on_both_sides_is_shared_not_foreign() {
        let mut inp = peer_wrote("shared.rs", "peer-lane", 1000.0);
        inp.mine.insert("/repo/shared.rs".into(), 1200.0);
        inp.mine_firsthand.insert("/repo/shared.rs".into());
        let v = classify(&[pair("shared.rs")], 1600.0, 21600.0, &inp);
        assert!(v.foreign.is_empty());
        assert_eq!(v.shared.len(), 1);
        assert_eq!(v.shared[0]["peer"], json!(true));
    }

    /// AF-24: your OWN file with unstaged changes is not a co-edit. `peer`
    /// false is what stops the renderer inventing a session named "(unknown)".
    #[test]
    fn own_dirty_file_is_shared_without_a_phantom_peer() {
        let mut inp = GuardInputs::default();
        inp.mine.insert("/repo/mine.rs".into(), 1200.0);
        inp.mine_firsthand.insert("/repo/mine.rs".into());
        inp.dirty.insert("/repo/mine.rs".into());
        let v = classify(&[pair("mine.rs")], 1600.0, 21600.0, &inp);
        assert_eq!(v.shared.len(), 1);
        assert_eq!(v.shared[0]["peer"], json!(false));
        assert_eq!(v.shared[0]["owner"], json!("(unknown)"));
        assert_eq!(v.shared[0]["has_unstaged_changes"], json!(true));
    }

    /// THE BELT (AMUX-2443): staged, no record from anyone. Not blocked, but
    /// never silent — silence is what made 762e06e a sweep.
    #[test]
    fn unrecorded_path_blocks_because_unknown_is_not_safe() {
        // CONTRACT CHANGE, AC-355. This test previously asserted the opposite —
        // foreign empty, unclaimed populated, i.e. warn-and-ship — and it was
        // encoding the bug: "no session can account for this staged file" was
        // treated as safe because there was no owner to defer to. Two sweeps in
        // one day went through that gap, including the commit that was fixing
        // this guard's own notice.
        //
        // Kept and inverted rather than deleted, so the change of contract is
        // visible in the diff to anyone who wonders why unclaimed now blocks.
        // Blind cotenant present: this is the sweep case, and it must BLOCK.
        let blind = GuardInputs { blind_cotenant: true, ..Default::default() };
        let v = classify(&[pair("mystery.txt")], 1600.0, 21600.0, &blind);
        assert_eq!(v.foreign.len(), 1, "an unattributable staged path must BLOCK");
        assert_eq!(v.foreign[0]["path"], json!("mystery.txt"));
        assert_eq!(v.foreign[0]["owner"], json!(""), "no owner can be named — that IS the finding");
        // The refusal must be actionable or it gets routed around: name the
        // escape and the exact diff command (the AMUX-2325 lesson).
        let why = v.foreign[0]["why"].as_str().unwrap();
        assert!(why.contains("AMUX_VERIFIED_SOLO"), "escape not named: {why}");
        assert!(why.contains("git diff --cached"), "no way to check it: {why}");
        // It blocks via `foreign` specifically, because that is the only field
        // installed hooks act on (module docs). A new key would be ignored by
        // every hook already on disk.
        assert!(v.unclaimed.is_empty(), "must not ALSO sit in the non-blocking list");

        // FULLY ATTRIBUTED checkout: nobody is hidden, so the same path is NOT a
        // sweep — it is most often the committer's own pre-window or generated
        // work. It stays a warning. Without this scoping the block adds a class
        // to a guard already refusing ~18/hour, which AMUX-2936 measured and
        // named as what gets the guard disabled outright.
        let clear = classify(&[pair("mystery.txt")], 1600.0, 21600.0, &GuardInputs::default());
        assert!(clear.foreign.is_empty(), "must NOT block when every cotenant is visible");
        assert_eq!(clear.unclaimed.len(), 1, "still disclosed, just not blocking");
    }

    /// The envelope shape the installed hooks parse. Every key on every path,
    /// including the short circuits: a hook reading `d["shared"]` gets `[]`.
    #[test]
    fn envelope_carries_every_key_the_hook_reads() {
        let v = Envelope { window: 21600.0, hook_outdated: true, ..Default::default() }.json();
        for k in ["ok", "foreign", "shared", "unclaimed", "cotenants", "window_secs"] {
            assert!(v.get(k).is_some(), "missing key the installed hook reads: {k}");
        }
        assert!(v["foreign"].is_array() && v["shared"].is_array() && v["unclaimed"].is_array());
        assert_eq!(v["undecided"], json!(false));
        assert_eq!(v["hook_outdated"], json!(true));
    }

    /// `undecided` must never look like a clean verdict.
    #[test]
    fn undecided_is_distinguishable_from_all_clear() {
        let v = Envelope {
            window: 21600.0,
            undecided: Some("cotenants unknown".into()),
            ..Default::default()
        }
        .json();
        assert_eq!(v["undecided"], json!(true));
        assert_eq!(v["reason"], json!("cotenants unknown"));
        assert!(v["foreign"].as_array().unwrap().is_empty());
    }

    /// `realpath` on a path that does not exist must still resolve its
    /// existing ancestors — otherwise staged deletions key differently from
    /// the transcript records and drop out of the comparison entirely.
    #[test]
    fn realpath_resolves_nonexistent_leaf() {
        let dir = std::env::temp_dir();
        let real_dir = dir.canonicalize().unwrap();
        let got = realpath(&dir.join("definitely-not-here-amux-guard.rs"));
        assert_eq!(got, real_dir.join("definitely-not-here-amux-guard.rs").to_string_lossy());
    }

    #[test]
    fn window_has_a_floor() {
        // py:19187: max(600, env). A tiny window would make almost everything
        // `unclaimed` and nothing `foreign` — a guard that never blocks.
        assert!(window_secs() >= WINDOW_FLOOR);
    }
}
