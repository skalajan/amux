#!/usr/bin/env bash
# test-cli-stale-url.sh [amux]: a localhost AMUX_API/AMUX_URL on a port that
# endpoint.json does not name is replaced by endpoint.json's canonical_url, and
# logged once; a remote override is left alone (2026-10-05: a reboot race left
# every lane on 8823 and every CLI verb failed with curl exit 7). Also: the CLI
# sends the owner bearer to its own localhost server and names a 401's cause.
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
H="$(mktemp -d "${TMPDIR:-/tmp}/csu.XXXXXX")"; mkdir -p "$H/.amux/state" "$H/srv/api"
PORT=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')
DEAD=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')
echo '[]' > "$H/srv/api/board"
( cd "$H/srv" && exec python3 -m http.server "$PORT" --bind 127.0.0.1 ) 2> "$H/srv.log" &
SRV=$!
trap 'kill $SRV 2>/dev/null; cd /; rm -rf -- "${H:?}"' EXIT
for _ in $(seq 1 50); do curl -s "http://127.0.0.1:$PORT/api/board" >/dev/null 2>&1 && break; sleep 0.1; done
printf '{"canonical_port":%s,"canonical_url":"http://127.0.0.1:%s","legacy_port":8822,"retired_ports":[8822]}\n' "$PORT" "$PORT" > "$H/.amux/endpoint.json"
fail=0
: > "$H/srv.log"
HOME="$H" CC_HOME="$H/.amux" AMUX_SESSION=csu AMUX_API="http://127.0.0.1:$DEAD" AMUX_URL="http://127.0.0.1:$DEAD" \
  bash "$AM" board ls >/dev/null 2>&1
grep -q 'GET /api/board' "$H/srv.log" && echo "ok   a stale localhost AMUX_API reaches endpoint.json's server" || { echo "FAIL the CLI did not reach the canonical server"; fail=1; }
n=$(grep -c 'stale_env_url_healed' "$H/.amux/cli-transport.jsonl" 2>/dev/null || echo 0)
[ "$n" -ge 1 ] && echo "ok   the heal is logged ($n line(s))" || { echo "FAIL no stale_env_url_healed line"; fail=1; }
HOME="$H" CC_HOME="$H/.amux" AMUX_SESSION=csu AMUX_API="http://127.0.0.1:$DEAD" bash "$AM" board ls >/dev/null 2>&1
n2=$(grep -c "\"var\":\"AMUX_API\"" "$H/.amux/cli-transport.jsonl" 2>/dev/null || echo 0)
[ "$n2" = 1 ] && echo "ok   logged once per lane and port, not per call" || { echo "FAIL AMUX_API heal logged $n2 times"; fail=1; }
out=$(grep -n '_amux_heal_url()' -A12 "$AM" | grep -c 'https://localhost:\*|http://localhost:\*|https://127.0.0.1:\*|http://127.0.0.1:\*) ;; \*) continue')
[ "$out" = 1 ] && echo "ok   only localhost URLs are healed; a remote override is left alone" || { echo "FAIL the localhost guard is missing"; fail=1; }

# AMUX_RS_NO_LOOPBACK_BYPASS=1 answers a tokenless localhost caller 401 (mac-server, 2026-11-03).
H2="$H/bearer"; mkdir -p "$H2/.amux/sessions"
AP=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')
cat > "$H/authsrv.py" <<'PY'
import http.server, json, sys
class H(http.server.BaseHTTPRequestHandler):
    def answer(self):
        auth = self.headers.get("Authorization")
        open(sys.argv[2], "a").write(f"{self.command} {self.path} {'bearer' if auth else 'none'}\n")
        if auth == "Bearer s3cret":
            code, body = 200, {"ok": True, "message": "started"}
        else:
            code, body = 401, {"error": "unauthorized", "reason": "invalid_bearer" if auth else "missing_credential", "recovery": "open_owner_or_invite_link"}
        raw = json.dumps(body, separators=(",", ":")).encode()
        self.send_response(code); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw))); self.end_headers(); self.wfile.write(raw)
    do_GET = do_POST = answer
    def log_message(self, *a): pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PY
python3 "$H/authsrv.py" "$AP" "$H/auth.log" &
ASRV=$!
trap 'kill $SRV $ASRV 2>/dev/null; cd /; rm -rf -- "${H:?}"' EXIT
for _ in $(seq 1 50); do curl -s "http://127.0.0.1:$AP/" >/dev/null 2>&1 && break; sleep 0.1; done
start() { HOME="$H2" CC_HOME="$H2/.amux" AMUX_API="http://127.0.0.1:$AP" AMUX_SESSION=csu AMUX_CURL_RESTART_WAIT=0 \
  bash -c 'source "$1"; _start_via_api better-life' _ "$AM"; }
row() { tail -1 "$H2/.amux/cli-transport.jsonl" 2>/dev/null; }

out=$(start 2>&1); rc=$?
[ "$rc" != 0 ] && grep -q 'FAILED: unauthorized (missing_credential)' <<<"$out" && grep -q 'wants the owner bearer' <<<"$out" \
  && echo "ok   no auth_token: start fails naming the missing owner bearer" || { echo "FAIL no-token start: rc=$rc $out"; fail=1; }
row | grep -q '"event":"server_unauthorized","reason":"missing_credential","bearer_sent":false' \
  && echo "ok   the refusal is logged as server_unauthorized" || { echo "FAIL no server_unauthorized row: $(row)"; fail=1; }

printf 'wrong-token\n' > "$H2/.amux/auth_token"
out=$(start 2>&1); rc=$?
[ "$rc" != 0 ] && grep -q 'refused the credential this request carried (invalid_bearer)' <<<"$out" \
  && row | grep -q '"reason":"invalid_bearer","bearer_sent":true' \
  && echo "ok   a wrong auth_token is named as the cause" || { echo "FAIL wrong-token start: rc=$rc $out $(row)"; fail=1; }
if grep -q 'wrong-token' "$H2/.amux/cli-transport.jsonl" || grep -q 'wrong-token' <<<"$out"; then
  echo "FAIL the token leaked into the output or the breadcrumb"; fail=1
else echo "ok   the token is in neither the output nor the breadcrumb"; fi

printf 's3cret\n' > "$H2/.amux/auth_token"
out=$(start 2>&1); rc=$?
[ "$rc" = 0 ] && grep -q 'started better-life' <<<"$out" && grep -q 'POST /api/sessions/better-life/start bearer' "$H/auth.log" \
  && echo "ok   with auth_token the start carries the bearer and succeeds" || { echo "FAIL bearer start: rc=$rc $out"; fail=1; }
remote=$(CC_HOME="$H2/.amux" bash -c 'source "$1"; _owner_bearer https://example.com:8824/api/x' _ "$AM")
[ -z "$remote" ] && echo "ok   the owner token is never sent to another host" || { echo "FAIL the token would go to a remote host"; fail=1; }
exit $fail
