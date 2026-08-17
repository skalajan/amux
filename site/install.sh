#!/usr/bin/env bash
# amux bootstrap installer (served from amux.io) — clones the repo and runs
# the real installer, ./install.sh, which builds the Rust server and installs
# the launchd agents. The old Python-symlink install this file used to do was
# retired with the Python server (2026-08-09).
set -euo pipefail

REPO_URL="https://github.com/mixpeek/amux"
DEST="${AMUX_CHECKOUT:-$HOME/Dev/amux}"

command -v git &>/dev/null || { echo "git is required" >&2; exit 1; }

if [ -d "$DEST/.git" ]; then
  echo "Using existing checkout at $DEST (not pulling — review and update it yourself)"
else
  echo "Cloning $REPO_URL to $DEST..."
  git clone "$REPO_URL" "$DEST"
fi

cd "$DEST"
exec ./install.sh
