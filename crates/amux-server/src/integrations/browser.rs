//! Native Chrome profile management + launch (RR-0092; plan lesson L7).
//!
//! Profile locations are the PYTHON server's, not a new scheme — L7's whole
//! lesson is that create-path and use-path must be the same bytes, and the
//! existing logged-in profiles live where `_bu_profile_dir` in amux-server.py
//! resolves them (NOT under `~/.amux/browser-profiles/`, which nothing ever
//! used — verified by grep before writing this):
//! - `default` (or empty)  -> `<amux_home>/playwright-auth/profile`
//! - named, legacy         -> `<amux_home>/playwright-auth/profiles/<name>`
//!   when that directory exists (the 35+ real logged-in profiles are here)
//! - named, otherwise      -> `<chrome-user-data-dir>/<name>` (a Chrome
//!   `--profile-directory` inside the user's real Chrome data dir)
//!
//! NEW profiles are created under `playwright-auth/profiles/<name>` — the
//! amux-owned location — because this launcher passes `--user-data-dir`
//! straight at the profile dir. (Python's create targets the shared Chrome
//! dir for browser-use interop; the native launcher IS the consumer here, so
//! amux-owned is the location whose create-path == use-path.) Existing
//! Chrome-dir profiles still resolve and launch via
//! `--user-data-dir=<chrome dir> --profile-directory=<name>`, exactly like
//! `_bu_profile_launch_target`.
//!
//! Lock-file hygiene: Chrome drops `SingletonLock`/`SingletonCookie`/
//! `SingletonSocket` on a clean exit and leaves them when killed; a stale set
//! blocks the next launch (AMUX-2070). We clean them on demand and at startup
//! reconcile — but ONLY inside amux-owned dirs (`playwright-auth/...`). The
//! real Chrome user-data-dir is the human's live browser; deleting its locks
//! while it runs would corrupt their session, so it is never touched.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Chrome removes these on clean exit; their presence after exit means the
/// profile was never flushed (Python `_CHROME_SINGLETONS`).
pub const CHROME_SINGLETONS: [&str; 3] = ["SingletonLock", "SingletonCookie", "SingletonSocket"];

pub use crate::config::amux_home;

/// The user's real Chrome user-data-dir (Python `_chrome_user_data_dir`).
pub fn chrome_user_data_dir() -> PathBuf {
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Google/Chrome")
    } else if cfg!(windows) {
        std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or(home)
            .join("Google/Chrome/User Data")
    } else {
        home.join(".config/google-chrome")
    }
}

/// Chrome user profiles from `Local State` → `profile.info_cache`.
/// Returns directory names ("Default", "Profile 11", …) sorted.
pub fn list_chrome_profiles() -> Vec<String> {
    let local_state = chrome_user_data_dir().join("Local State");
    let data = match std::fs::read_to_string(&local_state) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let parsed: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let Some(cache) = parsed
        .get("profile")
        .and_then(|p| p.get("info_cache"))
        .and_then(|c| c.as_object())
    else {
        return vec![];
    };
    let mut names: Vec<String> = cache.keys().cloned().collect();
    names.sort();
    names
}

/// Python `_bu_profile_dir`: where a profile name lives on disk. Pure in its
/// inputs so tests can drive it hermetically.
pub fn resolve_profile_dir(home: &Path, chrome_dir: &Path, name: &str) -> PathBuf {
    let n = name.trim();
    if n.is_empty() || n == "default" {
        return home.join("playwright-auth").join("profile");
    }
    let legacy = home.join("playwright-auth").join("profiles").join(n);
    if legacy.is_dir() {
        return legacy;
    }
    chrome_dir.join(n)
}

/// How to hand a resolved profile dir to Chrome (Python
/// `_bu_profile_launch_target`): a dir nested in the Chrome user-data-dir is
/// a `--profile-directory`, an amux-owned dir IS the `--user-data-dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchTarget {
    pub user_data_dir: PathBuf,
    pub profile_directory: Option<String>,
}

pub fn launch_target(home: &Path, chrome_dir: &Path, name: &str) -> LaunchTarget {
    let d = resolve_profile_dir(home, chrome_dir, name);
    if d.parent() == Some(chrome_dir) {
        LaunchTarget {
            user_data_dir: chrome_dir.to_path_buf(),
            profile_directory: d.file_name().map(|s| s.to_string_lossy().into_owned()),
        }
    } else {
        LaunchTarget { user_data_dir: d, profile_directory: None }
    }
}

/// Is this dir one amux owns outright (safe to create, clean locks in,
/// delete)? Anything else — notably the real Chrome user-data-dir — is the
/// human's and is never mutated beyond what Chrome itself does.
pub fn is_amux_owned(home: &Path, dir: &Path) -> bool {
    dir.starts_with(home.join("playwright-auth"))
}

// ---- inventory ------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct BrowserProfile {
    pub name: String,
    pub path: String,
    /// Unix seconds from the profile dir's mtime — Chrome touches the dir on
    /// use, so this is "when was this profile last opened", cheaply.
    pub last_used: Option<i64>,
    /// Only when the caller asked (`?sizes=1`): walking a 565MB profile took
    /// 2.9s in the Python listing and starved the picker, so sizes stay
    /// opt-in here too.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_mb: Option<f64>,
    /// Registry metadata (`playwright-auth/profiles.json`, shared with the
    /// Python server): which domains this profile is signed into.
    pub domains: Vec<String>,
    pub label: String,
    pub registered: bool,
}

fn dir_mtime_unix(p: &Path) -> Option<i64> {
    let modified = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
}

/// Recursive on-disk size without a walkdir dep (profiles are a few hundred
/// files; the Python listing does the same walk).
pub fn dir_size_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// Registry metadata (Python `_bu_registry_load`): name -> {domains, label}.
fn registry_load(home: &Path) -> serde_json::Map<String, serde_json::Value> {
    let path = home.join("playwright-auth").join("profiles.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Inventory of amux-owned profiles: a directory that exists IS a profile
/// (the Python listing's hard-won rule — the registry adds metadata, it is
/// not the source of truth for existence).
pub fn list_profiles(home: &Path, with_sizes: bool) -> Vec<BrowserProfile> {
    let registry = registry_load(home);
    let mut names: std::collections::BTreeSet<String> =
        registry.keys().cloned().collect();
    let profiles_dir = home.join("playwright-auth").join("profiles");
    if let Ok(entries) = std::fs::read_dir(&profiles_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() && !name.starts_with('.') {
                names.insert(name);
            }
        }
    }
    if home.join("playwright-auth").join("profile").is_dir() {
        names.insert("default".into());
    }

    let chrome_dir = chrome_user_data_dir();
    names
        .into_iter()
        .map(|name| {
            let dir = resolve_profile_dir(home, &chrome_dir, &name);
            let meta = registry.get(&name).and_then(|v| v.as_object());
            BrowserProfile {
                path: dir.display().to_string(),
                last_used: dir_mtime_unix(&dir),
                // 3-decimal precision: a 2KB profile must not round to 0.0
                // (a nonzero directory reading as empty is a lie; display
                // rounding is the UI's call).
                size_mb: with_sizes
                    .then(|| (dir_size_bytes(&dir) as f64 / (1024.0 * 1024.0) * 1000.0).round() / 1000.0),
                domains: meta
                    .and_then(|m| m.get("domains"))
                    .and_then(|d| d.as_array())
                    .map(|a| {
                        a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
                    })
                    .unwrap_or_default(),
                label: meta
                    .and_then(|m| m.get("label"))
                    .and_then(|l| l.as_str())
                    .unwrap_or("")
                    .to_string(),
                registered: meta.is_some(),
                name,
            }
        })
        .collect()
}

// ---- lock files -----------------------------------------------------------

/// Which Singleton* entries are present. `symlink_metadata` (lstat), not
/// `exists()`: the entries are symlinks pointing at `<host>-<pid>`, and
/// `exists()` follows the link — a live lock with an unresolvable target
/// would read as absent exactly when the check matters (Python
/// `_chrome_locks_present` carries the same comment).
pub fn locks_present(dir: &Path) -> Vec<String> {
    CHROME_SINGLETONS
        .iter()
        .filter(|n| dir.join(n).symlink_metadata().is_ok())
        .map(|n| n.to_string())
        .collect()
}

/// Remove Singleton* entries from ONE dir. Callers are responsible for only
/// pointing this at amux-owned dirs whose Chrome is known dead.
pub fn clean_locks(dir: &Path) -> Vec<String> {
    let mut removed = Vec::new();
    for n in CHROME_SINGLETONS {
        let p = dir.join(n);
        if p.symlink_metadata().is_ok() && std::fs::remove_file(&p).is_ok() {
            removed.push(n.to_string());
        }
    }
    removed
}

/// Startup reconcile: at boot no amux-launched Chrome exists, so any lock in
/// an amux-owned profile dir is stale by definition and blocks the next
/// launch. Returns (dir, removed-locks) pairs so the caller can log WHAT was
/// cleaned, not just that cleaning happened (ethos rule 4).
pub fn reconcile_locks_at_startup(home: &Path) -> Vec<(PathBuf, Vec<String>)> {
    let mut dirs = vec![home.join("playwright-auth").join("profile")];
    if let Ok(entries) = std::fs::read_dir(home.join("playwright-auth").join("profiles")) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                dirs.push(e.path());
            }
        }
    }
    let mut cleaned = Vec::new();
    for dir in dirs {
        debug_assert!(is_amux_owned(home, &dir));
        let removed = clean_locks(&dir);
        if !removed.is_empty() {
            cleaned.push((dir, removed));
        }
    }
    cleaned
}

// ---- Chrome launch + CDP over HTTP ----------------------------------------

/// The one browser this server has running (the dashboard's start/stop model
/// is a single session). Starting while one runs stops the old one first.
pub struct RunningBrowser {
    pub profile: String,
    pub user_data_dir: PathBuf,
    pub cdp_port: u16,
    pub started_at: i64,
    /// Always known. `child` is None for a browser ADOPTED after a server
    /// restart — we did not spawn it, so we have no handle, but we can still
    /// speak CDP to it and still kill it by pid.
    pub pid: u32,
    child: Option<tokio::process::Child>,
}

/// Where the handle survives a restart (AC-325).
///
/// The registry was in-process ONLY, so every server rebuild turned a live
/// browser into "no amux-launched browser is running" while Chrome kept
/// running — orphaned and unreachable at the same time. On a shared checkout
/// where peers' compile loops restart this server constantly, that killed every
/// browser sequence longer than a few seconds, which is every UI verification
/// (start -> auth -> switch org -> navigate -> click -> read). Two sessions
/// spent two days concluding the rig was flaky; it was being killed on a
/// schedule set by other lanes.
///
/// This is AC-296 in rust: python's driver registry had the same in-memory
/// assumption and was fixed by persisting the marker.
fn running_state_path(home: &Path) -> PathBuf {
    home.join("browser-running.json")
}

fn persist_running(home: &Path, profile: &str, dir: &Path, port: u16, pid: u32, started: i64) {
    let v = serde_json::json!({
        "profile": profile,
        "user_data_dir": dir.to_string_lossy(),
        "cdp_port": port,
        "pid": pid,
        "started_at": started,
    });
    let _ = std::fs::write(running_state_path(home), v.to_string());
}

fn clear_running(home: &Path) {
    let _ = std::fs::remove_file(running_state_path(home));
}

/// Re-adopt a browser this server did not spawn, if one is still there.
///
/// Verified against the LIVE process before adopting: the pid must exist AND
/// its CDP port must answer. A stale file must never resurrect a dead browser —
/// that would replace an honest "not running" with a confident wrong handle,
/// which is worse than the bug being fixed.
pub async fn adopt_if_orphaned(home: &Path) -> bool {
    if RUNNING.lock().expect("browser registry poisoned").is_some() {
        return false;
    }
    let Ok(raw) = std::fs::read_to_string(running_state_path(home)) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else { return false };
    let port = v.get("cdp_port").and_then(serde_json::Value::as_u64).unwrap_or(0) as u16;
    let pid = v.get("pid").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
    if port == 0 || pid == 0 {
        return false;
    }
    // Does CDP still answer? That is the only proof that matters — a live pid
    // whose Chrome has wedged is not an adoptable browser.
    if cdp_list(port).await.is_err() {
        clear_running(home);
        return false;
    }
    let profile = v.get("profile").and_then(serde_json::Value::as_str).unwrap_or("default").to_string();
    let dir = v
        .get("user_data_dir")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_default();
    let started = v.get("started_at").and_then(serde_json::Value::as_i64).unwrap_or(0);
    *RUNNING.lock().expect("browser registry poisoned") = Some(RunningBrowser {
        profile,
        user_data_dir: dir,
        cdp_port: port,
        started_at: started,
        pid,
        child: None,
    });
    tracing::info!(pid, port, "browser: adopted an orphan left by a previous server process");
    true
}

pub static RUNNING: LazyLock<Mutex<Option<RunningBrowser>>> = LazyLock::new(|| Mutex::new(None));

/// Locate a Chrome/Chromium binary. None is an honest answer the API
/// surfaces as 501 — not a fallback to some other browser.
pub fn chrome_binary() -> Option<PathBuf> {
    let fixed = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    for c in fixed {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    let names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"];
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        for n in names {
            let p = dir.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn ephemeral_port() -> anyhow::Result<u16> {
    // Bind :0, read the port, release. A race with another process is
    // possible but Chrome fails loudly if it loses, and CDP wait catches it.
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

/// base64(sha256(DER SubjectPublicKeyInfo)) of amux's own TLS cert — the exact
/// form Chrome's `--ignore-certificate-errors-spki-list` expects.
///
/// Read from `~/.amux/tls/cert.pem` at LAUNCH time rather than baked in as a
/// constant: tls.rs regenerates that material when it expires or is deleted, and
/// a hardcoded pin would then silently stop matching — the browser would go back
/// to failing on amux's own URL with nothing pointing at why. Deriving it from
/// the same file the server presents means the pin cannot drift from the cert.
///
/// Returns None if the cert is unreadable or malformed, in which case the flag is
/// simply not passed: no pin is strictly worse for reaching amux, but inventing
/// or guessing a pin would be worse still, and a blanket cert bypass is never the
/// fallback (see the call site's note about the auth profile).
fn amux_cert_spki_b64(home: &Path) -> Option<String> {
    use base64::Engine as _;
    use sha2::Digest as _;
    // Derived from the KEY via rcgen — which already generated this material in
    // tls.rs and hands back the DER SubjectPublicKeyInfo directly. That avoids
    // both a new x509 dependency and hand-slicing ASN.1 out of the certificate,
    // which is the step this kind of helper usually gets subtly wrong.
    let key_pem = std::fs::read_to_string(home.join("tls").join("key.pem")).ok()?;
    let key_pair = rcgen::KeyPair::from_pem(&key_pem).ok()?;
    let digest = sha2::Sha256::digest(key_pair.public_key_der());
    Some(base64::engine::general_purpose::STANDARD.encode(digest))
}

#[derive(Debug, Clone, Serialize)]
pub struct StartedBrowser {
    pub profile: String,
    pub pid: Option<u32>,
    pub cdp_port: u16,
    pub cdp_http: String,
    pub user_data_dir: String,
    pub started_at: i64,
}

/// The tail of Chrome's launch stderr, formatted for an error message, or "" if
/// the file is missing or empty. Turns "CDP never answered" from an opaque
/// timeout into an actionable reason (a bad flag, a locked profile, no display).
fn chrome_stderr_tail(path: &Path) -> String {
    let Ok(s) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    let n = s.chars().count();
    let tail: String = s.chars().skip(n.saturating_sub(600)).collect();
    format!(" (chrome stderr: {tail})")
}

/// Launch Chrome on a profile. Cleans stale locks first (amux-owned dirs
/// only — see module docs), waits for the CDP HTTP endpoint to answer so a
/// returned port is a WORKING port, and records the child for stop().
/// `session` is the lane this launch belongs to. It exists because of AC-336:
/// the tab Chrome opens for `url` must be CLAIMED by the caller, or the next
/// lane to run a driver verb adopts it as an unowned page.
pub async fn start(
    home: &Path,
    profile: &str,
    url: &str,
    session: &str,
) -> anyhow::Result<StartedBrowser> {
    let binary = chrome_binary().ok_or_else(|| {
        anyhow::anyhow!("no Chrome/Chromium binary found (looked in /Applications and PATH)")
    })?;

    // One running browser: replace, never silently stack a second Chrome on
    // the same profile (two Chromes on one user-data-dir corrupt it).
    if RUNNING.lock().expect("browser registry poisoned").is_some() {
        let _ = stop(home).await;
    }

    let target = launch_target(home, &chrome_user_data_dir(), profile);
    if is_amux_owned(home, &target.user_data_dir) {
        std::fs::create_dir_all(&target.user_data_dir)?;
        // Our child registry is empty (checked above), so any lock here is a
        // leftover from a dead Chrome and would block this launch.
        let removed = clean_locks(&target.user_data_dir);
        if !removed.is_empty() {
            tracing::info!(dir = %target.user_data_dir.display(), ?removed, "cleaned stale Chrome locks before launch");
        }
    }

    let port = ephemeral_port()?;
    let mut cmd = tokio::process::Command::new(&binary);
    cmd.arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", target.user_data_dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check");
    // amux serves its own dashboard over a SELF-SIGNED cert, so without this the
    // one site this browser most needs to reach — amux itself — lands on
    // chrome-error://chromewebdata/ and every subsequent verb runs against the
    // error page. Dogfooding: the browser primitive could not browse the product
    // that ships it.
    //
    // Deliberately SPKI-PINNED to amux's own key rather than the blanket
    // --ignore-certificate-errors. This profile (~/.amux/playwright-auth/profile)
    // holds real logged-in sessions for third-party sites; globally disabling
    // certificate validation there would expose those cookies to any MITM on the
    // network. The pin excuses exactly one public key and leaves validation fully
    // intact for everything else — including, importantly, still rejecting a
    // DIFFERENT bad cert on localhost.
    if let Some(spki) = amux_cert_spki_b64(home) {
        cmd.arg(format!("--ignore-certificate-errors-spki-list={spki}"));
    }
    if let Some(pd) = &target.profile_directory {
        cmd.arg(format!("--profile-directory={pd}"));
    }
    if !url.trim().is_empty() {
        cmd.arg(url.trim());
    }
    // Capture Chrome's stderr so a launch failure is DIAGNOSABLE. It used to go
    // to /dev/null, so "CDP never answered" could not distinguish a crash (a bad
    // flag, a locked profile, no display) from a slow start — the exact failure
    // this wait exists to report. A file is non-blocking; its tail is read back
    // into the error below.
    let stderr_path = target.user_data_dir.join("amux-chrome-launch.stderr");
    cmd.stdout(std::process::Stdio::null());
    match std::fs::File::create(&stderr_path) {
        Ok(f) => {
            cmd.stderr(std::process::Stdio::from(f));
        }
        Err(_) => {
            cmd.stderr(std::process::Stdio::null());
        }
    }
    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("spawn {}: {e}", binary.display()))?;
    let pid = child.id();

    // A port Chrome never bound is not a started browser; wait for CDP HTTP.
    // Two failure modes, told apart because they need opposite responses:
    //  - Chrome EXITED before CDP came up (crash): report the exit status and
    //    the stderr tail IMMEDIATELY — waiting the full window for a dead
    //    process is pointless and the exit reason is what the caller needs.
    //  - Chrome is ALIVE but slow: a real logged-in profile on a busy machine
    //    (a 50-lane fleet) routinely takes longer than the old 12s to open the
    //    debugging port, so give a live Chrome up to 30s before giving up.
    let http = format!("http://127.0.0.1:{port}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            anyhow::bail!(
                "Chrome (pid {pid:?}) exited {status} before CDP on port {port} came up{}",
                chrome_stderr_tail(&stderr_path)
            );
        }
        match reqwest::Client::new()
            .get(format!("{http}/json/version"))
            .timeout(std::time::Duration::from_secs(1))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => break,
            _ if std::time::Instant::now() > deadline => {
                anyhow::bail!(
                    "Chrome (pid {pid:?}) is running but CDP on port {port} never answered within 30s{}",
                    chrome_stderr_tail(&stderr_path)
                );
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
        }
    }

    // AC-336: CLAIM THE TAB WE JUST OPENED, FOR THE LANE THAT ASKED FOR IT.
    //
    // `resolve_page` treats a page tab as available unless it appears in
    // NATIVE_TARGETS — i.e. unless some lane has already DRIVEN it through this
    // API. The tab Chrome opens for this `url` has never been driven, so it was
    // indistinguishable from an unowned tab, and the next lane to call a driver
    // verb with no binding of its own adopted it. That is precisely the hijack
    // `resolve_page`'s own comment promises not to do ("open a fresh one rather
    // than hijacking a peer's tab"), reached through the one tab kind that was
    // never registered.
    //
    // How it presented: I called /start with url=cloud.amux.io/sign-in, then
    // /action, and eval ran against localhost:4177/solutions/creative-dna — a
    // PEER lane's dev server, opened by their own /start. Every response said
    // ok:true, because from the driver's side adopting an unowned tab is the
    // documented behaviour. I typed a god-mode credential into it.
    //
    // Stale bindings are dropped first: `start` replaces any running browser
    // (see the stop above), so every target id recorded against the previous
    // Chrome now names a tab that does not exist. Leaving them is not merely
    // untidy — they are counted as `claimed_by_others`, so dead ids would make
    // live tabs look owned.
    //
    // A freshly launched Chrome has exactly one page tab, so taking the first
    // one is right regardless of what `url` redirected to — which is why this
    // matches on tab TYPE and not on the URL string.
    // The CDP call happens BEFORE the lock is taken: NATIVE_TARGETS is a
    // std::sync::Mutex, and holding one across an await deadlocks the runtime.
    let launch_tab = cdp_list(port).await.ok().and_then(|tabs| {
        tabs.as_array()
            .and_then(|ts| ts.iter().find(|t| t.get("type").and_then(Value::as_str) == Some("page")))
            .and_then(|t| t.get("id").and_then(Value::as_str))
            .map(str::to_string)
    });
    {
        let mut map = NATIVE_TARGETS.lock().expect("native targets poisoned");
        map.clear();
        if let Some(id) = &launch_tab {
            map.insert(session.to_string(), id.clone());
        }
    }
    match &launch_tab {
        Some(id) => {
            tracing::info!(session, target = %id, "claimed the launch tab for the starting lane (AC-336)")
        }
        // Not fatal: the lane simply has no binding yet and resolve_page will
        // open it a fresh tab. Logged because a silent miss here is what the
        // whole card is about.
        None => tracing::warn!(session, port, "launch tab not claimed — CDP listed no page tab"),
    }

    let started_at = chrono::Utc::now().timestamp();
    let info = StartedBrowser {
        profile: profile.to_string(),
        pid,
        cdp_port: port,
        cdp_http: http,
        user_data_dir: target.user_data_dir.display().to_string(),
        started_at,
    };
    // `child.id()` is None only once the child has been reaped; a browser we
    // just spawned always has one. Refusing here beats persisting pid 0, which
    // `kill` would interpret as the whole process group.
    let pid_num = pid.ok_or_else(|| anyhow::anyhow!("chrome spawned without a pid"))?;
    let udd_for_state = target.user_data_dir.clone();
    *RUNNING.lock().expect("browser registry poisoned") = Some(RunningBrowser {
        profile: profile.to_string(),
        user_data_dir: target.user_data_dir,
        cdp_port: port,
        started_at,
        pid: pid_num,
        child: Some(child),
    });
    // Survive a server restart (AC-325). Written AFTER the handle is live so a
    // file never claims a browser that failed to start.
    persist_running(home, profile, &udd_for_state, port, pid_num, started_at);
    Ok(info)
}

#[derive(Debug, Clone, Serialize)]
pub struct StopReport {
    pub stopped: bool,
    pub profile: Option<String>,
    /// True when Chrome exited and dropped its own Singleton locks — the
    /// signal that Cookies/Local Storage were flushed (AMUX-2070).
    pub clean_exit: Option<bool>,
    pub locks_cleaned: Vec<String>,
}

/// Stop the tracked Chrome. SIGTERM first — TERM is the signal Chrome
/// flushes Cookies/Local Storage on; SIGKILL is precisely what loses them
/// (Python `_chrome_terminate_automation`'s lesson) — escalating to SIGKILL
/// only if it ignores TERM for 8s.
pub async fn stop(home: &Path) -> StopReport {
    let running = RUNNING.lock().expect("browser registry poisoned").take();
    let Some(mut running) = running else {
        return StopReport { stopped: false, profile: None, clean_exit: None, locks_cleaned: vec![] };
    };

    // std/tokio only offer SIGKILL; /bin/kill sends the TERM we need.
    let _ = tokio::process::Command::new("kill")
        .arg("-TERM")
        .arg(running.pid.to_string())
        .status()
        .await;
    match running.child.as_mut() {
        Some(ch) => {
            let graceful =
                tokio::time::timeout(std::time::Duration::from_secs(8), ch.wait()).await;
            if graceful.is_err() {
                let _ = ch.kill().await; // last resort; may lose storage flush
                let _ = ch.wait().await;
            }
        }
        None => {
            // ADOPTED: not our child, so we cannot wait() on it. Poll for exit
            // by pid instead, keeping the same 8s TERM budget before SIGKILL —
            // the flush window is the point, not the handle.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            while std::time::Instant::now() < deadline {
                let alive = tokio::process::Command::new("kill")
                    .args(["-0", &running.pid.to_string()])
                    .status()
                    .await
                    .map(|s| s.success())
                    .unwrap_or(false);
                if !alive {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            let _ = tokio::process::Command::new("kill")
                .args(["-KILL", &running.pid.to_string()])
                .status()
                .await;
        }
    }
    clear_running(home);

    let clean_exit = locks_present(&running.user_data_dir).is_empty();
    let locks_cleaned = if !clean_exit && is_amux_owned(home, &running.user_data_dir) {
        // The child is reaped; remaining locks are stale by definition.
        clean_locks(&running.user_data_dir)
    } else {
        vec![]
    };
    StopReport {
        stopped: true,
        profile: Some(running.profile),
        clean_exit: Some(clean_exit),
        locks_cleaned,
    }
}

/// CDP over plain HTTP: the tab list. (Command traffic — screenshot, eval,
/// input — is the WebSocket client below, [`CdpClient`].)
pub async fn cdp_list(port: u16) -> anyhow::Result<serde_json::Value> {
    let r = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/json/list"))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await?;
    Ok(r.json().await?)
}

/// CDP over plain HTTP: open a new tab. Chrome 111+ requires PUT on
/// /json/new; older builds only accept GET — try PUT, fall back.
pub async fn cdp_new_tab(port: u16, url: &str) -> anyhow::Result<serde_json::Value> {
    let client = reqwest::Client::new();
    let endpoint = format!(
        "http://127.0.0.1:{port}/json/new?{}",
        serde_urlencoded_url(url)
    );
    let put = client
        .put(&endpoint)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;
    let resp = match put {
        Ok(r) if r.status().is_success() => r,
        _ => client
            .get(&endpoint)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await?,
    };
    Ok(resp.json().await?)
}

/// Minimal query-encoding for the one place we build a query string by hand
/// (reqwest's `query()` would double-encode an already-composed URL value).
fn serde_urlencoded_url(url: &str) -> String {
    let mut out = String::from("url=");
    for b in url.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---- CDP WebSocket client (AMUX-2598 cutover) -----------------------------
//
// The driver verbs (/navigate /screenshot /state /action /inspect /search)
// used to proxy to the Python server's browser engine; they now speak the
// DevTools protocol directly to the Chrome THIS process launched via
// [`start`]: JSON-RPC over the page target's local ws:// endpoint. One
// connection per API call — the protocol is stateless at our grain — and the
// only cross-call state lives either in this process (the session→tab map)
// or in the PAGE itself as an injected shim (console/network capture,
// element index list), exactly like the Python implementation
// (`_BROWSER_CAPTURE_JS` / `window.__amux`), so a dropped connection loses
// nothing a re-run cannot rebuild.

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};

/// A WebSocket connection to one CDP target.
pub struct CdpClient {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: u64,
}

impl CdpClient {
    /// HARD INVARIANT (owner directive, AMUX-2598): browser automation
    /// executes on the SERVER machine, never in a dashboard-viewing client's
    /// browser. This client therefore connects exclusively to LOOPBACK CDP
    /// endpoints — the Chrome this process spawned advertises exactly those —
    /// and refuses anything else, so no code path can quietly grow a remote
    /// browser dependency.
    pub async fn connect(ws_url: &str) -> anyhow::Result<Self> {
        let host_ok = ws_url
            .strip_prefix("ws://")
            .and_then(|rest| rest.split('/').next())
            .map(|hp| {
                let host = hp.rsplit_once(':').map(|(h, _)| h).unwrap_or(hp);
                matches!(host, "127.0.0.1" | "localhost" | "[::1]")
            })
            .unwrap_or(false);
        if !host_ok {
            anyhow::bail!(
                "refusing non-loopback CDP endpoint {ws_url}: browser automation runs only \
                 against the server-machine Chrome this process launched"
            );
        }
        let (ws, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| anyhow::anyhow!("CDP websocket connect {ws_url}: {e}"))?;
        Ok(Self { ws, next_id: 0 })
    }

    /// One CDP command. Chrome interleaves EVENT messages on the same
    /// socket; anything without our id is skipped, and the deadline caps the
    /// whole exchange so a wedged page degrades to an error, not a hung
    /// handler. A CDP-level error surfaces with its message — never as an
    /// empty success.
    pub async fn call(
        &mut self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> anyhow::Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let payload = json!({ "id": id, "method": method, "params": params }).to_string();
        let fut = async {
            use tokio_tungstenite::tungstenite::Message;
            self.ws.send(Message::Text(payload)).await?;
            loop {
                let Some(frame) = self.ws.next().await else {
                    anyhow::bail!("CDP websocket closed during {method}");
                };
                match frame? {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(&t).map_err(|e| {
                            anyhow::anyhow!("CDP sent non-JSON during {method}: {e}")
                        })?;
                        if v.get("id").and_then(Value::as_u64) == Some(id) {
                            if let Some(err) = v.get("error") {
                                anyhow::bail!(
                                    "CDP {method}: {}",
                                    err.get("message").and_then(Value::as_str).unwrap_or("error")
                                );
                            }
                            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                        }
                        // No id match: a protocol event — not ours, keep reading.
                    }
                    Message::Close(_) => anyhow::bail!("CDP websocket closed during {method}"),
                    _ => {} // Ping/Pong/Binary: tungstenite answers pings itself.
                }
            }
        };
        match tokio::time::timeout(timeout, fut).await {
            Ok(r) => r,
            Err(_) => anyhow::bail!("CDP {method} timed out after {}s", timeout.as_secs()),
        }
    }

    /// Runtime.evaluate with by-value results. A page exception is an ERROR
    /// carrying the page's own description, never a silent null — an eval
    /// that cannot fail reads as "the page had no answer" (ethos rule 7).
    pub async fn eval(&mut self, expr: &str, timeout_s: u64) -> anyhow::Result<Value> {
        let r = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": expr, "returnByValue": true, "awaitPromise": true }),
                std::time::Duration::from_secs(timeout_s),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let desc = ex
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| ex.get("text").and_then(Value::as_str))
                .unwrap_or("page threw");
            anyhow::bail!("eval: {desc}");
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }
}

// ---- driver session → tab resolution --------------------------------------

/// Which CDP page tab each amux browser-session name operates on, inside the
/// one Chrome this server launched. AC-293's resolution order (explicit →
/// X-Amux-Session → "amux") happens in the API layer; this map is keyed by
/// the RESOLVED name. Two sessions never silently share a tab: an unbound
/// session binds only to a tab no OTHER session claims (the AC-293 incident
/// was two lanes driving one browser and diagnosing each other's pages).
pub static NATIVE_TARGETS: LazyLock<Mutex<std::collections::HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Why a driver verb cannot run. `NotRunning` is the caller's fixable state
/// (the API answers 409 + a pointer at /start); `Cdp` is the browser
/// misbehaving (502).
#[derive(Debug)]
pub enum DriverError {
    NotRunning,
    Cdp(anyhow::Error),
}

impl From<anyhow::Error> for DriverError {
    fn from(e: anyhow::Error) -> Self {
        DriverError::Cdp(e)
    }
}

/// The resolved page a driver verb runs against.
pub struct DriverPage {
    pub cdp_port: u16,
    pub target_id: String,
    pub ws_url: String,
    pub url: String,
    pub title: String,
}

/// Resolve the page for a session: its bound tab if still alive, else the
/// first live page tab no other session claims (bound as a side effect),
/// else a fresh tab on `create_url` (default about:blank). The port comes
/// from [`RUNNING`] only — verbs NEVER attach to a browser this process did
/// not launch (a human's Chrome is not ours to drive).
pub async fn resolve_page(session: &str, create_url: Option<&str>) -> Result<DriverPage, DriverError> {
    let port = {
        let guard = RUNNING.lock().expect("browser registry poisoned");
        match guard.as_ref() {
            Some(r) => r.cdp_port,
            None => return Err(DriverError::NotRunning),
        }
    };
    let tabs_v = cdp_list(port).await.map_err(DriverError::Cdp)?;
    let empty = vec![];
    let tabs = tabs_v.as_array().unwrap_or(&empty);
    let page_of = |t: &Value| -> Option<DriverPage> {
        Some(DriverPage {
            cdp_port: port,
            target_id: t.get("id")?.as_str()?.to_string(),
            ws_url: t.get("webSocketDebuggerUrl")?.as_str()?.to_string(),
            url: t.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
            title: t.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
        })
    };
    let tab_id = |t: &Value| t.get("id").and_then(Value::as_str).map(str::to_string);

    let (bound, claimed_by_others): (Option<String>, std::collections::HashSet<String>) = {
        let map = NATIVE_TARGETS.lock().expect("native targets poisoned");
        (
            map.get(session).cloned(),
            map.iter().filter(|(k, _)| k.as_str() != session).map(|(_, v)| v.clone()).collect(),
        )
    };
    if let Some(tid) = bound {
        if let Some(p) = tabs
            .iter()
            .find(|t| tab_id(t).as_deref() == Some(tid.as_str()))
            .and_then(page_of)
        {
            return Ok(p);
        }
        // The bound tab died (closed/crashed): fall through and rebind.
    }
    if let Some(p) = tabs
        .iter()
        .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
        .filter(|t| tab_id(t).is_some_and(|id| !claimed_by_others.contains(&id)))
        .find_map(page_of)
    {
        NATIVE_TARGETS
            .lock()
            .expect("native targets poisoned")
            .insert(session.to_string(), p.target_id.clone());
        return Ok(p);
    }
    // Every page tab is claimed by another session (or none exist): open a
    // fresh one rather than hijacking a peer's tab.
    let t = cdp_new_tab(port, create_url.unwrap_or("about:blank")).await.map_err(DriverError::Cdp)?;
    let p = page_of(&t).ok_or_else(|| {
        DriverError::Cdp(anyhow::anyhow!("CDP /json/new returned no webSocketDebuggerUrl: {t}"))
    })?;
    NATIVE_TARGETS
        .lock()
        .expect("native targets poisoned")
        .insert(session.to_string(), p.target_id.clone());
    Ok(p)
}

/// Session bindings for GET /api/browser/sessions: (name, target, alive?).
pub async fn session_bindings() -> Vec<(String, String, bool)> {
    let port = {
        let guard = RUNNING.lock().expect("browser registry poisoned");
        guard.as_ref().map(|r| r.cdp_port)
    };
    let live: std::collections::HashSet<String> = match port {
        Some(p) => match cdp_list(p).await {
            Ok(v) => v
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => Default::default(),
        },
        None => Default::default(),
    };
    let map = NATIVE_TARGETS.lock().expect("native targets poisoned").clone();
    let mut out: Vec<(String, String, bool)> =
        map.into_iter().map(|(name, tid)| (name, tid.clone(), live.contains(&tid))).collect();
    out.sort();
    out
}

// ---- observation caps (Python `_obs_cap`, D4: policy lives in env) --------

pub fn obs_eval_cap() -> usize {
    std::env::var("AMUX_OBS_EVAL_CAP").ok().and_then(|v| v.parse().ok()).unwrap_or(8000)
}

pub fn obs_state_cap() -> usize {
    std::env::var("AMUX_OBS_STATE_CAP").ok().and_then(|v| v.parse().ok()).unwrap_or(24000)
}

/// Python `_obs_cap`: truncate AND say so — silent truncation reads as the
/// page ending there. Char-boundary safe (a byte slice through a multibyte
/// char panics where Python's slice would not).
pub fn obs_cap(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let cut: String = text.chars().take(limit).collect();
    format!(
        "{cut}\n…[truncated at {limit} chars — page stays live; re-query narrower or reopen via the url pointer]"
    )
}

/// AMUX-3062: how much a cap will cut, for a STRUCTURED envelope signal. Returns
/// `Some(full_char_len)` when `text` exceeds `limit`, `None` when it fits. The
/// same predicate `obs_cap` truncates on (`chars > limit`), factored out so a
/// caller can set `truncated: true` + the original length on the response.
/// `obs_cap` appends a human notice INTO the string; that is easy to miss when
/// the envelope still reads `{ok:true}`, which is exactly how a truncated eval
/// read as success (a 10589-char page returned 23 of 34 records and would have
/// passed review). This is the half a run or reviewer checks programmatically.
pub fn obs_truncation(text: &str, limit: usize) -> Option<usize> {
    let full = text.chars().count();
    (full > limit).then_some(full)
}

// ---- driver JS blobs (ported from amux-server.py) -------------------------

/// Python `_BROWSER_CAPTURE_JS` verbatim: idempotent page-side capture shim
/// mirroring console.*, fetch, XHR and window errors into ring buffers on
/// `window.__amux`. Injected after every native /navigate and on first
/// /inspect, so capture state survives our stateless per-call connections.
pub const CAPTURE_JS: &str = r#"
(function(){
  if (window.__amux && window.__amux.__installed) return 'already';
  var CAP = 500;
  var S = window.__amux = window.__amux || {};
  S.console = S.console || []; S.net = S.net || []; S.errors = S.errors || [];
  S.__installed = true;
  var now = function(){ return Date.now(); };
  function push(a, x){ a.push(x); if (a.length > CAP) a.shift(); }
  function fmt(a){
    try {
      if (a instanceof Error) return a.stack || (a.name + ': ' + a.message);
      if (typeof a === 'object' && a !== null) { try { return JSON.stringify(a); } catch(e){ return String(a); } }
      return String(a);
    } catch(e){ return '[unserializable]'; }
  }
  ['log','info','warn','error','debug'].forEach(function(level){
    var orig = console[level]; if (!orig) return;
    console[level] = function(){
      try { push(S.console, { level: level, text: [].map.call(arguments, fmt).join(' '), ts: now() }); } catch(e){}
      return orig.apply(console, arguments);
    };
  });
  window.addEventListener('error', function(e){
    push(S.errors, { text: (e.message || 'error') + (e.filename ? (' @ ' + e.filename + ':' + e.lineno + ':' + e.colno) : ''),
                     stack: (e.error && e.error.stack) || '', ts: now() });
  });
  window.addEventListener('unhandledrejection', function(e){
    var r = e.reason; push(S.errors, { text: 'unhandledrejection: ' + ((r && r.message) || fmt(r)), stack: (r && r.stack) || '', ts: now() });
  });
  if (window.fetch) {
    var of = window.fetch;
    window.fetch = function(input, init){
      var url = (typeof input === 'string') ? input : (input && input.url) || '';
      var method = (init && init.method) || (input && input.method) || 'GET';
      var t0 = now();
      return of.apply(this, arguments).then(function(res){
        push(S.net, { type:'fetch', method:method, url:url, status:res.status, ok:res.ok, ms: now()-t0, ts: t0 }); return res;
      }, function(err){
        push(S.net, { type:'fetch', method:method, url:url, status:0, ok:false, error:String(err), ms: now()-t0, ts: t0 }); throw err;
      });
    };
  }
  var OX = window.XMLHttpRequest;
  if (OX) {
    var NX = function(){
      var xhr = new OX();
      var rec = { type:'xhr', method:'GET', url:'', status:0, ok:false, ms:0, ts: now() };
      var open = xhr.open;
      xhr.open = function(m, u){ rec.method = m; rec.url = u; return open.apply(xhr, arguments); };
      var send = xhr.send;
      xhr.send = function(){ var t0 = now();
        xhr.addEventListener('loadend', function(){ rec.status = xhr.status; rec.ok = (xhr.status >= 200 && xhr.status < 400); rec.ms = now()-t0; push(S.net, rec); });
        return send.apply(xhr, arguments); };
      return xhr;
    };
    NX.prototype = OX.prototype; window.XMLHttpRequest = NX;
  }
  return 'installed';
})()
"#;

/// Python `_BROWSER_INSPECT_JS` verbatim (placeholders `__LIMIT__` /
/// `__CLEAR__` substituted by [`inspect_js`]): read the capture buffers plus
/// the Resource Timing back-fill, which lists requests that fired before the
/// shim was installed.
const INSPECT_JS: &str = r#"
(function(){
  var S = window.__amux || {};
  var L = __LIMIT__;
  function tail(a){ a = a || []; return a.slice(Math.max(0, a.length - L)); }
  var res = [];
  try {
    res = performance.getEntriesByType('resource').slice(-L).map(function(e){
      return { url:e.name, type:e.initiatorType, ms:Math.round(e.duration), size:e.transferSize||0, start:Math.round(e.startTime) };
    });
  } catch(e){}
  var out = { url: location.href, title: document.title, installed: !!S.__installed,
    console: tail(S.console), network: tail(S.net), errors: tail(S.errors), resources: res,
    counts: { console:(S.console||[]).length, network:(S.net||[]).length, errors:(S.errors||[]).length, resources:res.length } };
  if (__CLEAR__) { if (S.console) S.console.length = 0; if (S.net) S.net.length = 0; if (S.errors) S.errors.length = 0; }
  return out;
})()
"#;

pub fn inspect_js(limit: usize, clear: bool) -> String {
    INSPECT_JS
        .replace("__LIMIT__", &limit.to_string())
        .replace("__CLEAR__", if clear { "true" } else { "false" })
}

/// Structured perception (the Python state surface's `elements`/`viewport`):
/// enumerate visible interactive elements, keep the live element list on
/// `window.__amux_els` so click-by-index addresses EXACTLY what /state
/// showed, and return `{url,title,viewport,text,elements}`. `__TEXT_CAP__`
/// slices one past the cap so [`obs_cap`] can append its truncation notice
/// only when text was actually cut.
const STATE_JS: &str = r#"
(function(){
  var SEL = 'a[href], button, input, select, textarea, summary, [role="button"], [role="link"], [role="tab"], [role="menuitem"], [role="checkbox"], [onclick], [contenteditable="true"]';
  var seen = [];
  document.querySelectorAll(SEL).forEach(function(e){
    var r = e.getBoundingClientRect();
    if (!r.width && !r.height) return;
    var st = getComputedStyle(e);
    if (st.visibility === 'hidden' || st.display === 'none') return;
    seen.push(e);
  });
  window.__amux_els = seen;
  var els = seen.slice(0, __EL_LIMIT__).map(function(e, i){
    var label = e.getAttribute('aria-label') || e.innerText || e.value || e.placeholder || e.name || e.id || '';
    label = String(label).replace(/\s+/g, ' ').trim().slice(0, 80);
    return { index: i, tag: (e.tagName || '').toLowerCase(), label: label };
  });
  return { url: location.href, title: document.title,
           viewport: { w: window.innerWidth, h: window.innerHeight },
           text: ((document.body && document.body.innerText) || '').slice(0, __TEXT_CAP__),
           elements: els };
})()
"#;

/// Elements shown per state call — matches Python `_bu_parse_elements`'s cap.
pub const STATE_EL_LIMIT: usize = 120;

pub fn state_js() -> String {
    STATE_JS
        .replace("__EL_LIMIT__", &STATE_EL_LIMIT.to_string())
        .replace("__TEXT_CAP__", &(obs_state_cap() + 1).to_string())
}

/// Python `_bu_click_selector`'s resolve-then-click, verbatim in behavior:
/// distinguishes no-match from hidden from clicked, because a click that
/// silently hits nothing is indistinguishable from one that worked.
fn selector_click_js(selector: &str) -> String {
    format!(
        "(function(){{var e=document.querySelector({sel});\
         if(!e)return 'NOMATCH';\
         e.scrollIntoView({{block:'center'}});\
         var r=e.getBoundingClientRect();\
         if(r.width===0&&r.height===0)return 'NOTVISIBLE';\
         e.click();\
         return 'OK|'+(e.tagName||'')+'|'+((e.textContent||'').trim().slice(0,60));}})()",
        sel = json!(selector)
    )
}

/// Click element N of the `window.__amux_els` list /state built — same
/// discrimination ladder as the selector click, plus STALE (the list
/// outlived a navigation) and NOELEMENT (index out of range / no list yet).
fn index_click_js(index: usize) -> String {
    format!(
        "(function(){{var els=window.__amux_els||[];var e=els[{index}];\
         if(!e)return 'NOELEMENT';\
         if(!e.isConnected)return 'STALE';\
         e.scrollIntoView({{block:'center'}});\
         var r=e.getBoundingClientRect();\
         if(!r.width&&!r.height)return 'NOTVISIBLE';\
         e.click();\
         return 'OK|'+(e.tagName||'')+'|'+((e.textContent||e.value||'').trim().slice(0,60));}})()"
    )
}

// ---- driver verb mechanics -------------------------------------------------

/// Python `_CDP_KEYS`: key name → (windowsVirtualKeyCode, char text). The
/// API layer validates against this table so an unsupported key is a 400
/// naming the supported set, not a CDP failure.
pub const CDP_KEYS: &[(&str, i64, &str)] = &[
    ("Enter", 13, "\r"),
    ("Tab", 9, "\t"),
    ("Escape", 27, ""),
    ("Backspace", 8, ""),
    ("ArrowDown", 40, ""),
    ("ArrowUp", 38, ""),
    ("ArrowLeft", 37, ""),
    ("ArrowRight", 39, ""),
    ("PageDown", 34, ""),
    ("PageUp", 33, ""),
];

/// Python `_live_key`: rawKeyDown → optional char → keyUp.
pub async fn dispatch_key(c: &mut CdpClient, key: &str) -> anyhow::Result<()> {
    let (name, vk, text) = CDP_KEYS
        .iter()
        .find(|(k, ..)| *k == key)
        .map(|(k, v, t)| (*k, *v, *t))
        .ok_or_else(|| anyhow::anyhow!("unsupported key {key:?}"))?;
    let t = std::time::Duration::from_secs(10);
    c.call(
        "Input.dispatchKeyEvent",
        json!({ "type": "rawKeyDown", "key": name, "code": name,
                "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk }),
        t,
    )
    .await?;
    if !text.is_empty() {
        c.call("Input.dispatchKeyEvent", json!({ "type": "char", "text": text, "key": name }), t)
            .await?;
    }
    c.call(
        "Input.dispatchKeyEvent",
        json!({ "type": "keyUp", "key": name, "code": name,
                "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk }),
        t,
    )
    .await?;
    Ok(())
}

/// Coordinate click: move → press → release, like a pointer would.
pub async fn click_xy(c: &mut CdpClient, x: f64, y: f64) -> anyhow::Result<()> {
    let t = std::time::Duration::from_secs(10);
    c.call(
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none" }),
        t,
    )
    .await?;
    for ev in ["mousePressed", "mouseReleased"] {
        c.call(
            "Input.dispatchMouseEvent",
            json!({ "type": ev, "x": x, "y": y, "button": "left", "clickCount": 1 }),
            t,
        )
        .await?;
    }
    Ok(())
}

/// Interpret the OK|tag|text / NOMATCH / NOTVISIBLE / STALE / NOELEMENT
/// protocol the click JS speaks. `Ok(json)` may still carry an `error` key —
/// the API layer maps that to a 400, matching Python's selector-click.
pub fn click_outcome(raw: &Value, what: &str, hint: &str) -> Value {
    let out = raw.as_str().unwrap_or("");
    if out == "NOMATCH" || out == "NOELEMENT" {
        return json!({ "error": format!("no element matches {what}"), "hint": hint });
    }
    if out == "NOTVISIBLE" {
        return json!({ "error": format!("element for {what} has zero size (hidden or not laid out)") });
    }
    if out == "STALE" {
        return json!({ "error": format!("element list is stale for {what} — the page navigated; re-fetch GET /api/browser/state") });
    }
    if let Some(rest) = out.strip_prefix("OK|") {
        let mut parts = rest.splitn(2, '|');
        let tag = parts.next().unwrap_or("");
        let txt = parts.next().unwrap_or("");
        return json!({ "ok": true, "clicked": { "tag": tag, "text": txt } });
    }
    json!({ "error": "click produced no result — the page may have navigated mid-click", "raw": raw })
}

/// Click by CSS selector (Python `_bu_click_selector`).
pub async fn click_selector(c: &mut CdpClient, selector: &str) -> anyhow::Result<Value> {
    let raw = c.eval(&selector_click_js(selector), 20).await?;
    let mut v = click_outcome(
        &raw,
        &format!("selector {selector:?}"),
        "check the selector against GET /api/browser/state",
    );
    if let Some(o) = v.as_object_mut() {
        o.insert("selector".into(), json!(selector));
    }
    Ok(v)
}

/// Click by /state element index. NOELEMENT (no list yet — e.g. the caller
/// never fetched /state on this connection's page) re-enumerates once and
/// retries, so index clicks work on the list /state WOULD show.
pub async fn click_index(c: &mut CdpClient, index: usize) -> anyhow::Result<Value> {
    let mut raw = c.eval(&index_click_js(index), 20).await?;
    if raw.as_str() == Some("NOELEMENT") {
        let _ = c.eval(&state_js(), 20).await?;
        raw = c.eval(&index_click_js(index), 20).await?;
    }
    let mut v = click_outcome(
        &raw,
        &format!("element index {index}"),
        "indexes come from GET /api/browser/state — re-fetch it",
    );
    if let Some(o) = v.as_object_mut() {
        o.insert("index".into(), json!(index));
    }
    Ok(v)
}

/// Navigate the page and wait (bounded) for `document.readyState` to reach
/// `complete`; a slow page degrades to "still loading", never a hang. Then
/// re-inject the capture shim (parity with Python `_bu_open` re-injecting
/// per navigation) and run the landing check.
pub async fn navigate_and_settle(c: &mut CdpClient, url: &str) -> anyhow::Result<Value> {
    c.call("Page.navigate", json!({ "url": url }), std::time::Duration::from_secs(20)).await?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut ready = String::new();
    loop {
        if let Ok(v) = c.eval("document.readyState", 5).await {
            ready = v.as_str().unwrap_or("").to_string();
            if ready == "complete" {
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let _ = c.eval(CAPTURE_JS, 10).await;
    // WHERE WE LANDED IS A TARGET FACT, NOT A PAGE FACT (AC-324).
    //
    // A cross-origin navigation SWAPS the renderer process, which invalidates
    // the execution context this CdpClient is bound to. `location.href` then
    // evaluates in the dead pre-swap context and returns the OLD url — for a
    // navigation that had briefly shown Chrome's error page, that is
    // `chrome-error://chromewebdata/`, so a page that loaded perfectly reports
    // nav_failed.
    //
    // Measured on cloud.amux.io: amux said `nav_failed: true, landed:
    // chrome-error://` while the tab read `https://cloud.amux.io/` at +0s, +3s
    // and +8s. That false verdict blocked all god-mode UI verification of
    // prospect envs and sent two sessions chasing DNS, certificates and a
    // "poisoned" browser profile — none of which were involved.
    //
    // `Page.getNavigationHistory` is answered by the BROWSER about the target,
    // not by a script inside a renderer that may no longer exist, so it
    // survives the swap. The eval stays as a fallback for the case where the
    // history call is unavailable.
    let landed = match c
        .call("Page.getNavigationHistory", json!({}), std::time::Duration::from_secs(10))
        .await
        .ok()
        .and_then(|v| {
            let idx = v.get("currentIndex")?.as_i64()?;
            let entries = v.get("entries")?.as_array()?;
            entries
                .get(idx as usize)?
                .get("url")?
                .as_str()
                .map(str::to_string)
        }) {
        Some(u) if !u.is_empty() => u,
        _ => c
            .eval("location.href", 10)
            .await
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
    };
    let mut out = json!({ "ok": true, "url": landed, "ready_state": ready });
    // Landing check (Python `_bu_landing_check`): an open that cannot fail
    // sends the caller to debug the wrong layer — every later verb would run
    // against an error page or about:blank, not their page.
    if landed.starts_with("chrome-error://") {
        out["nav_failed"] = json!(true);
        out["landed"] = json!(landed);
        out["why"] = json!(format!(
            "Chromium refused to load {url}. For amux's own https://localhost this is the \
             self-signed certificate — the browser has no exception for it."
        ));
        out["hint"] = json!("every subsequent verb runs against the error page, not your page");
    } else if landed == "about:blank" && !url.is_empty() && url != "about:blank" {
        out["nav_failed"] = json!(true);
        out["landed"] = json!(landed);
        out["why"] = json!(format!("navigation to {url} left the page on about:blank"));
        out["hint"] = json!("every subsequent verb runs against a blank page, not your page");
    }
    Ok(out)
}

/// Page.captureScreenshot → PNG on disk, same output shape as Python's
/// driver screenshot (`~/.amux/browser-screenshots/<backend>-<session>.png`,
/// response carries `path`). Zero decoded bytes is an ERROR — a 0-byte file
/// reading as success is the lie ethos rule 7 exists for.
pub async fn screenshot_to_file(c: &mut CdpClient, home: &Path, session: &str) -> anyhow::Result<(PathBuf, usize)> {
    let r = c
        .call(
            "Page.captureScreenshot",
            json!({ "format": "png" }),
            std::time::Duration::from_secs(30),
        )
        .await?;
    let b64 = r
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Page.captureScreenshot returned no data"))?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| anyhow::anyhow!("screenshot base64 decode: {e}"))?;
    if bytes.is_empty() {
        anyhow::bail!("Page.captureScreenshot returned zero bytes");
    }
    let dir = home.join("browser-screenshots");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("native-{}.png", safe_file_component(session)));
    std::fs::write(&file, &bytes)?;
    Ok((file, bytes.len()))
}

/// Session names come from callers; a name is a FILE component here, so
/// anything path-shaped is flattened (Python interpolates the raw name —
/// deliberately not ported).
pub fn safe_file_component(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .collect();
    if cleaned.trim_matches(['.', '_']).is_empty() {
        "amux".into()
    } else {
        cleaned
    }
}

// ---- profile registry writes (Python `_bu_registry_register` etc.) --------

fn registry_path(home: &Path) -> PathBuf {
    home.join("playwright-auth").join("profiles.json")
}

pub fn registry_save(home: &Path, reg: &serde_json::Map<String, Value>) -> anyhow::Result<()> {
    let path = registry_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write-then-rename, like Python: a torn write must not eat the registry.
    let tmp = path.with_file_name("profiles.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&Value::Object(reg.clone()))?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Ensure a profile is registered; add `host` to its domains, set `label`
/// when given, bump `updated`. Returns the entry (Python
/// `_bu_registry_register`).
pub fn registry_register(home: &Path, name: &str, host: &str, label: &str) -> anyhow::Result<Value> {
    let mut reg = registry_load(home);
    let mut entry = match reg.get(name) {
        Some(Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    entry.entry("domains").or_insert_with(|| json!([]));
    entry.entry("label").or_insert_with(|| json!(""));
    if !host.is_empty() {
        if let Some(doms) = entry.get_mut("domains").and_then(Value::as_array_mut) {
            if !doms.iter().any(|d| d.as_str() == Some(host)) {
                doms.push(json!(host));
            }
        }
    }
    if !label.is_empty() {
        entry.insert("label".into(), json!(label));
    }
    entry.insert("updated".into(), json!(chrono::Utc::now().timestamp()));
    let entry_v = Value::Object(entry);
    reg.insert(name.to_string(), entry_v.clone());
    registry_save(home, &reg)?;
    Ok(entry_v)
}

/// Python's GET /api/browser/pw-profiles listing: profile DIRS under
/// `playwright-auth/profiles` plus `default` when the bare profile exists.
pub fn pw_profiles(home: &Path) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(home.join("playwright-auth").join("profiles")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() && !name.starts_with('.') {
                out.insert(name);
            }
        }
    }
    if home.join("playwright-auth").join("profile").is_dir() {
        out.insert("default".into());
    }
    out.into_iter().collect()
}

/// DELETE /api/browser/profile/{name}. DELIBERATE deviation from Python:
/// Python resolves an unknown name into the REAL Chrome user-data-dir and
/// `rmtree`s it — deleting a human's actual Chrome profile directory. The
/// native port refuses anything outside amux-owned dirs, the rule every
/// other mutation in this module already follows (see module docs).
pub fn delete_profile(home: &Path, name: &str) -> Result<Value, (u16, Value)> {
    if name == "default" {
        return Err((400, json!({ "error": "refusing to delete the default profile" })));
    }
    let dir = resolve_profile_dir(home, &chrome_user_data_dir(), name);
    if !dir.is_dir() {
        return Err((404, json!({ "error": "no such profile" })));
    }
    if !is_amux_owned(home, &dir) {
        return Err((
            400,
            json!({
                "error": format!(
                    "profile {name:?} resolves into the real Chrome user-data-dir — refusing to \
                     delete a human's browser profile (native-only guard; amux-owned profiles \
                     live under playwright-auth/)"
                ),
                "path": dir.display().to_string(),
            }),
        ));
    }
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        return Err((500, json!({ "error": e.to_string() })));
    }
    let mut reg = registry_load(home);
    if reg.remove(name).is_some() {
        let _ = registry_save(home, &reg);
    }
    Ok(json!({ "ok": true, "deleted": name }))
}

// ---- tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // All tests run against TEMP dirs only — never the live Chrome profiles
    // (repo rule: tests must not touch real profile contents), and never
    // launch Chrome except the #[ignore]d gated tests at the bottom.

    /// EVERY live-Chrome test must hold this for its whole body.
    ///
    /// `RUNNING` and `NATIVE_TARGETS` are process-global — one browser, one
    /// binding table — so two live tests running concurrently are two lanes
    /// fighting over one browser, which is the very bug this file's newest test
    /// is about. Concretely: `start` stops whatever is running and clears the
    /// binding table, so a peer test's launch wipes this test's claim mid-body
    /// and the assertion fails against CORRECT code. That is the worst kind of
    /// red — it accuses the fix.
    ///
    /// It is a tokio Mutex, not a std one, because these bodies await while
    /// holding it. `--test-threads=1` would also work but cannot be enforced
    /// from here, and a test that only passes under a flag someone has to
    /// remember is a test that will go red for the wrong reason.
    static LIVE_BROWSER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn fake_home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn profile_resolution_matches_python_precedence() {
        let home = fake_home();
        let chrome = home.path().join("fake-chrome-udd");
        std::fs::create_dir_all(&chrome).unwrap();

        // default -> the bare playwright-auth/profile dir
        assert_eq!(
            resolve_profile_dir(home.path(), &chrome, "default"),
            home.path().join("playwright-auth/profile")
        );
        assert_eq!(
            resolve_profile_dir(home.path(), &chrome, "  "),
            home.path().join("playwright-auth/profile")
        );

        // a legacy dir that exists wins over the chrome location
        let legacy = home.path().join("playwright-auth/profiles/gh");
        std::fs::create_dir_all(&legacy).unwrap();
        assert_eq!(resolve_profile_dir(home.path(), &chrome, "gh"), legacy);

        // unknown names resolve into the chrome user-data-dir
        assert_eq!(resolve_profile_dir(home.path(), &chrome, "brandnew"), chrome.join("brandnew"));
    }

    #[test]
    fn launch_target_splits_chrome_dir_profiles() {
        let home = fake_home();
        let chrome = home.path().join("fake-chrome-udd");
        std::fs::create_dir_all(&chrome).unwrap();

        // amux-owned dir IS the user-data-dir
        let legacy = home.path().join("playwright-auth/profiles/gh");
        std::fs::create_dir_all(&legacy).unwrap();
        let t = launch_target(home.path(), &chrome, "gh");
        assert_eq!(t.user_data_dir, legacy);
        assert_eq!(t.profile_directory, None);

        // chrome-dir profile becomes --profile-directory under the parent
        let t = launch_target(home.path(), &chrome, "Work");
        assert_eq!(t.user_data_dir, chrome);
        assert_eq!(t.profile_directory.as_deref(), Some("Work"));
    }

    #[test]
    fn inventory_lists_dirs_and_registry_metadata() {
        let home = fake_home();
        let profiles = home.path().join("playwright-auth/profiles");
        std::fs::create_dir_all(profiles.join("github")).unwrap();
        std::fs::write(profiles.join("github/Cookies"), vec![0u8; 2048]).unwrap();
        std::fs::create_dir_all(home.path().join("playwright-auth/profile")).unwrap();
        std::fs::write(
            home.path().join("playwright-auth/profiles.json"),
            r#"{"github":{"domains":["github.com"],"label":"GH"}}"#,
        )
        .unwrap();

        let list = list_profiles(home.path(), true);
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"github"), "dir-backed profile listed: {names:?}");
        assert!(names.contains(&"default"), "default profile listed: {names:?}");

        let gh = list.iter().find(|p| p.name == "github").unwrap();
        assert!(gh.registered);
        assert_eq!(gh.domains, vec!["github.com"]);
        assert_eq!(gh.label, "GH");
        assert!(gh.last_used.is_some(), "mtime-derived last_used");
        assert!(gh.size_mb.unwrap() > 0.0, "walked size includes the 2KB cookie file");

        // sizes are opt-in
        let slim = list_profiles(home.path(), false);
        assert!(slim.iter().all(|p| p.size_mb.is_none()));
    }

    #[test]
    fn lock_cleanup_removes_singletons_including_dangling_symlinks() {
        let home = fake_home();
        let prof = home.path().join("playwright-auth/profiles/x");
        std::fs::create_dir_all(&prof).unwrap();
        // Real Chrome locks are symlinks at "<host>-<pid>" whose target does
        // not exist — the exact case exists() gets wrong and lstat gets right.
        #[cfg(unix)]
        std::os::unix::fs::symlink("nohost-99999", prof.join("SingletonLock")).unwrap();
        #[cfg(not(unix))]
        std::fs::write(prof.join("SingletonLock"), "x").unwrap();
        std::fs::write(prof.join("SingletonCookie"), "c").unwrap();
        std::fs::write(prof.join("Preferences"), "{}").unwrap();

        let present = locks_present(&prof);
        assert!(present.contains(&"SingletonLock".to_string()), "lstat sees the dangling symlink");
        assert!(present.contains(&"SingletonCookie".to_string()));

        let cleaned = reconcile_locks_at_startup(home.path());
        assert_eq!(cleaned.len(), 1);
        let (dir, removed) = &cleaned[0];
        assert_eq!(dir, &prof);
        assert!(removed.contains(&"SingletonLock".to_string()));
        assert!(removed.contains(&"SingletonCookie".to_string()));
        assert!(locks_present(&prof).is_empty(), "locks actually gone");
        assert!(prof.join("Preferences").exists(), "profile CONTENTS untouched");
    }

    #[test]
    fn amux_owned_boundary() {
        let home = fake_home();
        assert!(is_amux_owned(home.path(), &home.path().join("playwright-auth/profiles/x")));
        assert!(is_amux_owned(home.path(), &home.path().join("playwright-auth/profile")));
        assert!(!is_amux_owned(home.path(), &chrome_user_data_dir()));
        assert!(!is_amux_owned(home.path(), Path::new("/tmp/elsewhere")));
    }

    // ---- CDP client + driver mechanics (hermetic — fake WS, temp dirs) ----

    /// The client's one job: match responses by id THROUGH interleaved
    /// events, and surface CDP errors as errors. A fake Chrome answers every
    /// call with an event first — a client that read frames positionally
    /// would return the event as the result.
    #[tokio::test]
    async fn cdp_client_skips_events_matches_ids_and_surfaces_errors() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            use tokio_tungstenite::tungstenite::Message;
            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Text(t) = msg {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    let id = v["id"].as_u64().unwrap();
                    let method = v["method"].as_str().unwrap_or("");
                    // Interleaved EVENT before every response.
                    ws.send(Message::Text(
                        json!({ "method": "Page.frameNavigated", "params": {} }).to_string(),
                    ))
                    .await
                    .unwrap();
                    let resp = if method == "Deliberate.fail" {
                        json!({ "id": id, "error": { "message": "boom" } })
                    } else {
                        json!({ "id": id, "result": { "echo": method } })
                    };
                    ws.send(Message::Text(resp.to_string())).await.unwrap();
                }
            }
        });
        let mut c = CdpClient::connect(&format!("ws://{addr}")).await.unwrap();
        let five = std::time::Duration::from_secs(5);
        let r = c.call("Page.navigate", json!({}), five).await.unwrap();
        assert_eq!(r["echo"], "Page.navigate");
        let r = c.call("Runtime.evaluate", json!({}), five).await.unwrap();
        assert_eq!(r["echo"], "Runtime.evaluate", "second call matches its own id");
        let err = c.call("Deliberate.fail", json!({}), five).await.unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
    }

    /// The server-machine invariant, hermetically: a CDP endpoint that is
    /// not loopback is refused BEFORE any connection attempt — automation
    /// can only ever reach the Chrome this server launched.
    #[tokio::test]
    async fn cdp_client_refuses_non_loopback_endpoints() {
        for url in [
            "ws://192.168.1.50:9222/devtools/page/X",
            "ws://client.example.com:9222/devtools/page/X",
            "wss://example.com/devtools/page/X",
        ] {
            let err = match CdpClient::connect(url).await {
                Err(e) => e,
                Ok(_) => panic!("{url}: non-loopback endpoint must be refused"),
            };
            assert!(err.to_string().contains("refusing non-loopback"), "{url}: {err}");
        }
    }

    #[test]
    fn obs_cap_truncates_and_says_so() {
        assert_eq!(obs_cap("short", 100), "short");
        let capped = obs_cap(&"x".repeat(50), 10);
        assert!(capped.starts_with("xxxxxxxxxx\n…[truncated at 10 chars"), "{capped}");
        // Multibyte safety: a byte-index slice would panic here.
        let capped = obs_cap(&"é".repeat(50), 10);
        assert!(capped.contains("truncated at 10"), "{capped}");
    }

    /// AMUX-3062: the structured half. obs_truncation reports Some(full_len) on
    /// the same predicate obs_cap truncates on, and None otherwise, so the eval
    /// envelope can carry truncated=true + the original length instead of the
    /// caller substring-matching the notice out of an {ok:true} result.
    #[test]
    fn obs_truncation_reports_the_original_length_only_when_it_cut() {
        assert_eq!(obs_truncation("short", 100), None, "fits => no signal");
        assert_eq!(obs_truncation(&"x".repeat(10), 10), None, "exactly at the cap is not truncated");
        assert_eq!(obs_truncation(&"x".repeat(50), 10), Some(50), "over the cap => full length");
        // Counts CHARS, matching obs_cap, so a multibyte page is not mis-measured.
        assert_eq!(obs_truncation(&"é".repeat(50), 10), Some(50), "multibyte counted by char");
    }

    #[test]
    fn click_outcome_discriminates() {
        let ok = click_outcome(&json!("OK|A|More information..."), "selector \"a\"", "h");
        assert_eq!(ok["ok"], json!(true));
        assert_eq!(ok["clicked"]["tag"], "A");
        for (raw, needle) in [
            ("NOMATCH", "no element matches"),
            ("NOELEMENT", "no element matches"),
            ("NOTVISIBLE", "zero size"),
            ("STALE", "stale"),
        ] {
            let v = click_outcome(&json!(raw), "x", "h");
            assert!(
                v["error"].as_str().unwrap().contains(needle),
                "{raw}: {v}"
            );
        }
        // Garbage (page navigated mid-click) is an error, not a silent ok.
        assert!(click_outcome(&json!(null), "x", "h")["error"].is_string());
    }

    #[test]
    fn js_blob_substitution() {
        let js = inspect_js(300, true);
        assert!(js.contains("var L = 300;"));
        assert!(js.contains("if (true) {"));
        assert!(!js.contains("__LIMIT__") && !js.contains("__CLEAR__"));
        let js = state_js();
        assert!(!js.contains("__EL_LIMIT__") && !js.contains("__TEXT_CAP__"));
        assert!(js.contains("window.__amux_els"));
        // Selector JSON-encodes through format!: quotes must survive.
        let js = selector_click_js("a[href=\"x\"]");
        assert!(js.contains("querySelector(\"a[href=\\\"x\\\"]\")"), "{js}");
    }

    #[test]
    fn registry_register_appends_domains_and_survives_reload() {
        let home = fake_home();
        let e = registry_register(home.path(), "gh", "github.com", "GH").unwrap();
        assert_eq!(e["domains"], json!(["github.com"]));
        assert_eq!(e["label"], "GH");
        // Second host appends; duplicate host does not; label sticks.
        registry_register(home.path(), "gh", "gist.github.com", "").unwrap();
        let e = registry_register(home.path(), "gh", "github.com", "").unwrap();
        assert_eq!(e["domains"], json!(["github.com", "gist.github.com"]));
        assert_eq!(e["label"], "GH");
        assert!(e["updated"].as_i64().unwrap() > 0);
        // And the listing path (registry_load) sees the same bytes.
        let reg = registry_load(home.path());
        assert_eq!(reg["gh"]["domains"], json!(["github.com", "gist.github.com"]));
    }

    #[test]
    fn pw_profiles_lists_dirs_plus_default() {
        let home = fake_home();
        std::fs::create_dir_all(home.path().join("playwright-auth/profiles/github")).unwrap();
        std::fs::create_dir_all(home.path().join("playwright-auth/profiles/.hidden")).unwrap();
        std::fs::create_dir_all(home.path().join("playwright-auth/profile")).unwrap();
        assert_eq!(pw_profiles(home.path()), vec!["default".to_string(), "github".to_string()]);
    }

    #[test]
    fn delete_profile_guards() {
        let home = fake_home();
        let dir = home.path().join("playwright-auth/profiles/x");
        std::fs::create_dir_all(&dir).unwrap();
        registry_register(home.path(), "x", "x.com", "").unwrap();

        assert_eq!(delete_profile(home.path(), "default").unwrap_err().0, 400);
        assert_eq!(
            delete_profile(home.path(), "amux-native-test-nonexistent-9f3a").unwrap_err().0,
            404
        );
        let ok = delete_profile(home.path(), "x").unwrap();
        assert_eq!(ok["deleted"], "x");
        assert!(!dir.exists());
        assert!(registry_load(home.path()).get("x").is_none(), "registry entry pruned");
    }

    #[test]
    fn safe_file_component_flattens_path_shapes() {
        assert_eq!(safe_file_component("amux"), "amux");
        assert_eq!(safe_file_component("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(safe_file_component(""), "amux");
        assert_eq!(safe_file_component(".."), "amux");
    }

    /// Live-Chrome smoke test: launches a real Chrome on a THROWAWAY temp
    /// user-data-dir (never a saved profile), checks CDP answers, stops it.
    /// #[ignore]d: `cargo test -- --ignored` runs it only where a human asked.
    #[tokio::test]
    #[ignore = "launches a real Chrome; run explicitly with -- --ignored"]
    async fn live_chrome_start_stop() {
        let _serial = LIVE_BROWSER.lock().await;
        if chrome_binary().is_none() {
            eprintln!("no Chrome binary on this machine; skipping");
            return;
        }
        let home = fake_home();
        // Use a scratch profile name so the target dir is amux-owned + temp.
        let info = start(home.path(), "default", "about:blank", "live-smoke").await.expect("start");
        assert!(info.cdp_port > 0);
        let tabs = cdp_list(info.cdp_port).await.expect("cdp /json/list");
        assert!(tabs.is_array());

        // Server-machine pin (owner directive): the page a driver verb
        // resolves is served by the LOOPBACK CDP port of the child THIS
        // process spawned — same port, same registry pid — and a round-trip
        // eval proves the automation executed there.
        let page = resolve_page("live-smoke", None).await.map_err(|e| match e {
            DriverError::NotRunning => anyhow::anyhow!("not running"),
            DriverError::Cdp(e) => e,
        }).expect("resolve_page");
        assert_eq!(page.cdp_port, info.cdp_port, "verb port IS the spawned child's port");
        assert!(
            page.ws_url.starts_with(&format!("ws://127.0.0.1:{}/", info.cdp_port))
                || page.ws_url.starts_with(&format!("ws://localhost:{}/", info.cdp_port)),
            "loopback CDP only: {}",
            page.ws_url
        );
        {
            let guard = RUNNING.lock().expect("browser registry poisoned");
            // `pid` rather than `child.id()`: an ADOPTED browser has no Child
            // (AC-325) and the registry must still name the process it is
            // acting on. Asserting through the Option would pass vacuously the
            // day adoption is exercised here.
            let reg_pid = guard.as_ref().map(|r| r.pid);
            assert_eq!(reg_pid, info.pid, "acting Chrome is the registry's process");
            assert!(reg_pid.is_some());
            assert!(
                guard.as_ref().and_then(|r| r.child.as_ref()).is_some(),
                "a browser this test SPAWNED must carry its Child handle"
            );
        }
        let mut c = CdpClient::connect(&page.ws_url).await.expect("cdp ws connect");
        let v = c.eval("1+1", 10).await.expect("eval");
        assert_eq!(v, json!(2));

        NATIVE_TARGETS.lock().unwrap().remove("live-smoke");
        let report = stop(home.path()).await;
        assert!(report.stopped);
    }

    /// AC-336: one lane must not be handed the tab another lane launched.
    ///
    /// This is the incident rebuilt from its own artifact rather than a
    /// convenient case. The reported specimen was a peer's `/start {url}` tab
    /// being adopted by the next lane to call a driver verb — and it was
    /// adoptable for exactly one reason: the launch tab was never recorded in
    /// NATIVE_TARGETS, so `resolve_page` could not tell it from an unowned page.
    ///
    /// THE ORDERING IS THE WHOLE TEST, and getting it wrong made an earlier
    /// draft of this test non-discriminating. The hijack needs the PEER to
    /// resolve BEFORE the owner has ever driven its own tab — which is exactly
    /// what happened: a peer lane called `/start {url}` and never drove it, then
    /// I called a driver verb and was handed their page. If the owner resolves
    /// first it claims the launch tab under the old code too, so an
    /// owner-then-peer test passes against the bug and proves nothing.
    ///
    /// The launch tab is read from CDP, not from NATIVE_TARGETS, for the same
    /// reason: asserting on our own bookkeeping would make the test fail at the
    /// bookkeeping instead of at the behaviour, and the behaviour is the claim.
    ///
    /// Falsifiability CHECKED, not assumed: with `map.insert` in `start`
    /// removed, `peer` is handed the launch tab and the assert_ne below fires.
    #[tokio::test]
    #[ignore = "launches a real Chrome; run explicitly with -- --ignored"]
    async fn a_peers_launch_tab_is_not_adopted_by_another_lane() {
        let _serial = LIVE_BROWSER.lock().await;
        if chrome_binary().is_none() {
            eprintln!("no Chrome binary on this machine; skipping");
            return;
        }
        let home = fake_home();
        let info = start(home.path(), "default", "about:blank", "owner").await.expect("start");

        // The launch tab as CHROME reports it — the incident's artifact.
        let launch_tab = cdp_list(info.cdp_port)
            .await
            .expect("cdp /json/list")
            .as_array()
            .and_then(|ts| {
                ts.iter()
                    .find(|t| t.get("type").and_then(Value::as_str) == Some("page"))
                    .and_then(|t| t.get("id").and_then(Value::as_str))
                    .map(str::to_string)
            })
            .expect("a freshly launched Chrome has a page tab");

        // PEER RESOLVES FIRST — the owner has driven nothing yet.
        let peer_page = resolve_page("peer", None).await.expect("peer resolves");
        assert_ne!(
            peer_page.target_id, launch_tab,
            "peer was handed the tab the owner launched — this is AC-336"
        );
        assert_eq!(peer_page.cdp_port, info.cdp_port, "still one shared browser, per-lane tabs");

        // The owner still gets the tab it launched, not the peer's new one.
        let owner_page = resolve_page("owner", None).await.expect("owner resolves");
        assert_eq!(owner_page.target_id, launch_tab, "owner keeps the tab it launched");
        assert_ne!(owner_page.target_id, peer_page.target_id, "two lanes, two tabs");

        for s in ["owner", "peer"] {
            NATIVE_TARGETS.lock().unwrap().remove(s);
        }
        let report = stop(home.path()).await;
        assert!(report.stopped);
    }
}

#[cfg(test)]
mod spki_pin_tests {
    use super::*;

    /// The pin must equal what OpenSSL computes for the same cert, because that
    /// is the value Chrome compares against. Generating a real keypair here and
    /// checking only "returns Some" would pass for any digest of any bytes — so
    /// this recomputes the expected answer by the independent path (public key
    /// DER -> sha256 -> base64) and, crucially, also asserts the helper does NOT
    /// simply hash the file contents, which is the plausible wrong implementation.
    #[test]
    fn spki_pin_matches_openssl_definition_and_is_not_a_file_hash() {
        use base64::Engine as _;
        use sha2::Digest as _;

        let dir = std::env::temp_dir().join(format!("amux-spki-test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let tls = dir.join("tls");
        std::fs::create_dir_all(&tls).unwrap();

        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "amux");
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();
        std::fs::write(tls.join("cert.pem"), cert.pem()).unwrap();
        std::fs::write(tls.join("key.pem"), key_pair.serialize_pem()).unwrap();

        let got = amux_cert_spki_b64(&dir).expect("pin computed");

        // Independent recomputation of Chrome's documented input:
        // base64(sha256(DER SubjectPublicKeyInfo)).
        let want = base64::engine::general_purpose::STANDARD
            .encode(sha2::Sha256::digest(key_pair.public_key_der()));
        assert_eq!(got, want, "pin must be base64(sha256(SPKI DER))");

        // Counter-case: a digest of the PEM FILE would also be Some(...) and
        // would look correct in every "did it return a value" check, while
        // matching nothing Chrome ever computes.
        let file_hash = base64::engine::general_purpose::STANDARD
            .encode(sha2::Sha256::digest(std::fs::read(tls.join("cert.pem")).unwrap()));
        assert_ne!(got, file_hash, "pin must not be a hash of the cert file");

        // Missing material must degrade to None (no flag), never to a guess.
        std::fs::remove_file(tls.join("key.pem")).unwrap();
        assert!(amux_cert_spki_b64(&dir).is_none(), "absent key -> no pin");

        std::fs::remove_dir_all(&dir).ok();
    }
}
