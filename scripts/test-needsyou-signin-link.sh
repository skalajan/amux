#!/usr/bin/env bash
# AMUX-3073 regression: `amux board needsyou <id> "<sign-in ask>"` must attach a
# clickable resume deep link (#browser=<session>) into the card's desc, so the
# needs-you note can say "click here to sign in" — the reported gap was that even
# after raising needs-you there was NO route to a session's browser view.
#
# This is a CLI-side write (the desc_append PATCH body is built in the shell), so
# NO server log could catch a regression — this test is the guard, same as
# test-register-merge.sh. It runs the REAL shipped verb against a MOCK curl that
# captures the PATCH bodies, so it needs no live server and mutates no board.
set -uo pipefail
cd "$(dirname "$0")/.."
AMUX_BIN="${AMUX_BIN:-./amux}"
PASS=0; FAIL=0
has()  { if grep -qF -- "$2" "$1"; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "FAIL: $3 (missing '$2')"; fi; }
hasnt(){ if grep -qF -- "$2" "$1"; then FAIL=$((FAIL+1)); echo "FAIL: $3 (unexpected '$2')"; else PASS=$((PASS+1)); fi; }
count(){ if [ "$2" = "$(grep -oF -- "$3" "$1" | wc -l | tr -d ' ')" ]; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "FAIL: $4 (expected $2 of '$3', got $(grep -oF -- "$3" "$1" | wc -l | tr -d ' '))"; fi; }

TMP=$(mktemp -d)
# Mock curl: capture every PATCH -d body; canned JSON for the tag-fetch GET.
mkdir -p "$TMP/bin"
cat > "$TMP/bin/curl" <<'MOCK'
#!/usr/bin/env bash
body=""; is_patch=0; args=("$@")
for ((i=0;i<${#args[@]};i++)); do
  case "${args[i]}" in
    -X) [[ "${args[$((i+1))]}" == "PATCH" ]] && is_patch=1 ;;
    -d) body="${args[$((i+1))]}" ;;
  esac
done
if [[ $is_patch -eq 1 ]]; then
  printf '%s\n' "$body" >> "$CAPTURE"
  echo '{"ok":true,"item":{"id":"TEST-1","status":"needsyou","tags":["needs:you"]}}'
else
  echo '{"item":{"id":"TEST-1","status":"doing","type":"code","tags":[]}}'
fi
MOCK
chmod +x "$TMP/bin/curl"
export PATH="$TMP/bin:$PATH"
export AMUX_API="https://localhost:9999"   # never contacted — curl is mocked
export AMUX_SESSION="wtest" AMUX_WORKER="wtest"

run() { export CAPTURE="$1"; : > "$CAPTURE"; shift; "$AMUX_BIN" board needsyou TEST-1 "$@" >/dev/null 2>&1 || true; }

# 1. A sign-in ask gets the resume deep link into THIS session's browser.
run "$TMP/c1" "please sign in to Wexus so I can continue the NetSuite export"
has "$TMP/c1" 'NEEDS-YOU:'                              "signin ask recorded"
has "$TMP/c1" '/#browser=wtest'                         "signin ask gets resume deep link for this session"

# 2. An ask that already carries a #browser= link is NOT double-linked.
run "$TMP/c2" "log in here: https://localhost:9999/#browser=amux-gtm"
count "$TMP/c2" 1 '#browser='                           "agent-supplied link not doubled"

# 3. A non-sign-in ask gets NO browser link (no spurious resume path).
run "$TMP/c3" "need your decision on the pricing tier before I proceed"
has   "$TMP/c3" 'NEEDS-YOU:'                             "plain ask still recorded"
hasnt "$TMP/c3" '#browser='                             "plain ask gets no browser link"

rm -rf "$TMP"
echo "needsyou sign-in link: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
