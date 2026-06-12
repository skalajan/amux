# Local amux Modifications

Custom changes layered on top of upstream `mixpeek/amux`.

Goal: per-session Claude account selection. The session's `CLAUDE_CONFIG_DIR` decides which Claude.ai account is used inside the tmux pane — work vs personal vs anything else.

> **2026-06-12 — implemented in BOTH the CLI and the server.**
> The routing now lives in two places that must stay in sync:
>
> 1. **`amux` (bash CLI)** — the hunks below (register/exec persist `CC_CONFIG_DIR`, cmd_start exports it). Sessions started via `amux start` use this path.
> 2. **`amux-server.py`** — `_pick_claude_config_dir` / `_session_claude_config_dir` plus `CLAUDE_CONFIG_DIR` injection in `start_session()`. Sessions started via the dashboard, the API, or auto-wake use this path. The server also routes transcript/resume/memory/stats lookups across all config homes (`_claude_config_homes`, `_claude_project_dir`).
>
> **Incident note:** the server's `_auto_trust_dir` used to rewrite `/usr/local/bin/amux` with its minimal CLI stub whenever the content differed. Since that path is a symlink into this repo, it silently destroyed the modified bash CLI (uncommitted at the time). The stub-writer now refuses to follow symlinks or overwrite anything that isn't the stub itself, and the CLI hunks were reapplied and committed. The CLI additionally gained server-API passthroughs (`crm`, `restart`, `sessions`, `share`, `unshare`) that previously only existed in the stub.

## Rule

- **Personal is the default.**
- **Work** is auto-selected when the session's working directory (`--dir`) is under any path listed in `$AMUX_WORK_PATHS` (colon-separated, default `$HOME/Desktop/Projects/Fidoo`).
- New CLI flags override the auto-detect:
  - `--config-dir <abs-path>` — explicit
  - `--work` — shortcut for `$AMUX_WORK_CONFIG_DIR` (default `$HOME/.claude-work`)
  - `--personal` — shortcut for `$AMUX_PERSONAL_CONFIG_DIR` (default `$HOME/.claude-personal`)

Decision sticks: the chosen dir is persisted to `~/.amux/sessions/<name>.env` as `CC_CONFIG_DIR=...`. Every subsequent `amux start <name>` re-reads it.

## Configurable environment

| Variable | Default | Purpose |
|---|---|---|
| `AMUX_WORK_PATHS` | `$HOME/Desktop/Projects/Fidoo` | Colon-separated paths whose subdirs auto-select work |
| `AMUX_WORK_CONFIG_DIR` | `$HOME/.claude-work` | Target dir for `--work` and auto-selection |
| `AMUX_PERSONAL_CONFIG_DIR` | `$HOME/.claude-personal` | Target dir for `--personal` and default |
| `AMUX_DEFAULT_MODEL` | `claude-opus-4-7[1m]` | Model passed to `claude` when the session didn't set its own `--model` |
| `AMUX_NO_REMOTE_CONTROL` | (unset) | Set to `1` to skip auto-appending `--remote-control` |
| `AMUX_NO_YOLO` | (unset) | Set to `1` to disable the default `--dangerously-skip-permissions` (yolo) for new sessions |

Set these in `~/.amux/server.env` or your shell rc to override.

## Files touched

- `amux` (the bash CLI) — hunks 1–7 below + server-API passthrough commands
- `amux-server.py` — account routing in `start_session()` + cross-home lookups + stub-writer guard + restart-race fix

## How to reapply after `git pull` from upstream

Re-run the 5 hunks below against the freshly pulled `amux` file, then deploy:

```bash
bash -n amux && cp amux /usr/local/bin/amux
```

If any `old_string` no longer matches verbatim (upstream refactored the surrounding code), find the closest equivalent — the hunks are positioned around stable structural landmarks (`parse_claude_flags`, `cmd_register`, `cmd_exec`, `cmd_start` shell_setup block).

### Hunk 1 — add helper `pick_default_config_dir`

Insert immediately after `parse_claude_flags()` closing `}`, before the `# ── Commands ──` divider.

```bash
# pick_default_config_dir <resolved_dir> — decide which Claude config dir
# (= which Claude.ai account) a new session should use when neither
# --config-dir nor --work nor --personal was passed.
#
# Rule: personal by default; work when the resolved working dir is under
# any path listed in $AMUX_WORK_PATHS (colon-separated). Configurable via:
#   AMUX_WORK_PATHS          (default: $HOME/Desktop/Projects/Fidoo)
#   AMUX_WORK_CONFIG_DIR     (default: $HOME/.claude-work)
#   AMUX_PERSONAL_CONFIG_DIR (default: $HOME/.claude-personal)
pick_default_config_dir() {
  local resolved="$1"
  local work_paths="${AMUX_WORK_PATHS:-$HOME/Desktop/Projects/Fidoo}"
  local work_dir="${AMUX_WORK_CONFIG_DIR:-$HOME/.claude-work}"
  local personal_dir="${AMUX_PERSONAL_CONFIG_DIR:-$HOME/.claude-personal}"
  local IFS=':' p
  for p in $work_paths; do
    [[ -z "$p" ]] && continue
    case "$resolved" in
      "$p"|"$p"/*) echo "$work_dir"; return ;;
    esac
  done
  echo "$personal_dir"
}
```

### Hunk 2 — three new flags in `parse_claude_flags`

Insert immediately AFTER the existing `--dir)` case and BEFORE the catch-all `*)`:

```bash
      # Claude config directory — selects which account to launch as.
      # --config-dir specifies an absolute path; --work / --personal are
      # shortcuts for the standard locations (see pick_default_config_dir).
      --config-dir)
        mkdir -p "$2" || die "cannot create --config-dir: $2"
        CC_CONFIG_DIR="$(cd "$2" && pwd)" || die "cannot resolve --config-dir to absolute path: $2"
        shift 2
        ;;
      --work)
        CC_CONFIG_DIR="${AMUX_WORK_CONFIG_DIR:-$HOME/.claude-work}"
        shift
        ;;
      --personal)
        CC_CONFIG_DIR="${AMUX_PERSONAL_CONFIG_DIR:-$HOME/.claude-personal}"
        shift
        ;;
```

### Hunk 3 — `cmd_register` auto-detect + persist

In `cmd_register()`, after the `case "$resolved_dir" ... esac` validation block, insert the auto-detect call. Then extend the heredoc with `CC_CONFIG_DIR` and the final echo with a `config-dir:` line:

```bash
  # Auto-detect Claude config dir (account) if not explicitly set.
  [[ -z "${CC_CONFIG_DIR:-}" ]] && CC_CONFIG_DIR="$(pick_default_config_dir "$resolved_dir")"

  local file="$CC_SESSIONS/$name.env"
  cat > "$file" <<EOF
# amux session: $name
# registered: $(date -Iseconds)
CC_NAME="$name"
CC_DIR="$resolved_dir"
CC_FLAGS="${CC_FLAGS_STR}"
CC_CONFIG_DIR="$CC_CONFIG_DIR"
EOF

  echo "${GREEN}registered${RESET} ${BOLD}$name${RESET} → $resolved_dir"
  echo "  config-dir: $CC_CONFIG_DIR"
  [[ -n "$CC_FLAGS_STR" ]] && echo "  flags: $CC_FLAGS_STR"
```

### Hunk 4 — `cmd_exec` auto-detect + persist

Same auto-detect + CC_CONFIG_DIR heredoc line in `cmd_exec()` (no echo change there since exec immediately chains into start):

```bash
  # Auto-detect Claude config dir (account) if not explicitly set.
  [[ -z "${CC_CONFIG_DIR:-}" ]] && CC_CONFIG_DIR="$(pick_default_config_dir "$resolved_dir")"

  # Register if not exists, or update
  local file="$CC_SESSIONS/$name.env"
  cat > "$file" <<EOF
# amux session: $name
# registered: $(date -Iseconds)
CC_NAME="$name"
CC_DIR="$resolved_dir"
CC_FLAGS="${CC_FLAGS_STR}"
CC_CONFIG_DIR="$CC_CONFIG_DIR"
EOF
```

### Hunk 5 — `cmd_start` shell_setup: inject export + fix OAuth check

In `cmd_start()`, replace the existing `shell_setup` block (the one that unsets `CLAUDECODE` and conditionally `ANTHROPIC_API_KEY`) with:

```bash
  local shell_setup=""
  if [[ "$provider" != "codex" ]]; then
    shell_setup="unset CLAUDECODE CLAUDE_CODE_ENTRYPOINT; "
    # Per-session Claude config dir (account isolation: work vs personal vs ...).
    # Injecting the export *inside* the tmux command ensures correctness
    # regardless of whether the tmux server was started before or after amux
    # itself had CLAUDE_CONFIG_DIR set — tmux server env doesn't update on
    # client reconnect.
    if [[ -n "${CC_CONFIG_DIR:-}" ]]; then
      shell_setup="${shell_setup}export CLAUDE_CONFIG_DIR=$(printf '%q' "$CC_CONFIG_DIR"); "
    fi
    # OAuth check: look in the *session's* config dir so ANTHROPIC_API_KEY only
    # gets unset when this session's target account actually has OAuth.
    local oauth_file="${CC_CONFIG_DIR:-$HOME}/.claude.json"
    if [[ -f "$oauth_file" ]] && grep -q '"oauthAccount"' "$oauth_file" 2>/dev/null; then
      shell_setup="${shell_setup}unset ANTHROPIC_API_KEY; "
    fi
  fi
```

Two semantic changes here:
1. Injects `export CLAUDE_CONFIG_DIR=...` into the tmux command (not just amux's own env) so it propagates regardless of tmux-server state.
2. Looks up `.claude.json` under the **session's** config dir, not always `$HOME`, so `ANTHROPIC_API_KEY` is unset only when the target account actually has OAuth.

## Verification (post-reapply)

```bash
bash -n /usr/local/bin/amux && echo "syntax ok"

# Test 1 — Fidoo path auto-picks work
amux register _t1 --yolo --dir "$HOME/Desktop/Projects/Fidoo/fidoo-mobile-apps"
grep CC_CONFIG_DIR ~/.amux/sessions/_t1.env  # expect $HOME/.claude-work

# Test 2 — non-Fidoo path auto-picks personal
amux register _t2 --yolo --dir /tmp
grep CC_CONFIG_DIR ~/.amux/sessions/_t2.env  # expect $HOME/.claude-personal

# Test 3 — explicit --personal overrides Fidoo auto-detect
amux register _t3 --personal --yolo --dir "$HOME/Desktop/Projects/Fidoo/fidoo-mobile-apps"
grep CC_CONFIG_DIR ~/.amux/sessions/_t3.env  # expect $HOME/.claude-personal

# Test 4 — explicit --config-dir
amux register _t4 --config-dir "$HOME/.claude" --yolo --dir /tmp
grep CC_CONFIG_DIR ~/.amux/sessions/_t4.env  # expect $HOME/.claude

# Cleanup
for s in _t1 _t2 _t3 _t4; do amux rm $s; done
```

All four lines should print the expected paths.

### Hunk 6 — `cmd_start`: auto-append `--remote-control` and bump default model to Opus 4.7 (1M)

In `cmd_start()`, in the **claude branch** of the codex-vs-claude conditional (the `else` of `if [[ "$provider" == "codex" ]]`), replace:

```bash
  else
    [[ "$cmd" == *"--model"* ]] || cmd="$cmd --model sonnet"
  fi
```

with:

```bash
  else
    # Always enable Claude Code's per-session remote-control URL
    # (claude.ai/code/session_XXX). Complementary to amux's dashboard — gives
    # a stable per-session browser handle for that one session.
    # Opt out per host with: AMUX_NO_REMOTE_CONTROL=1
    [[ "${AMUX_NO_REMOTE_CONTROL:-}" == "1" ]] || [[ "$cmd" == *"--remote-control"* ]] || cmd="$cmd --remote-control"
    # Default model: opus 4.7 with 1M-context. Override per-session with --model
    # on amux register, or globally with $AMUX_DEFAULT_MODEL.
    local default_model="${AMUX_DEFAULT_MODEL:-claude-opus-4-7[1m]}"
    [[ "$cmd" == *"--model"* ]] || cmd="$cmd --model $(printf '%q' "$default_model")"
  fi
```

Two changes wrapped together:
1. **Auto-append `--remote-control`** — every claude launch under amux now also gets Claude Code's own remote-control web URL (claude.ai/code/...). amux's dashboard remains the primary control surface; this is an extra handle for driving a single session from a browser. Set `AMUX_NO_REMOTE_CONTROL=1` to skip.
2. **Default model = `claude-opus-4-7[1m]`** (Opus 4.7 with 1M context window) instead of upstream's `sonnet`. Brackets are escape-safe via `printf '%q'`. Override per-session by passing `--model X` to `amux register`, or globally with `AMUX_DEFAULT_MODEL`.

Both apply only to the claude provider — the codex branch is untouched.

### Hunk 6 verification

```bash
# After deploy, verify the new defaults appear in cmd_start's command-build path:
grep -nE "(remote-control|AMUX_DEFAULT_MODEL|claude-opus-4-7\[1m\])" /usr/local/bin/amux
```

Should match 3 lines: the `--remote-control` line, the `AMUX_DEFAULT_MODEL` lookup line, and the comment referencing the model id.

Quick logic check (no real claude launch needed):
```bash
bash <<'T'
cmd="claude"
[[ "${AMUX_NO_REMOTE_CONTROL:-}" == "1" ]] || [[ "$cmd" == *"--remote-control"* ]] || cmd="$cmd --remote-control"
default_model="${AMUX_DEFAULT_MODEL:-claude-opus-4-7[1m]}"
[[ "$cmd" == *"--model"* ]] || cmd="$cmd --model $(printf '%q' "$default_model")"
echo "$cmd"
T
# expect: claude --remote-control --model claude-opus-4-7\[1m\]
```

### Hunk 7 — `parse_claude_flags`: `--yolo` (`--dangerously-skip-permissions`) ON by default

amux sessions run unattended inside tmux, so Claude Code's permission prompts deadlock them. Make `--dangerously-skip-permissions` the default for every new session, with explicit opt-outs.

Two parts:

**7a.** Add a `--no-yolo` case to the boolean-flag section of `parse_claude_flags`. Insert immediately after the existing `--yolo|--verbose|...)` boolean-flag block:

```bash
      # Opt-out of the implicit yolo default (see end of parse_claude_flags).
      # Use this when you explicitly want Claude Code's normal permission prompts.
      --no-yolo)
        CC_NO_YOLO=1
        shift
        ;;
```

**7b.** Just before the final `CC_FLAGS_STR="${CC_FLAGS[*]:-}"` at the end of `parse_claude_flags`, insert the default-injection block:

```bash
  # Default: enable --dangerously-skip-permissions (a.k.a. --yolo). amux sessions
  # run unattended inside tmux, so permission prompts deadlock them. Opt out per
  # session with --no-yolo, or globally with AMUX_NO_YOLO=1.
  if [[ "${CC_NO_YOLO:-}" != "1" && "${AMUX_NO_YOLO:-}" != "1" ]]; then
    local already_yolo=0 f
    for f in "${CC_FLAGS[@]:-}"; do
      case "$f" in
        --dangerously-skip-permissions|--allow-dangerously-skip-permissions) already_yolo=1; break ;;
      esac
    done
    [[ $already_yolo -eq 0 ]] && CC_FLAGS+=("--dangerously-skip-permissions")
  fi
```

Help text update (line ~1075 area): note `(default ON)` and add a `--no-yolo` row.

### Hunk 7 verification

```bash
# Default ON
amux register _y1 --dir /tmp >/dev/null && grep CC_FLAGS ~/.amux/sessions/_y1.env
# expect: CC_FLAGS="--dangerously-skip-permissions"

# --no-yolo opts out
amux register _y2 --no-yolo --dir /tmp >/dev/null && grep CC_FLAGS ~/.amux/sessions/_y2.env
# expect: CC_FLAGS=""

# AMUX_NO_YOLO env opts out
AMUX_NO_YOLO=1 amux register _y3 --dir /tmp >/dev/null && grep CC_FLAGS ~/.amux/sessions/_y3.env
# expect: CC_FLAGS=""

# Explicit --yolo not duplicated
amux register _y4 --yolo --verbose --dir /tmp >/dev/null && grep CC_FLAGS ~/.amux/sessions/_y4.env
# expect: CC_FLAGS="--dangerously-skip-permissions --verbose"

for s in _y1 _y2 _y3 _y4; do amux rm $s; done
```

## Known limitations

- ~~Dashboard-created sessions don't get auto-detect.~~ **Resolved 2026-06-12:** `amux-server.py` now auto-detects at every `start_session()` (explicit `CC_CONFIG_DIR` in the env file still wins), and POST `/api/sessions` accepts an optional `config_dir` field (`"work"`, `"personal"`, or an absolute path).
- **Existing session env files without `CC_CONFIG_DIR`** now get the auto-detect at start time (work for `$AMUX_WORK_PATHS` dirs, personal otherwise) instead of falling through to `$HOME`. Pin a session to a specific account by adding `CC_CONFIG_DIR="<path>"` to its `~/.amux/sessions/<name>.env`.
- **First start after switching accounts starts fresh.** Transcripts written under `~/.claude/projects` before the routing landed aren't visible from `~/.claude-work` / `~/.claude-personal`, so the first routed start of an old session can't `--resume` its previous conversation.

## Re-running the deploy

After editing `amux` in this fork:

```bash
bash -n amux                         # syntax
cp amux /usr/local/bin/amux          # deploy CLI only
# (don't run ./install.sh unless you also want to push amux-server.py)
```

`install.sh` deploys **both** the CLI and the server. If your `amux-server.py` has unrelated local edits that aren't ready, use the explicit `cp` instead.
