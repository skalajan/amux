#!/usr/bin/env bash
# amux installer
set -euo pipefail

BOLD=$'\033[1m' GREEN=$'\033[32m' YELLOW=$'\033[33m' RED=$'\033[31m' RESET=$'\033[0m'

INSTALL_DIR="${AMUX_INSTALL_DIR:-/usr/local/bin}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "${BOLD}amux installer${RESET}"
echo ""

# Check dependencies
check_dep() {
  command -v "$1" &>/dev/null || { echo "${RED}missing:${RESET} $1 — install with: $2"; exit 1; }
}
check_dep tmux   "brew install tmux"
check_dep python3 "brew install python3"

# Install amux CLI
if [[ ! -w "$INSTALL_DIR" ]]; then
  echo "${YELLOW}note:${RESET} writing to $INSTALL_DIR requires sudo"
  SUDO=sudo
else
  SUDO=""
fi

$SUDO cp "$SCRIPT_DIR/amux" "$INSTALL_DIR/amux"
$SUDO chmod +x "$INSTALL_DIR/amux"

# Copy server next to the CLI so amux serve can find it
$SUDO cp "$SCRIPT_DIR/amux-server.py" "$INSTALL_DIR/amux-server.py"

# Copy the remote wrapper (drive a remote amux over its REST API)
$SUDO cp "$SCRIPT_DIR/amux-remote" "$INSTALL_DIR/amux-remote"
$SUDO chmod +x "$INSTALL_DIR/amux-remote"

echo "${GREEN}✓${RESET} installed ${BOLD}amux${RESET} → $INSTALL_DIR/amux"
echo "${GREEN}✓${RESET} installed ${BOLD}amux-server.py${RESET} → $INSTALL_DIR/amux-server.py"
echo "${GREEN}✓${RESET} installed ${BOLD}amux-remote${RESET} → $INSTALL_DIR/amux-remote"
echo ""
echo "Quick start:"
echo "  amux register myproject --dir ~/Dev/myproject --yolo"
echo "  amux start myproject"
echo "  amux serve   # → https://localhost:8822"
