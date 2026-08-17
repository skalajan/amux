#!/usr/bin/env bash
# amux uninstaller — the inverse of ./install.sh.
#
# Removes: the launchd agents (server + builder) and the installed binaries.
# NEVER touches ~/.amux — your DB, sessions, tokens, memory and logs are
# data, and an uninstaller that deletes data is a bug, not a feature.
# Re-running ./install.sh against the same AMUX_HOME picks all of it back up.
#
# Honors the same overrides as install.sh (AMUX_INSTALL_BIN,
# AMUX_LAUNCHD_LABEL, AMUX_LAUNCHD_DIR, AMUX_HOME — the last only to NAME
# what is being kept).
set -euo pipefail

BOLD=$'\033[1m' GREEN=$'\033[32m' YELLOW=$'\033[33m' RESET=$'\033[0m'
say()  { echo "${GREEN}✓${RESET} $*"; }
warn() { echo "${YELLOW}!${RESET} $*"; }

AMUX_HOME="${AMUX_HOME:-$HOME/.amux}"
BIN_DIR="${AMUX_INSTALL_BIN:-$HOME/.local/bin}"
LABEL="${AMUX_LAUNCHD_LABEL:-com.amux.server-rs}"
PLIST_DIR="${AMUX_LAUNCHD_DIR:-$HOME/Library/LaunchAgents}"
UID_N="$(id -u)"

echo "${BOLD}amux uninstaller${RESET}"
echo ""

if [[ "$(uname -s)" == "Darwin" ]]; then
  for l in "$LABEL-builder" "$LABEL"; do
    if launchctl print "gui/$UID_N/$l" >/dev/null 2>&1; then
      launchctl bootout "gui/$UID_N/$l" 2>/dev/null || true
      say "stopped + unloaded launchd agent: $l"
    fi
    if [[ -f "$PLIST_DIR/$l.plist" ]]; then
      rm -f "$PLIST_DIR/$l.plist"
      say "removed $PLIST_DIR/$l.plist"
    fi
  done
fi

for b in amux-server-rs amux-rs; do
  if [[ -e "$BIN_DIR/$b" ]]; then
    rm -f "$BIN_DIR/$b"
    say "removed $BIN_DIR/$b"
  fi
done

echo ""
warn "KEPT (on purpose): $AMUX_HOME — your DB, sessions, auth token, memory and logs."
echo "  amux never deletes your data on uninstall. Re-run ./install.sh to come back to it,"
echo "  or remove it yourself if you truly want a clean slate:  rm -rf $AMUX_HOME"
