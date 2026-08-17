#!/usr/bin/env bash
# deploy/mac-server/install.sh — mac-server deploy kit installer.
#
# Idempotent, non-interactive-capable installer for the mac-server deploy kit
# (see docs/mac-server-deploy.md and .omc/plans/mac-server-deploy.md). Clones
# (or verifies) the checkout, symlinks /usr/local/bin/amux INTO the checkout
# (never a copy — that symlink is what makes a later `git pull` a hot deploy),
# seeds ~/.amux/, renders the four plist templates with real values, and
# bootstraps ONLY com.amux.serve. Every other LaunchAgent (telegram, start-all,
# pull-update) is rendered but deliberately left un-bootstrapped: each has a
# real prerequisite this script cannot satisfy (a Telegram bot+group, a
# registered fleet, and P3.b's evidence-based allowlist, respectively) — this
# script prints the exact manual command for each, once its prerequisite is
# met.
#
# *** This script NEVER runs `launchctl bootstrap`/`enable` for
# com.amux.pull-update, under any flag. That gate is P3.b (allowlist populated
# from OBSERVED git status on this host, not the seed candidates alone) — see
# the mandatory executor notes at the top of .omc/plans/mac-server-deploy.md. ***
#
# *** Full Disk Access (TCC) for the server's python is a manual macOS GUI step
# that cannot be scripted or SSH'd under any execution mode. This script
# prints the exact System Settings path; it does not attempt to grant or
# verify it. ***
#
# Usage:
#   install.sh [flags]
#
# Flags (all have an env var fallback; all have a default; --yes skips the
# final confirmation prompt but never silences the FDA/pull-update warnings):
#   --home <path>          $AMUX_DEPLOY_HOME       default: $HOME
#   --checkout <path>      $AMUX_DEPLOY_CHECKOUT   default: <home>/Desktop/Projects/amux
#   --python <path>        $AMUX_DEPLOY_PYTHON     default: /usr/local/bin/python3 if present, else `command -v python3`
#   --repo <git-url>       $AMUX_DEPLOY_REPO       default: https://github.com/skalajan/amux.git
#   --branch <name>        $AMUX_DEPLOY_BRANCH     default: main
#   --cadence <seconds>    $AMUX_DEPLOY_CADENCE    default: 300 (pull-update StartInterval)
#   --yes                  $AMUX_DEPLOY_YES=1       skip the confirmation prompt (non-interactive)
#   --skip-bootstrap                                render plists only; bootstrap nothing (fully manual)
#   --force-symlink                                 overwrite an existing amux binary/symlink that
#                                                    isn't already the expected symlink (see step 4)
#   --bin-target <path>    $AMUX_DEPLOY_BIN_TARGET  default: /usr/local/bin/amux — testing only,
#                                                    never needed for a real mac-server install
#   -h, --help
set -euo pipefail

BOLD=$'\033[1m' GREEN=$'\033[32m' YELLOW=$'\033[33m' RED=$'\033[31m' CYAN=$'\033[36m' RESET=$'\033[0m'
ok()   { echo "${GREEN}✓${RESET} $*"; }
warn() { echo "${YELLOW}⚠${RESET} $*"; }
err()  { echo "${RED}✗${RESET} $*"; }
info() { echo "  $*"; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── defaults / flags ─────────────────────────────────────────────────────────
HOME_DIR="${AMUX_DEPLOY_HOME:-$HOME}"
CHECKOUT_DIR="${AMUX_DEPLOY_CHECKOUT:-}"
PYTHON_BIN="${AMUX_DEPLOY_PYTHON:-}"
REPO_URL="${AMUX_DEPLOY_REPO:-https://github.com/skalajan/amux.git}"
BRANCH="${AMUX_DEPLOY_BRANCH:-main}"
CADENCE="${AMUX_DEPLOY_CADENCE:-300}"
NON_INTERACTIVE="${AMUX_DEPLOY_YES:-0}"
SKIP_BOOTSTRAP=0
FORCE_SYMLINK=0
# Overridable only for testing this script itself in a sandbox without
# touching the real /usr/local/bin — production installs never need this flag.
AMUX_BIN_TARGET="${AMUX_DEPLOY_BIN_TARGET:-/usr/local/bin/amux}"

usage() { sed -n '2,/^set -euo pipefail/p' "${BASH_SOURCE[0]}" | sed '$d'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --home) HOME_DIR="$2"; shift 2 ;;
    --checkout) CHECKOUT_DIR="$2"; shift 2 ;;
    --python) PYTHON_BIN="$2"; shift 2 ;;
    --repo) REPO_URL="$2"; shift 2 ;;
    --branch) BRANCH="$2"; shift 2 ;;
    --cadence) CADENCE="$2"; shift 2 ;;
    --yes) NON_INTERACTIVE=1; shift ;;
    --skip-bootstrap) SKIP_BOOTSTRAP=1; shift ;;
    --force-symlink) FORCE_SYMLINK=1; shift ;;
    --bin-target) AMUX_BIN_TARGET="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) err "unknown flag: $1"; usage; exit 1 ;;
  esac
done

[[ -n "$CHECKOUT_DIR" ]] || CHECKOUT_DIR="$HOME_DIR/Desktop/Projects/amux"
if [[ -z "$PYTHON_BIN" ]]; then
  if [[ -x /usr/local/bin/python3 ]]; then
    PYTHON_BIN="/usr/local/bin/python3"
  else
    PYTHON_BIN="$(command -v python3 || true)"
  fi
fi

echo "${BOLD}amux mac-server deploy kit installer${RESET}"
echo ""
echo "  home:       $HOME_DIR"
echo "  checkout:   $CHECKOUT_DIR"
echo "  python:     ${PYTHON_BIN:-<not found>}"
echo "  repo:       $REPO_URL"
echo "  branch:     $BRANCH"
echo "  cadence:    ${CADENCE}s (pull-update StartInterval — NOT enabled by this script)"
echo ""

# ── 1. prerequisite checks ──────────────────────────────────────────────────
echo "${BOLD}1. Prerequisites${RESET}"
FAIL=0

if command -v tmux &>/dev/null; then ok "tmux: $(command -v tmux)"; else err "tmux not found — install with: brew install tmux"; FAIL=1; fi
if command -v git  &>/dev/null; then ok "git: $(command -v git)";   else err "git not found — install with: brew install git"; FAIL=1; fi

if [[ -n "$PYTHON_BIN" && -x "$PYTHON_BIN" ]]; then
  if "$PYTHON_BIN" -c 'import sys; sys.exit(0 if sys.version_info >= (3,10) else 1)' 2>/dev/null; then
    ok "python3: $PYTHON_BIN ($("$PYTHON_BIN" --version 2>&1))"
  else
    err "python3 at $PYTHON_BIN is older than 3.10 ($("$PYTHON_BIN" --version 2>&1)) — amux requires >=3.10"
    FAIL=1
  fi
else
  err "python3 not found — install with: brew install python3, or pass --python <path>"
  FAIL=1
fi

# Port 8822 — informational only (a re-install on an already-running host is
# expected to find it in use).
if command -v lsof &>/dev/null; then
  if lsof -i :8822 -sTCP:LISTEN &>/dev/null; then
    warn "port 8822 is already in use — expected if amux is already installed here, otherwise investigate before continuing"
  else
    ok "port 8822 is free"
  fi
else
  warn "lsof not found — could not check port 8822"
fi

# launchd label collision — informational; the agents system must use
# different labels than com.amux.* (see docs/mac-server-deploy.md Preflight).
if command -v launchctl &>/dev/null; then
  EXISTING_AMUX_LABELS="$(launchctl list 2>/dev/null | grep -i amux || true)"
  if [[ -n "$EXISTING_AMUX_LABELS" ]]; then
    info "existing com.amux.* launchd labels found (expected on a re-install):"
    echo "$EXISTING_AMUX_LABELS" | sed 's/^/    /'
  fi
  info "record the OTHER (non-amux) launchd labels on this host yourself and confirm"
  info "they don't collide with com.amux.* — this script cannot know which ones"
  info "belong to the 'agents' system (see docs/mac-server-deploy.md Preflight)."
fi

if [[ "$FAIL" -eq 1 ]]; then
  err "one or more required prerequisites are missing — fix the above and re-run"
  exit 1
fi
echo ""

# ── confirmation (skippable with --yes) ─────────────────────────────────────
if [[ "$NON_INTERACTIVE" -ne 1 ]]; then
  read -r -p "Proceed with install using the values above? [y/N] " _reply
  case "$_reply" in
    y|Y|yes|YES) ;;
    *) echo "aborted."; exit 1 ;;
  esac
fi

# ── 2. clone or verify the checkout ─────────────────────────────────────────
echo "${BOLD}2. Checkout${RESET}"
if [[ -d "$CHECKOUT_DIR/.git" ]]; then
  ok "checkout already exists at $CHECKOUT_DIR — leaving its git state alone"
  EXISTING_ORIGIN="$(git -C "$CHECKOUT_DIR" remote get-url origin 2>/dev/null || true)"
  if [[ -n "$EXISTING_ORIGIN" && "$EXISTING_ORIGIN" != "$REPO_URL" ]]; then
    warn "existing checkout's origin ($EXISTING_ORIGIN) differs from --repo ($REPO_URL) — not touching it"
  fi
elif [[ -e "$CHECKOUT_DIR" ]]; then
  err "$CHECKOUT_DIR exists and is not a git checkout — remove it or pass a different --checkout"
  exit 1
else
  info "cloning $REPO_URL (branch $BRANCH) into $CHECKOUT_DIR ..."
  mkdir -p "$(dirname "$CHECKOUT_DIR")"
  git clone --branch "$BRANCH" "$REPO_URL" "$CHECKOUT_DIR"
  ok "cloned into $CHECKOUT_DIR"
fi
echo ""

# ── 3. ~/.amux/ skeleton ─────────────────────────────────────────────────────
echo "${BOLD}3. ~/.amux/ skeleton${RESET}"
mkdir -p "$HOME_DIR/.amux/logs"
ok "$HOME_DIR/.amux/logs"

seed_env() {
  local example="$1" target="$2"
  if [[ -f "$target" ]]; then
    ok "$target already exists — left untouched"
  else
    cp "$example" "$target"
    ok "seeded $target from $(basename "$example")"
  fi
}
seed_env "$CHECKOUT_DIR/deploy/mac-server/server.env.example"   "$HOME_DIR/.amux/server.env"
seed_env "$CHECKOUT_DIR/deploy/mac-server/defaults.env.example" "$HOME_DIR/.amux/defaults.env"
echo "  (telegram.env is NOT seeded here — it needs real bot credentials from"
echo "   Phase 5; see $CHECKOUT_DIR/deploy/mac-server/telegram.env.example)"
echo ""

# ── 4. amux symlink — the hot-deploy linchpin ───────────────────────────────
echo "${BOLD}4. amux symlink${RESET}"
AMUX_BIN_SRC="$CHECKOUT_DIR/amux"
SUDO=""
[[ -w "$(dirname "$AMUX_BIN_TARGET")" ]] || SUDO="sudo"

_current_link=""
[[ -L "$AMUX_BIN_TARGET" ]] && _current_link="$(readlink "$AMUX_BIN_TARGET")"

if [[ "$_current_link" == "$AMUX_BIN_SRC" ]]; then
  ok "$AMUX_BIN_TARGET already symlinks into the checkout"
elif [[ -e "$AMUX_BIN_TARGET" && "$FORCE_SYMLINK" -ne 1 ]]; then
  warn "$AMUX_BIN_TARGET already exists and is NOT the expected symlink"
  info "This is the install.sh-copy-trap: if it's a COPY of amux-server.py's"
  info "companion CLI (e.g. from the top-level ./install.sh) rather than a"
  info "symlink into the checkout, 'amux serve' resolves its own script_dir"
  info "via readlink -f \$0 and will serve a STALE copy — a git pull to the"
  info "checkout will silently NOT hot-deploy. Fix manually, then re-run with"
  info "--force-symlink to have this script do it, or run yourself:"
  info "  $SUDO rm '$AMUX_BIN_TARGET' && $SUDO ln -s '$AMUX_BIN_SRC' '$AMUX_BIN_TARGET'"
else
  $SUDO mkdir -p "$(dirname "$AMUX_BIN_TARGET")"
  [[ -e "$AMUX_BIN_TARGET" || -L "$AMUX_BIN_TARGET" ]] && $SUDO rm -f "$AMUX_BIN_TARGET"
  $SUDO ln -s "$AMUX_BIN_SRC" "$AMUX_BIN_TARGET"
  ok "symlinked $AMUX_BIN_TARGET -> $AMUX_BIN_SRC"
fi
echo ""

# ── 5. render plist templates ───────────────────────────────────────────────
echo "${BOLD}5. Render LaunchAgent plists${RESET}"
LAUNCH_AGENTS_DIR="$HOME_DIR/Library/LaunchAgents"
mkdir -p "$LAUNCH_AGENTS_DIR"

render_plist() {
  local tmpl="$1" out="$2"
  sed -e "s#@@HOME@@#$HOME_DIR#g" \
      -e "s#@@CHECKOUT@@#$CHECKOUT_DIR#g" \
      -e "s#@@PYTHON@@#$PYTHON_BIN#g" \
      -e "s#@@CADENCE_SECONDS@@#$CADENCE#g" \
      "$tmpl" > "$out"
  if command -v plutil &>/dev/null; then
    plutil -lint -s "$out" || { err "rendered $out failed plutil -lint"; exit 1; }
  fi
  ok "rendered $out"
}

render_plist "$CHECKOUT_DIR/deploy/mac-server/com.amux.serve.plist.tmpl"       "$LAUNCH_AGENTS_DIR/com.amux.serve.plist"
render_plist "$CHECKOUT_DIR/deploy/mac-server/com.amux.telegram.plist.tmpl"    "$LAUNCH_AGENTS_DIR/com.amux.telegram.plist"
render_plist "$CHECKOUT_DIR/deploy/mac-server/com.amux.start-all.plist.tmpl"   "$LAUNCH_AGENTS_DIR/com.amux.start-all.plist"
render_plist "$CHECKOUT_DIR/deploy/mac-server/com.amux.pull-update.plist.tmpl" "$LAUNCH_AGENTS_DIR/com.amux.pull-update.plist"
echo ""

# ── 6. bootstrap core (com.amux.serve only) ─────────────────────────────────
echo "${BOLD}6. Bootstrap com.amux.serve${RESET}"
UID_NUM="$(id -u)"
if [[ "$SKIP_BOOTSTRAP" -eq 1 ]]; then
  warn "--skip-bootstrap given — plists rendered but nothing loaded. Bootstrap it yourself:"
  info "  launchctl bootstrap gui/$UID_NUM $LAUNCH_AGENTS_DIR/com.amux.serve.plist"
  info "  launchctl enable    gui/$UID_NUM/com.amux.serve"
else
  if launchctl print "gui/$UID_NUM/com.amux.serve" &>/dev/null; then
    ok "com.amux.serve already bootstrapped — leaving it running (edit+re-bootstrap manually to pick up plist changes)"
  else
    launchctl bootstrap "gui/$UID_NUM" "$LAUNCH_AGENTS_DIR/com.amux.serve.plist"
    launchctl enable "gui/$UID_NUM/com.amux.serve"
    ok "bootstrapped + enabled com.amux.serve"
  fi
fi
echo ""

# ── 7. manual steps this script cannot do ───────────────────────────────────
echo "${BOLD}7. Manual steps required (this script cannot do these)${RESET}"
echo ""
echo "  ${RED}${BOLD}Full Disk Access (required before com.amux.serve can function):${RESET}"
echo "    System Settings -> Privacy & Security -> Full Disk Access -> add:"
echo "      ${CYAN}${PYTHON_BIN:-<python3 path>}${RESET}"
echo "    This is a manual GUI (TCC) step under ANY execution mode — it"
echo "    cannot be scripted or done over SSH. See docs/mac-server-deploy.md"
echo "    Phase 4."
echo ""
echo "  ${BOLD}Telegram (Phase 5)${RESET} — before installing com.amux.telegram:"
echo "    1. Create a NEW BotFather bot + the \"Amux Server\" forum supergroup"
echo "       (docs/telegram-chat.md §1-3)."
echo "    2. Fill $HOME_DIR/.amux/telegram.env from"
echo "       $CHECKOUT_DIR/deploy/mac-server/telegram.env.example, then:"
echo "       chmod 600 $HOME_DIR/.amux/telegram.env"
echo "    3. launchctl bootstrap gui/$UID_NUM $LAUNCH_AGENTS_DIR/com.amux.telegram.plist"
echo "       launchctl enable    gui/$UID_NUM/com.amux.telegram"
echo ""
echo "  ${BOLD}Fleet + reboot wake (Phase 6)${RESET} — after registering sessions on THIS host:"
echo "    launchctl bootstrap gui/$UID_NUM $LAUNCH_AGENTS_DIR/com.amux.start-all.plist"
echo ""
echo "  ${RED}${BOLD}Auto-update (Phase 6, P3.b-gated) — this script NEVER does this:${RESET}"
echo "    Do NOT bootstrap com.amux.pull-update until"
echo "    deploy/mac-server/pull-reconcile-allowlist.txt has been populated"
echo "    from OBSERVED 'git status --porcelain' output on THIS host across at"
echo "    least one full cadence window (Phase 3.b) — the seed candidates"
echo "    alone are insufficient. Once that's done:"
echo "      launchctl bootstrap gui/$UID_NUM $LAUNCH_AGENTS_DIR/com.amux.pull-update.plist"
echo "      launchctl enable    gui/$UID_NUM/com.amux.pull-update"
echo ""
echo "  Verify once FDA is granted:"
echo "    curl -sk \${AMUX_URL:-https://localhost:8822}/api/sessions"
echo ""
ok "install.sh done — see docs/mac-server-deploy.md for the full P3-P6 runbook"
