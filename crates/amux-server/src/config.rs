//! Server configuration: `~/.amux/server.env` + environment + defaults
//! (RR-0020, Invariant 2).
//!
//! Precedence (highest wins): process environment > server.env > defaults.
//! This mirrors the Python server's `os.environ.setdefault` semantics so the
//! same server.env file drives both servers during the strangler-fig
//! migration. The four-tier Org/Global/Group/Worker resolution for
//! worker-scoped config happens in amux-core's scope module against DB rows;
//! this file only handles PROCESS-level configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The port a server binds when nothing says otherwise.
///
/// 8823 originally meant "not 8822, which Python owns". Python retired
/// (792ce1f) and the installed service now answers 8822 AND 8824 — but the
/// value stays, for a different and still-live reason: `cargo run -p
/// amux-server` on a dev machine must not collide with the running service.
/// The launchd agent sets `AMUX_RS_PORT` explicitly; nothing in production
/// depends on this default.
///
/// The CLIENT default deliberately differs (`DEFAULT_CLIENT_URL` in amux-cli
/// points at the installed port, 8824): a client's job is to reach the server
/// that IS running, and pointing it here is what made every bare `amux-rs`
/// invocation fail with a connection error indistinguishable from the server
/// being down (AMUX-2672).
pub const DEFAULT_PORT: u16 = 8823;

/// The port THIS server is actually answering on — the one a client should be
/// told to call.
///
/// Reads the same `AMUX_RS_PORT` that [`ServerConfig::load`] does, which is
/// safe from anywhere because that load exports server.env into the process env
/// (setdefault) before anything else runs. Deliberately NOT the legacy port and
/// NOT a literal.
///
/// This exists because the literal was the bug. `session_verbs` hardcoded
/// `AMUX_URL=https://localhost:8822` into every tmux lane it started, which
/// pinned two deployments to one number at once: the local install (8824) could
/// not retire the legacy address while new sessions kept minting it, and the
/// cloud image had to bind 8822 *because* of the hardcode — its Dockerfile said
/// so in a comment, naming this exact line. One env-derived accessor lets each
/// deployment answer for itself, with no build-time branch (the single-codebase
/// rule) and nothing to keep in step.
pub fn canonical_port() -> u16 {
    std::env::var("AMUX_RS_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// `~/.amux` — sessions dir, DB, TLS material, tokens.
    pub amux_home: PathBuf,
    pub port: u16,
    /// Path to the SQLite database (shared with the Python server).
    pub db_path: PathBuf,
    /// Everything from server.env plus process env overlays, for
    /// worker-environment assembly later.
    pub env: BTreeMap<String, String>,
}

impl ServerConfig {
    /// Load configuration. Pure given its inputs — callers pass the home dir
    /// and process env so tests can drive it hermetically.
    pub fn load(home: PathBuf, process_env: &BTreeMap<String, String>) -> Self {
        let mut env = parse_env_file(&home.join("server.env"));
        // PYTHON-PARITY SETDEFAULT, for real: export server.env values into
        // the PROCESS env when the process doesn't already set them. The doc
        // above always claimed setdefault semantics, but values only reached
        // the Config struct — every `std::env::var()` read site (the
        // AMUX_RS_SCHEDULER gate, AMUX_HERDR_SESSION, the caps/knobs) saw
        // nothing, so server.env flags silently didn't work (live incident
        // 2026-08-09: scheduler stayed in shadow mode with the flag set).
        for (k, v) in env.iter() {
            if !process_env.contains_key(k) && std::env::var_os(k).is_none() {
                std::env::set_var(k, v);
            }
        }
        // Process env wins over server.env (same rule as Python's setdefault).
        for (k, v) in process_env {
            env.insert(k.clone(), v.clone());
        }
        let port = env
            .get("AMUX_RS_PORT")
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_PORT);
        let db_path = env
            .get("AMUX_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("amux.db"));
        ServerConfig {
            amux_home: home,
            port,
            db_path,
            env,
        }
    }

    pub fn from_process_env() -> Self {
        let process_env: BTreeMap<String, String> = std::env::vars().collect();
        Self::load(amux_home(), &process_env)
    }

    pub fn tls_dir(&self) -> PathBuf {
        self.amux_home.join("tls")
    }

    /// Python's `_AUTH_TOKEN_FILE` (amux-server.py:700): `auth_token`,
    /// UNDERSCORE. This crate shipped reading `auth-token` (dash) and minted
    /// its own token there, so every client holding the real shared token got
    /// 401s from this server while the auth docstring claimed the file was
    /// shared. The stale dash-file may still exist on machines that ran the
    /// old build; nothing reads it anymore.
    pub fn auth_token_path(&self) -> PathBuf {
        self.amux_home.join("auth_token")
    }
}

/// THE amux home: `$AMUX_HOME`, legacy `$CC_HOME`, else `~/.amux`.
///
/// One resolver, because there were TEN and they did not agree (AMUX-2919).
/// Nine private copies plus `from_process_env` had drifted into three distinct
/// behaviours, and the divergences were the interesting part:
///
///   * **`AMUX_HOME=""` was treated as SET by nine of the ten.**
///     `std::env::var` returns `Ok("")` for an exported-but-empty variable, so
///     `PathBuf::from("")` produced an EMPTY path and every `amux_home().join(x)`
///     silently became the RELATIVE path `x` — writing the DB, tls dir and auth
///     token wherever the process happened to be cwd'd. Only api/settings.rs
///     checked for empty. That check is now the shared one.
///   * **`CC_HOME` was honoured by exactly one of the ten** (api/settings.rs,
///     which serves settings/journal/history). So with `CC_HOME` set and
///     `AMUX_HOME` unset, settings read one home while groups, dictation, push
///     and the rest read another — one server, two data directories. `CC_HOME`
///     is unset on this machine today, so unifying on it changes nothing now
///     and closes the split-brain if anything ever sets it. The bash `amux` CLI
///     already honours it (amux:35), which is where the divergence would bite.
///   * **The `$HOME`-missing fallback split** `unwrap_or_default()` (→ the
///     relative `.amux`) against `PathBuf::from("/")` (→ `/.amux`). Now `/.amux`
///     everywhere: an absolute path is wrong loudly, a relative one is wrong
///     silently.
///
/// api/settings.rs's docstring claimed it matched `from_process_env`; it did
/// not, in both of the ways above. That claim is now true because there is only
/// one implementation left to be true about (ethos rule 6).
pub fn amux_home() -> PathBuf {
    resolve_home(|k| std::env::var(k).ok())
}

/// The resolution itself, over an injected lookup.
///
/// Split out so the rules above are testable WITHOUT setting process env:
/// `std::env::set_var` is global and `cargo test` runs threads in parallel, so
/// an env-mutating test races every other test that reads a home — which is
/// most of them. A test that must run single-threaded to be correct is one
/// that will be made green by `--test-threads=1` and quietly stop
/// discriminating (ethos rule 7).
fn resolve_home(get: impl Fn(&str) -> Option<String>) -> PathBuf {
    for var in ["AMUX_HOME", "CC_HOME"] {
        match get(var) {
            Some(h) if !h.is_empty() => return PathBuf::from(h),
            _ => {}
        }
    }
    match get("HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h).join(".amux"),
        _ => PathBuf::from("/").join(".amux"),
    }
}

#[cfg(test)]
mod home_resolution_tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn amux_home_wins_and_cc_home_is_the_legacy_fallback() {
        assert_eq!(
            resolve_home(env(&[("AMUX_HOME", "/a"), ("CC_HOME", "/c"), ("HOME", "/h")])),
            PathBuf::from("/a")
        );
        assert_eq!(
            resolve_home(env(&[("CC_HOME", "/c"), ("HOME", "/h")])),
            PathBuf::from("/c"),
            "CC_HOME was honoured by exactly one of the ten old copies — settings read \
             one home while groups/dictation/push read another"
        );
        assert_eq!(resolve_home(env(&[("HOME", "/h")])), PathBuf::from("/h/.amux"));
    }

    /// The bug that made this consolidation worth doing. An exported-but-empty
    /// variable yields `Ok("")`, and nine of the ten copies mapped that
    /// straight to `PathBuf::from("")` — an EMPTY path, so every `.join(x)`
    /// became the RELATIVE path `x` and the DB, tls dir and auth token landed
    /// wherever the process was cwd'd. Silent, and cwd-dependent.
    #[test]
    fn an_exported_but_empty_var_is_not_a_home() {
        assert_eq!(
            resolve_home(env(&[("AMUX_HOME", ""), ("HOME", "/h")])),
            PathBuf::from("/h/.amux")
        );
        assert_eq!(
            resolve_home(env(&[("AMUX_HOME", ""), ("CC_HOME", ""), ("HOME", "/h")])),
            PathBuf::from("/h/.amux")
        );
        // The shape the old code produced, asserted as NOT happening: a
        // relative path is the failure mode, so name it explicitly.
        let got = resolve_home(env(&[("AMUX_HOME", ""), ("HOME", "/h")]));
        assert!(got.is_absolute(), "an empty AMUX_HOME must not yield a relative home: {got:?}");
    }

    /// `$HOME` missing split the old copies too: `unwrap_or_default()` gave the
    /// relative `.amux`, `PathBuf::from("/")` gave `/.amux`. Absolute wins —
    /// wrong loudly beats wrong silently.
    #[test]
    fn a_missing_home_still_resolves_absolute() {
        let got = resolve_home(env(&[]));
        assert_eq!(got, PathBuf::from("/.amux"));
        assert!(got.is_absolute());
    }
}

/// Parse a KEY=VALUE env file. Supports `#` comments, blank lines, single or
/// double quoted values, and `export ` prefixes — the shapes that appear in
/// real server.env files today.
pub fn parse_env_file(path: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(content) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let mut v = v.trim();
        if (v.starts_with('"') && v.ends_with('"') && v.len() >= 2)
            || (v.starts_with('\'') && v.ends_with('\'') && v.len() >= 2)
        {
            v = &v[1..v.len() - 1];
        }
        if !k.is_empty() {
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_file_shapes() {
        let dir = std::env::temp_dir().join(format!("amux-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("server.env");
        std::fs::write(
            &p,
            "# comment\nAMUX_S3_BUCKET=my-bucket\nQUOTED=\"has spaces\"\nexport EXPORTED='single'\n\nBROKEN_LINE\n",
        )
        .unwrap();
        let env = parse_env_file(&p);
        assert_eq!(env.get("AMUX_S3_BUCKET").unwrap(), "my-bucket");
        assert_eq!(env.get("QUOTED").unwrap(), "has spaces");
        assert_eq!(env.get("EXPORTED").unwrap(), "single");
        assert!(!env.contains_key("BROKEN_LINE"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn process_env_beats_server_env() {
        let dir = std::env::temp_dir().join(format!("amux-cfg-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.env"), "AMUX_RS_PORT=9000\nA=file\n").unwrap();
        let mut penv = BTreeMap::new();
        penv.insert("AMUX_RS_PORT".to_string(), "9001".to_string());
        let cfg = ServerConfig::load(dir.clone(), &penv);
        assert_eq!(cfg.port, 9001);
        assert_eq!(cfg.env.get("A").unwrap(), "file");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn defaults_when_nothing_set() {
        let dir = std::env::temp_dir().join(format!("amux-cfg-test3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = ServerConfig::load(dir.clone(), &BTreeMap::new());
        assert_eq!(cfg.port, DEFAULT_PORT);
        assert_eq!(cfg.db_path, dir.join("amux.db"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

// ── Shared env parsing (AMUX-2919) ─────────────────────────────────────────
// These were duplicated verbatim across runtime_jobs/board_drive.rs,
// runtime_jobs/autofix.rs and api/git_guard.rs. Unlike `amux_home` above, the
// copies genuinely WERE identical, so this consolidation is mechanical.

/// `$KEY` parsed as f64, trimmed, falling back to `default`.
pub fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

/// `$KEY` parsed as i64, trimmed, falling back to `default`.
pub fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

/// Unix epoch seconds as f64. Three identical copies (board_drive, alerts,
/// session_verbs).
///
/// NOT to be conflated with the two `now_secs()` functions, which return
/// DIFFERENT TYPES from different clocks — api/upload.rs returns u64 from
/// SystemTime, api/board.rs returns i64 from chrono::Utc. They share a name and
/// nothing else; merging them on the strength of the name is the mistake this
/// comment exists to prevent.
pub fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
