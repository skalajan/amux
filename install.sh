#!/usr/bin/env bash
# amux installer — one command from a fresh checkout to a running dashboard.
#
#   ./install.sh
#
# What it does, in order:
#   1. checks prerequisites (rust toolchain, tmux; herdr is optional) —
#      prompts before installing anything, never silently
#   2. cargo build --release the workspace
#   3. installs the server (amux-server-rs) + CLI (amux-rs) into ~/.local/bin
#   4. writes + loads the launchd agents (macOS): com.amux.server-rs on
#      port 8824, and com.amux.server-rs-builder (auto-rebuild on new
#      commits). On other platforms it installs the binaries and prints an
#      honest "run it like this" instead of pretending to manage a service.
#   5. creates ~/.amux (the server mints its DB, TLS material and auth token
#      there on first boot), waits for /health, prints the dashboard URL.
#
# IDEMPOTENT: re-running rebuilds and upgrades the binaries + agents in
# place. It NEVER writes into existing ~/.amux data (DB, sessions, tokens).
#
# Overridable (used by the e2e self-test to install against a throwaway
# prefix without touching the live service — and handy for parallel installs):
#   AMUX_HOME           data dir                  (default: ~/.amux)
#   AMUX_INSTALL_BIN    binary dir                (default: ~/.local/bin)
#   AMUX_RS_PORT        https port                (default: 8824)
#   AMUX_LAUNCHD_LABEL  launchd label             (default: com.amux.server-rs)
#   AMUX_LAUNCHD_DIR    plist dir                 (default: ~/Library/LaunchAgents)
#   AMUX_NO_BUILDER=1   skip the auto-rebuild agent
#   AMUX_ALLOW_NO_TMUX=1  install anyway without tmux (dashboard-only)
set -euo pipefail

BOLD=$'\033[1m' GREEN=$'\033[32m' YELLOW=$'\033[33m' RED=$'\033[31m' RESET=$'\033[0m'
say()  { echo "${GREEN}✓${RESET} $*"; }
warn() { echo "${YELLOW}!${RESET} $*"; }
die()  { echo "${RED}✗${RESET} $*" >&2; exit 1; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AMUX_HOME="${AMUX_HOME:-$HOME/.amux}"
BIN_DIR="${AMUX_INSTALL_BIN:-$HOME/.local/bin}"
PORT="${AMUX_RS_PORT:-8824}"
LABEL="${AMUX_LAUNCHD_LABEL:-com.amux.server-rs}"
PLIST_DIR="${AMUX_LAUNCHD_DIR:-$HOME/Library/LaunchAgents}"
TARGET_DIR="${CARGO_TARGET_DIR:-$SCRIPT_DIR/target}"
OS="$(uname -s)"

echo "${BOLD}amux installer${RESET} (Rust server, port $PORT)"
echo ""

# ── 1. Prerequisites ────────────────────────────────────────────────────────
# rustup puts cargo in ~/.cargo/bin, which a fresh shell may not have yet.
export PATH="$HOME/.cargo/bin:$PATH"

if ! command -v cargo >/dev/null 2>&1; then
  warn "rust toolchain not found (cargo)."
  echo "  amux's server and CLI are Rust; the standard toolchain installer is rustup:"
  echo "      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  if [[ -t 0 ]]; then
    read -r -p "  Install rustup now? [y/N] " reply
    if [[ "$reply" == "y" || "$reply" == "Y" ]]; then
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
      export PATH="$HOME/.cargo/bin:$PATH"
      command -v cargo >/dev/null 2>&1 || die "rustup finished but cargo still not found — open a new shell and re-run ./install.sh"
    else
      die "cargo is required. Install rust, then re-run ./install.sh"
    fi
  else
    die "cargo is required and this shell is non-interactive — install rust (rustup), then re-run ./install.sh"
  fi
fi
say "rust toolchain: $(cargo --version)"

if command -v tmux >/dev/null 2>&1; then
  say "tmux: $(tmux -V)"
elif [[ "${AMUX_ALLOW_NO_TMUX:-}" == "1" ]]; then
  warn "tmux not found — continuing (AMUX_ALLOW_NO_TMUX=1). The dashboard will run, but worker sessions need tmux."
else
  warn "tmux not found — amux hosts worker sessions in tmux."
  if [[ "$OS" == "Darwin" ]]; then
    echo "  install it with:  brew install tmux"
  else
    echo "  install it with your package manager, e.g.:  sudo apt install tmux"
  fi
  die "install tmux and re-run ./install.sh (or AMUX_ALLOW_NO_TMUX=1 ./install.sh for a dashboard-only install)"
fi

if command -v herdr >/dev/null 2>&1; then
  say "herdr: found (optional backend for headless workers)"
else
  echo "  herdr: not found — optional. tmux is the default session backend;"
  echo "         install herdr later and set AMUX_HERDR_SESSION=1 per worker to use it."
fi

# ── 2. Build ────────────────────────────────────────────────────────────────
echo ""
echo "Building (cargo build --release --workspace) …"
(cd "$SCRIPT_DIR" && cargo build --release --workspace)
[[ -x "$TARGET_DIR/release/amux-server" ]] || die "build finished but $TARGET_DIR/release/amux-server is missing"
[[ -x "$TARGET_DIR/release/amux-rs" ]]     || die "build finished but $TARGET_DIR/release/amux-rs is missing"
say "built server + CLI"

# ── 3. Install binaries ─────────────────────────────────────────────────────
mkdir -p "$BIN_DIR"
# install(1) replaces the file atomically enough for the server's
# self-adoption watcher: a running server notices its binary changed and
# exits for launchd to relaunch the new build.
install -m 0755 "$TARGET_DIR/release/amux-server" "$BIN_DIR/amux-server-rs"
install -m 0755 "$TARGET_DIR/release/amux-rs" "$BIN_DIR/amux-rs"
say "installed $BIN_DIR/amux-server-rs"
say "installed $BIN_DIR/amux-rs"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) warn "$BIN_DIR is not on your PATH — add it to use amux-rs directly" ;;
esac

# ── 4. Data dir — created, never clobbered ──────────────────────────────────
# The server mints everything else itself on first boot: SQLite DB
# (amux.db), TLS material (tls/), and the shared bearer token (auth_token).
# Existing files are DATA and are never touched by an upgrade.
mkdir -p "$AMUX_HOME/logs"
say "data dir: $AMUX_HOME (existing data untouched)"

# Worker templates are CODE, not data: they ship with the checkout and an
# upgrade should carry new ones. They used to be found beside the installed
# amux-server.py, which was deleted with the Python server — after which
# templates_dir() resolved to nothing and `apply-template` answered "template
# not found" for every real id. Syncing them here is what makes that rung exist.
# Override with AMUX_TEMPLATES_DIR if you keep your own set.
if [[ -d "$SCRIPT_DIR/templates" ]]; then
  mkdir -p "$AMUX_HOME/templates"
  cp -R "$SCRIPT_DIR/templates/." "$AMUX_HOME/templates/"
  say "templates: $AMUX_HOME/templates ($(find "$AMUX_HOME/templates" -name template.json | wc -l | tr -d ' ') available)"
fi

# Shared-checkout git guard (AMUX-3033). The PreToolUse Bash hook runs
# ~/.amux/hooks/git-shared-guard.py on EVERY Bash tool call across the fleet, so
# it gates git in shared checkouts. It used to be an unversioned 32KB runtime
# file: it could not be reviewed, diffed, or rolled back, and "can't reproduce on
# the current file" could not tell already-fixed from changed-under-us. The source
# now lives in the repo (scripts/git-hooks/) and is INSTALLED from there, so the
# committed copy is authoritative. We record its sha256 alongside it; the server's
# `hooks.shared_guard_matches_committed` invariant compares the running file against
# the sha embedded in the binary and surfaces any drift in /api/health/invariants.
if [[ -f "$SCRIPT_DIR/scripts/git-hooks/git-shared-guard.py" ]]; then
  mkdir -p "$AMUX_HOME/hooks"
  cp "$SCRIPT_DIR/scripts/git-hooks/git-shared-guard.py" "$AMUX_HOME/hooks/git-shared-guard.py"
  chmod +x "$AMUX_HOME/hooks/git-shared-guard.py"
  _guard_sha="$(shasum -a 256 "$AMUX_HOME/hooks/git-shared-guard.py" | cut -d' ' -f1)"
  printf '%s  git-shared-guard.py\n' "$_guard_sha" > "$AMUX_HOME/hooks/git-shared-guard.py.sha256"
  say "git guard: $AMUX_HOME/hooks/git-shared-guard.py (sha ${_guard_sha:0:12})"
fi

# State-report hook (AMUX-2936), installed from the repo for the same reason as
# the guard above: it was an unversioned runtime file, and unversioned runtime
# files fork. There were already THREE spellings of "report state to amux" on
# this machine — an inline one-liner in settings.json, ~/.amux/hooks/amux-report.sh,
# and this script — and settings.json pointed at the POOREST of them, so model and
# token reporting silently regressed to nothing and auto-compact lost its only
# input. That is the failure amux-report.sh header already warned about in
# 2026-08-11 ("two implementations of one thing is what produced this bug; do not
# re-fork it"), recurring because nothing made the canonical copy authoritative.
#
# It reports state + model + tokens + the lane conversation id. The last one is
# what lets the staged-commit guard resolve a lane transcript at all; without it
# a lane is BLIND, which is the one class where a commit absorbing another
# session work passes silently.
if [[ -f "$SCRIPT_DIR/scripts/hooks/hook-report.sh" ]]; then
  cp "$SCRIPT_DIR/scripts/hooks/hook-report.sh" "$AMUX_HOME/hook-report.sh"
  chmod +x "$AMUX_HOME/hook-report.sh"
  _rep_sha="$(shasum -a 256 "$AMUX_HOME/hook-report.sh" | cut -d' ' -f1)"
  printf '%s  hook-report.sh\n' "$_rep_sha" > "$AMUX_HOME/hook-report.sh.sha256"
  say "report hook: $AMUX_HOME/hook-report.sh (sha ${_rep_sha:0:12})"
fi

# ── 5. Service ──────────────────────────────────────────────────────────────
if [[ "$OS" != "Darwin" ]]; then
  # Honest degrade: no launchd here, and pretending to manage systemd from a
  # bash installer is how services half-exist. Print exactly what to run.
  warn "$OS: no service manager configured by this installer."
  echo ""
  echo "Run the server in the foreground:"
  echo "    AMUX_RS_PORT=$PORT $BIN_DIR/amux-server-rs"
  echo ""
  echo "Or wrap it in a systemd user unit (~/.config/systemd/user/amux.service):"
  echo "    [Service]"
  echo "    ExecStart=$BIN_DIR/amux-server-rs"
  echo "    Environment=AMUX_RS_PORT=$PORT"
  echo "    Restart=always"
  echo ""
  echo "Then: dashboard at ${BOLD}https://localhost:$PORT${RESET} · token in $AMUX_HOME/auth_token"
  exit 0
fi

mkdir -p "$PLIST_DIR"
UID_N="$(id -u)"
SERVER_PLIST="$PLIST_DIR/$LABEL.plist"

# launchd does NOT inherit a shell PATH — a thrice-hit incident class in this
# repo (restic, the rust builder, the server itself): every subprocess the
# server spawns (tmux, claude, herdr, git) must be reachable from the PATH
# written HERE, or it fails only when launchd starts it and works in every
# terminal you debug from.
LAUNCHD_PATH="$HOME/.cargo/bin:$BIN_DIR:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin"

cat > "$SERVER_PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array><string>$BIN_DIR/amux-server-rs</string></array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>AMUX_RS_PORT</key><string>$PORT</string>
    <key>AMUX_HOME</key><string>$AMUX_HOME</string>
    <key>HOME</key><string>$HOME</string>
    <key>PATH</key><string>$LAUNCHD_PATH</string>
  </dict>
  <key>KeepAlive</key><true/>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>$AMUX_HOME/logs/server-rs.log</string>
  <key>StandardErrorPath</key><string>$AMUX_HOME/logs/server-rs.log</string>
</dict>
</plist>
PLIST

# (Re)load: bootout is a no-op complaint when the label isn't loaded yet.
launchctl bootout "gui/$UID_N/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$UID_N" "$SERVER_PLIST"
say "launchd agent loaded: $LABEL"

if [[ "${AMUX_NO_BUILDER:-}" != "1" ]]; then
  BUILDER_LABEL="$LABEL-builder"
  BUILDER_PLIST="$PLIST_DIR/$BUILDER_LABEL.plist"
  cat > "$BUILDER_PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$BUILDER_LABEL</string>
  <key>ProgramArguments</key>
  <array><string>$SCRIPT_DIR/scripts/rust-auto-build.sh</string></array>
  <key>StartInterval</key><integer>60</integer>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>$AMUX_HOME/logs/rust-auto-build.log</string>
  <key>StandardErrorPath</key><string>$AMUX_HOME/logs/rust-auto-build.log</string>
</dict>
</plist>
PLIST
  launchctl bootout "gui/$UID_N/$BUILDER_LABEL" 2>/dev/null || true
  launchctl bootstrap "gui/$UID_N" "$BUILDER_PLIST"
  say "launchd agent loaded: $BUILDER_LABEL (rebuilds + redeploys on new commits in $SCRIPT_DIR)"
fi

# ── 6. Wait for /health ─────────────────────────────────────────────────────
echo ""
echo "Waiting for the server on https://localhost:$PORT …"
healthy=""
for _ in $(seq 1 30); do
  if body=$(curl -sk --max-time 2 "https://localhost:$PORT/health" 2>/dev/null) \
     && [[ "$body" == *'"status":"ok"'* ]]; then
    healthy=1
    break
  fi
  sleep 1
done
if [[ -z "$healthy" ]]; then
  die "server did not answer /health within 30s — check $AMUX_HOME/logs/server-rs.log"
fi
say "server is up: $(echo "$body" | tr -d '\n' | cut -c1-120)"

echo ""
echo "${BOLD}Done.${RESET}"
echo "  Dashboard   https://localhost:$PORT   (self-signed cert — your browser will warn once)"
echo "  Auth token  $AMUX_HOME/auth_token    (the dashboard + amux-rs read this automatically on this machine)"
echo "  CLI         amux-rs --url https://localhost:$PORT health"
echo "  Logs        $AMUX_HOME/logs/server-rs.log"
echo "  Uninstall   ./uninstall.sh   (removes binaries + agents; never touches $AMUX_HOME data)"
