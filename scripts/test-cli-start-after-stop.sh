#!/usr/bin/env bash
# `amux start` after a stop. The server's stop ends the agent and keeps its tmux
# pane (a bare shell), so `tmux has-session` stays true. `amux start` read that
# as "already running" and refused until the session was killed by hand, and
# `amux stop` waited its full 30 seconds for a session that never goes away.
#
# Runs the real CLI against a PRIVATE tmux server (its own TMUX_TMPDIR, $TMUX
# dropped) and a throwaway CC_HOME, with a stub `claude` that just sleeps, so no
# real worker and not the fleet's tmux server is touched.
#
# Cases, each checked on its outcome:
#   A  pane holds only a shell        -> start replaces it and launches
#   B  pane shell has a live child    -> "already running", pane untouched
#   C  pane process is the agent      -> "already running" (an exec'd launch has
#      itself, not a shell               no children and must not look empty)
#   D  agent_alive (what stop waits on) is false for A's shape, true for B's
# plus a negative control: the CLI with the new check disabled fails case A.
set -euo pipefail
cd "$(dirname "$0")/.." || exit 1
AMUX_BIN="${AMUX_BIN:-./amux}"
command -v tmux >/dev/null || { echo "tmux is required for this test" >&2; exit 1; }
PASS=0; FAIL=0
ok()  { echo "  ok   $1"; PASS=$((PASS+1)); }
bad() { echo "  FAIL $1"; [ -n "${2:-}" ] && echo "       $2"; FAIL=$((FAIL+1)); }

WORK=$(mktemp -d)
export TMUX_TMPDIR="$WORK/tmux"; mkdir -p "$TMUX_TMPDIR"
unset TMUX TMUX_PANE
# The CLI runs bare `tmux`, which TMUX_TMPDIR points at this socket. The test's
# own calls name the socket outright, so none of them can reach another server.
SOCK="$TMUX_TMPDIR/tmux-$(id -u)/default"
mkdir -p -m 700 "${SOCK%/*}"
t() { tmux -S "$SOCK" "$@"; }
cleanup() { t kill-server >/dev/null 2>&1 || true; rm -rf "$WORK"; }
trap cleanup EXIT

FAKE_HOME="$WORK/home"; CC="$FAKE_HOME/.amux"
mkdir -p "$CC/sessions" "$WORK/workdir" "$WORK/bin"
cat > "$CC/sessions/sas.env" <<ENVEOF
CC_DIR="$WORK/workdir"
CC_PROVIDER="claude"
ENVEOF
# The agent: a process that stays up, so the launched pane has a live child.
printf '#!/usr/bin/env bash\nexec sleep 300\n' > "$WORK/bin/claude"
chmod +x "$WORK/bin/claude"

# AMUX_API names a port nothing listens on: with a server up, `amux start`
# hands a claude launch to it, and a test must never reach the live one. The
# empty-pane check runs before that hand-off, so the local path covers it.
run_start() {
  env -u AMUX_URL AMUX_API=https://127.0.0.1:9 PATH="$WORK/bin:$PATH" HOME="$FAKE_HOME" CC_HOME="$CC" \
    AMUX_SESSION=sas-test AMUX_WORKER=sas-test \
    bash "$1" start sas --detach >"$WORK/out" 2>&1
}
pane_pid() { t list-panes -t "=amux-sas:" -F '#{pane_pid}' 2>/dev/null | head -1; }
fresh_session() {
  t kill-session -t "=amux-sas" >/dev/null 2>&1 || true
  t new-session -d -s amux-sas -c "$WORK/workdir" "$@"
  sleep 0.5
}
has_live_child() {
  local kids; kids=$(pgrep -P "$1" | paste -sd, -) || return 1
  [ -n "$kids" ] && ps -p "$kids" -o stat= | grep -qv '^[[:space:]]*Z'
}

# ── A: the shape a server stop leaves ─────────────────────────────────────────
case_a() {
  fresh_session bash --noprofile --norc
  if run_start "$1"; then RC=0; else RC=$?; fi
  OUT=$(cat "$WORK/out"); AFTER=$(pane_pid)
}
case_a "$AMUX_BIN"
if [ "$RC" -eq 0 ] && grep -q "started" <<<"$OUT" && ! grep -q "already running" <<<"$OUT" \
   && [ -n "$AFTER" ] && has_live_child "$AFTER"; then
  ok "A: a shell-only session is replaced and the worker launches"
else
  bad "A: start after stop did not launch (rc=$RC)" "$OUT"
fi
if grep -aq "verdict=empty_pane_replaced session=sas" "$CC/logs/server-rs.log" 2>/dev/null; then
  ok "A: the replacement is logged (verdict=empty_pane_replaced)"
else
  bad "A: no empty_pane_replaced line in server-rs.log"
fi

# ── B: a live worker (shell with a running child) ─────────────────────────────
fresh_session bash --noprofile --norc -c 'sleep 300; true'
BEFORE=$(pane_pid)
if run_start "$AMUX_BIN"; then RC=0; else RC=$?; fi; OUT=$(cat "$WORK/out")
if grep -q "already running" <<<"$OUT" && [ "$(pane_pid)" = "$BEFORE" ] && has_live_child "$BEFORE"; then
  ok "B: a live worker is left alone (already running)"
else
  bad "B: a live worker was not reported as running, or its pane changed" "$OUT"
fi

# ── C: the agent is the pane process itself, with no children ─────────────────
fresh_session sleep 300
BEFORE=$(pane_pid)
if run_start "$AMUX_BIN"; then RC=0; else RC=$?; fi; OUT=$(cat "$WORK/out")
if grep -q "already running" <<<"$OUT" && [ "$(pane_pid)" = "$BEFORE" ]; then
  ok "C: an exec'd agent with no children is not mistaken for an empty pane"
else
  bad "C: an exec'd agent was replaced" "$OUT"
fi

# ── D: the predicate stop waits on ────────────────────────────────────────────
probe() {
  env HOME="$FAKE_HOME" CC_HOME="$CC" bash -c \
    'source "$1" >/dev/null 2>&1; set +e; agent_alive sas; echo "rc=$?"' _ "$AMUX_BIN"
}
fresh_session bash --noprofile --norc
D_EMPTY=$(probe)
fresh_session bash --noprofile --norc -c 'sleep 300; true'
D_LIVE=$(probe)
if [ "$D_EMPTY" = "rc=1" ] && [ "$D_LIVE" = "rc=0" ]; then
  ok "D: agent_alive is false for a shell-only pane and true for a live one"
else
  bad "D: agent_alive gave empty=$D_EMPTY live=$D_LIVE (want rc=1 and rc=0)"
fi

# ── E: `amux attach` / `amux here` never attach to a shell-only pane ──────────
E_BIN=$(realpath "$AMUX_BIN")
run_verb() {
  (cd "$WORK/workdir" && env -u AMUX_URL AMUX_API=https://127.0.0.1:9 AMUX_CURL_RESTART_WAIT=0 PATH="$WORK/bin:$PATH" \
    HOME="$FAKE_HOME" CC_HOME="$CC" bash "$E_BIN" "$@" </dev/null >"$WORK/out" 2>&1) || true
  cat "$WORK/out"
}
fresh_session bash --noprofile --norc
E_ATTACH=$(run_verb attach sas); E_HERE=$(run_verb here)
fresh_session bash --noprofile --norc -c 'sleep 300; true'
E_LIVE=$(run_verb attach sas)
if grep -q "is stopped" <<<"$E_ATTACH" && grep -q "NOTHING was started" <<<"$E_HERE" && ! grep -q "is stopped" <<<"$E_LIVE"; then
  ok "E: attach refuses and here starts (not attaches to) a shell-only pane; a live pane is not refused"
else
  bad "E: attach/here on a shell-only pane" "attach: $E_ATTACH | here: $E_HERE | live: $E_LIVE"
fi

# ── F: a tmux probe that times out is unknown, never a stopped worker ─────────
mkdir -p "$WORK/slow"
printf '#!/usr/bin/env bash\n[ "$1" = has-session ] && sleep 2\nexec %q "$@"\n' "$(command -v tmux)" > "$WORK/slow/tmux"
chmod +x "$WORK/slow/tmux"
fresh_session bash --noprofile --norc
F_ATTACH=$(PATH="$WORK/slow:$PATH" AMUX_TMUX_PROBE_TIMEOUT_S=1 run_verb attach sas)
F_HERE=$(PATH="$WORK/slow:$PATH" AMUX_TMUX_PROBE_TIMEOUT_S=1 run_verb here)
if ! grep -q "is stopped" <<<"$F_ATTACH" && ! grep -q "NOTHING was started" <<<"$F_HERE"; then
  ok "F: with a timed-out tmux probe, attach does not call the worker stopped and here sends no start"
else
  bad "F: a timed-out probe was read as a stopped worker" "attach: $F_ATTACH | here: $F_HERE"
fi

# ── G: `amux here` in a subfolder offers the parent's worker, never takes it unasked ──
mkdir -p "$WORK/workdir/sub"
SUB=$(cd "$WORK/workdir/sub" && pwd -P)
# Runs `amux here` in the subfolder on a pty (the choice is read from /dev/tty), typing $1.
here_tty() {
  (cd "$SUB" && env -u AMUX_URL AMUX_API=https://127.0.0.1:9 AMUX_CURL_RESTART_WAIT=0 PATH="$WORK/bin:$PATH" \
    HOME="$FAKE_HOME" CC_HOME="$CC" KEYS="$1" python3 -c '
import os, pty, select, sys, time
pid, fd = pty.fork()
if pid == 0:
    os.execvp("bash", ["bash", sys.argv[1], "here"])
os.write(fd, os.environ["KEYS"].encode())
end = time.time() + 60
while time.time() < end and select.select([fd], [], [], 1)[0]:
    try:
        data = os.read(fd, 4096)
    except OSError:
        break
    if not data:
        break
    sys.stdout.write(data.decode(errors="replace"))
' "$E_BIN") 2>&1 || true
}
fresh_session bash --noprofile --norc
G_PIPE=$(cd "$SUB" && env -u AMUX_URL AMUX_API=https://127.0.0.1:9 AMUX_CURL_RESTART_WAIT=0 PATH="$WORK/bin:$PATH" \
  HOME="$FAKE_HOME" CC_HOME="$CC" bash "$E_BIN" here </dev/null 2>&1) || true
G_ENTER=$(here_tty $'\n')
G_NEW=$(here_tty $'n\n\nn\n')
if grep -q "NOTHING was started" <<<"$G_PIPE" && ! grep -q "a new worker here" <<<"$G_PIPE" \
  && grep -q "a new worker here" <<<"$G_ENTER" && grep -q "NOTHING was started" <<<"$G_ENTER" \
  && grep -q "a new worker here" <<<"$G_NEW" && grep -q "nothing created" <<<"$G_NEW" \
  && [ ! -e "$CC/sessions/sub.env" ]; then
  ok "G: in a subfolder, here asks on a terminal (Enter = the parent's worker, n = create here) and keeps the parent's worker without one"
else
  bad "G: here in a subfolder of a worker's folder" "pipe: $G_PIPE | enter: $G_ENTER | new: $G_NEW"
fi
G_FORCE=$(cd "$SUB" && env -u AMUX_URL AMUX_API=https://127.0.0.1:9 AMUX_CURL_RESTART_WAIT=0 PATH="$WORK/bin:$PATH" \
  HOME="$FAKE_HOME" CC_HOME="$CC" bash "$E_BIN" here --new -y </dev/null 2>&1) || true
if grep -q "^CC_DIR=\"$SUB\"" "$CC/sessions/sub.env" 2>/dev/null; then
  ok "G: here --new registers a worker for the subfolder instead of using the parent's"
else
  bad "G: here --new did not register a worker for the subfolder" "$G_FORCE"
fi
rm -f "$CC/sessions/sub.env"

# ── H: `amux here` on a terminal offers this folder's workers and a new one, and asks the new one's name ──
# Runs `amux here` in the worker's folder on a pty and types $1; $2 = seconds to wait, or "attach" to wait until the worker is attached
# (prints ATTACHED); then closes the pty, after a detach when $3 = detach. $ASK is AMUX_HERE_ASK_ON_CLOSE.
here_pty() {
  (cd "$WORK/workdir" && env -u AMUX_URL -u SSH_CONNECTION AMUX_HERE_ASK_ON_CLOSE="${ASK:-1}" AMUX_API=https://127.0.0.1:9 AMUX_CURL_RESTART_WAIT=0 PATH="$WORK/bin:$PATH" \
    HOME="$FAKE_HOME" CC_HOME="$CC" KEYS="$1" HOLD="$2" END="${3:-eof}" SOCK="$SOCK" python3 -c '
import os, pty, select, subprocess, sys, time
pid, fd = pty.fork()
if pid == 0:
    os.execvp("bash", ["bash", sys.argv[1], "here"])
def drain(secs):
    end = time.time() + secs
    while time.time() < end:
        if not select.select([fd], [], [], 0.2)[0]:
            continue
        try:
            data = os.read(fd, 4096)
        except OSError:
            return
        if not data:
            return
        sys.stdout.write(data.decode(errors="replace"))
os.write(fd, os.environ["KEYS"].encode())
if os.environ["HOLD"] == "attach":
    end = time.time() + 60
    while time.time() < end:
        drain(0.3)
        if subprocess.run(["tmux", "-S", os.environ["SOCK"], "list-clients", "-t", "=amux-sas"], capture_output=True).stdout.strip():
            print("ATTACHED")
            break
    drain(1)
else:
    drain(float(os.environ["HOLD"]))
if os.environ["END"] == "detach":
    subprocess.run(["tmux", "-S", os.environ["SOCK"], "detach-client", "-s", "=amux-sas"])
    drain(3)
os.close(fd)
' "$E_BIN") 2>&1 || true
}
cat > "$CC/sessions/sas2.env" <<ENVEOF
CC_DIR="$WORK/workdir"
CC_PROVIDER="claude"
ENVEOF
fresh_session bash --noprofile --norc -c 'sleep 300; true'
H_MENU=$(here_pty $'x\n' 8)
H_NAME=$(here_pty $'n\nBad Name\nsas\nthird\nn\n' 12)
if grep -q "1) sas .*running" <<<"$H_MENU" && grep -q "2) sas2 .*stopped" <<<"$H_MENU" \
  && grep -q "n) a new worker" <<<"$H_MENU" && grep -q "nothing picked" <<<"$H_MENU"; then
  ok "H: here lists the folder's workers with their state, the running one first, plus a new worker"
else
  bad "H: the menu of a folder with two workers" "$H_MENU"
fi
if grep -q "New worker in" <<<"$H_NAME" && grep -q "a name is made of" <<<"$H_NAME" && grep -q "worker 'sas' already exists" <<<"$H_NAME" \
  && grep -q "name:    third" <<<"$H_NAME" && grep -q "nothing created" <<<"$H_NAME" && [ ! -e "$CC/sessions/third.env" ]; then
  ok "H: a new worker's name is asked, and a bad or a taken name is asked again"
else
  bad "H: naming a new worker" "$H_NAME"
fi
rm -f "$CC/sessions/sas2.env"

# ── I: a closed terminal asks whether the worker stops; a detach never asks ───
# The stub answers the dialog from $OSA_ANSWER and leaves a mark, so a test never opens a real one.
printf '#!/usr/bin/env bash\necho asked >> "%s"\necho "${OSA_ANSWER:-Keep running}"\n' "$WORK/asked" > "$WORK/bin/osascript"
chmod +x "$WORK/bin/osascript"
wait_asked() { local i; for i in $(seq 1 40); do [ -e "$WORK/asked" ] && return 0; sleep 0.25; done; return 1; }
live_agent() { local p; p=$(pane_pid); [ -n "$p" ] && has_live_child "$p"; }
fresh_session bash --noprofile --norc -c 'sleep 300; true'
rm -f "$WORK/asked"; I_OUT=$(here_pty $'\n' attach detach); sleep 2
if grep -q ATTACHED <<<"$I_OUT" && [ ! -e "$WORK/asked" ] && live_agent; then
  ok "I: a detach asks nothing and the worker runs on"
else
  bad "I: a detach asked about the worker or ended it" "$I_OUT"
fi
rm -f "$WORK/asked"; I_OUT=$(ASK=0 here_pty $'\n' attach); sleep 3
if grep -q ATTACHED <<<"$I_OUT" && [ ! -e "$WORK/asked" ] && live_agent; then
  ok "I: AMUX_HERE_ASK_ON_CLOSE=0 turns the question off"
else
  bad "I: the question was asked with AMUX_HERE_ASK_ON_CLOSE=0" "$I_OUT"
fi
rm -f "$WORK/asked"; I_OUT=$(here_pty $'\n' attach)
if grep -q ATTACHED <<<"$I_OUT" && wait_asked && sleep 2 && live_agent; then
  ok "I: a closed terminal asks, and \"Keep running\" keeps the worker"
else
  bad "I: closing the terminal did not ask, or the worker ended on \"Keep running\""
fi
rm -f "$WORK/asked"; I_OUT=$(OSA_ANSWER="Stop worker" here_pty $'\n' attach)
I_GONE=no
if grep -q ATTACHED <<<"$I_OUT" && wait_asked; then for _ in $(seq 1 40); do t has-session -t "=amux-sas" 2>/dev/null || { I_GONE=yes; break; }; sleep 0.25; done; fi
if [ "$I_GONE" = yes ]; then
  ok "I: \"Stop worker\" stops the worker whose terminal was closed"
else
  bad "I: \"Stop worker\" left the worker running"
fi

# ── negative control: without the replacement, case A must fail ───────────────
BROKEN="$WORK/amux-broken"
sed 's/if \[\[ "\$_pane_rc" == 1 \]\]; then/if false; then/' "$AMUX_BIN" > "$BROKEN"
if cmp -s "$AMUX_BIN" "$BROKEN"; then
  bad "negative control: the fixture is identical to the CLI (has the specimen drifted?)"
else
  case_a "$BROKEN"
  if grep -q "already running" <<<"$OUT"; then
    ok "negative control: the CLI without the fix reports a stopped worker as already running"
  else
    bad "negative control: the broken CLI still launched; this test cannot fail" "$OUT"
  fi
fi

echo
echo "  $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
