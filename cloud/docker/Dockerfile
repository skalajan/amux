FROM python:3.11-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    tmux curl git procps ca-certificates gnupg \
    && rm -rf /var/lib/apt/lists/*

# Node 22 — Debian's `nodejs` package is far too old for the Claude Code CLI.
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir requests

# Claude Code. This MUST run the postinstall: it downloads the platform-native
# binary. The previous build used `--ignore-scripts ... 2>/dev/null || true`,
# which skipped that download and swallowed the error, so every cloud container
# shipped a claude that failed at runtime with "native binary not installed" —
# and the smoke test never noticed, because it only asserted that a session
# produced *some* output, which a bare shell prompt satisfies.
RUN npm install -g @anthropic-ai/claude-code

# Fail the BUILD, not the customer's session, if the CLI is not actually runnable.
RUN claude --version && echo "claude CLI verified at build time"

WORKDIR /app

# Browser automation. amux launches Playwright via `require('playwright')` with
# cwd=/app, so the node package must resolve from HERE (not -g), and Chromium
# plus its OS libs must be present. Cloud has no display, so this is the
# headless engine; an interactive sign-in is handled by importing a saved
# storage-state/profile rather than popping a window. This is a DEPLOYMENT
# concern only — amux-server.py is byte-identical to local.
RUN npm install playwright \
    && npx playwright install --with-deps chrome \
    && node -e "require('playwright'); console.log('playwright resolves from /app')"

# The Browser tab drives browser-use (python console script at
# /usr/local/bin/browser-use), with playwright underneath — the node package
# alone is not enough. Verify both entrypoints exist at build time so this
# capability cannot ship broken the way the claude CLI did.
# PIN to the version local runs. browser-use 3.x removed --json and the
# preset subcommands amux drives, so an unpinned install builds fine and
# then fails at runtime — exactly the class of break this file now guards.
RUN pip install --no-cache-dir 'browser-use==0.12.2' playwright \
        && test -x /usr/local/bin/browser-use \
    && browser-use --help 2>&1 | grep -q -- "--json" \
    && echo "browser-use 0.12.2 verified at build time (--json present)"

# browser-use 0.12.2 looks for real Chrome, not Chromium — locally that is
# the desktop install. Assert a chrome binary exists so this cannot ship
# half-working again.
RUN (which google-chrome || which google-chrome-stable || ls /root/.cache/ms-playwright/chrome-*/chrome-linux*/chrome) \
    && echo "chrome binary verified at build time"

COPY amux-server.py .

ENV AMUX_PORT=8822
# Bring the user's sessions back when this container is replaced. Deploying a
# new image recreates it; the volume restores every session's .env, the board
# and the schedules, but the tmux processes died with the old container — so the
# workspace reads fully populated through the API and shows its owner nothing.
#
# Set HERE rather than by the gateway because the deploy path never touches the
# gateway: .github/workflows/deploy-cloud.yml runs `docker compose up -d
# --no-recreate` directly, so a per-container write in start_container() reaches
# only containers the gateway itself starts — which is exactly why the first
# live test of this found every session still down on a container that already
# had the code. Image-level covers every start path.
#
# amux-server.py defaults this OFF, which is the right default on a workstation
# (starting an entire fleet on every restart). This image is the environment
# where the opposite is right, so the image is what says so.
ENV AMUX_AUTOSTART_SESSIONS=1
EXPOSE 8822

CMD ["python", "amux-server.py", "8822", "--no-tls"]
