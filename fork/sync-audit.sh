#!/usr/bin/env bash
# Egress / exposure / telemetry audit of an upstream range before it is merged into the fork.
# usage: fork/sync-audit.sh [RANGE]   (default HEAD...upstream/main, i.e. only what upstream changed); exit 0 = no hits, 1 = hits to review, 2 = usage error
set -euo pipefail

cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
range="${1:-HEAD...upstream/main}"
git rev-parse -q --verify "${range##*..}^{commit}" >/dev/null || { echo "no such revision: ${range##*..}" >&2; exit 2; }

# Runtime code only: the marketing site, docs and tests never run on the owner's machines.
paths=(. ':(exclude)site/**' ':(exclude)docs/**' ':(exclude)fork-archive/**' ':(exclude)**/*.md'
       ':(exclude)tests/**' ':(exclude)**/tests/**' ':(exclude)**/*_test.*' ':(exclude)**/*.test.*'
       ':(exclude)**/*.spec.*' ':(exclude)**/test_*.py' ':(exclude)**/test-*.sh' ':(exclude)**/test/**'
       ':(exclude)**/fixtures/**' ':(exclude)Cargo.lock' ':(exclude)**/package-lock.json')

hits=0
section() {
  local title="$1" re="$2" out
  out=$(git diff -U0 "$range" -- "${paths[@]}" \
        | RE="$re" awk '
            /^\+\+\+ /{f=substr($0,7); next}
            /^@@/{split($3,a,","); ln=substr(a[1],2)+0; next}
            /^\+[[:space:]]*(\/\/|#[^\[!]|#$|\*|<!--)/{ln++; next}
            /^\+/{ if (tolower(substr($0,2)) ~ ENVIRON["RE"]) printf "%s:%d: %s\n", f, ln, substr(substr($0,2),1,220); ln++; next }
            /^ /{ln++}' \
        | grep -viE '//[[:space:]]*(test|example)|localhost|127\.0\.0\.1|\[::1\]' || true)
  if [[ -n "$out" ]]; then
    hits=$((hits + $(printf '%s\n' "$out" | wc -l)))
    printf '\n== %s (%s)\n%s\n' "$title" "$(printf '%s\n' "$out" | wc -l | tr -d ' ')" "$out"
  fi
}

echo "range: $range ($(git rev-list --count "$range") commits)"

section "Upstream-owned or third-party hosts" \
  'mixpeek|amux\.io|getamux|amuxapp|amux\.dev|amux\.sh|amux\.so|github\.com/mixpeek|api\.github\.com|googleapis|google-analytics|googletagmanager|gstatic|cdn\.|unpkg|jsdelivr|cloudflare|vercel|netlify|fly\.dev|render\.com|supabase|firebase|https?://[a-z0-9.-]+\.[a-z]{2,}'
section "Telemetry / analytics / reporting" \
  'telemetry|analytic|posthog|gtag|sentry|segment\.(io|com)|mixpanel|amplitude|datadog|honeycomb|opentelemetry|otlp|beacon|phone.?home|usage.?(report|stat|ping)|crash.?report|error.?report|heartbeat_url|report_url|metrics_url|install_id|anonymous_id|machine_id|instance_id'
section "Outbound clients" \
  'client::builder|httptransport|ctx\.http\.|urlsession|url\(string: "https|import\(.https|\.src *= *.https|@latest|env::var\("[a-z_]*_(url|base|gateway|host|endpoint)"|reqwest::|hyper::client|hyper_util::client|ureq::|isahc|surf::|tcpstream::connect|udpsocket|tokio_tungstenite|connect_async|websocket\(|new websocket|sendbeacon|xmlhttprequest|fetch\(.https?:|urllib|requests\.(get|post|put)|http\.client|(^|[^a-z_])(curl|wget|nc|ncat|socat) |openssl s_client'
section "Inbound exposure" \
  'tcplistener::bind|bind_rustls|\[0, 0, 0, 0\]|public_paths|public_prefixes|public_routes|require_bearer|0\.0\.0\.0|\[::\]|inaddr_any|unspecified\(|bind_addr|listen_addr|access-control-allow-origin|cors|public_path|public_route|no_auth|skip_auth|unauthenticated|loopback_bypass|trust_loopback|tunnel|ngrok|cloudflared|tailscale.*funnel|funnel (on|--)|localtunnel|serveo|relay|webhook|port_forward|upnp'
section "Remote code / self-update" \
  'self.?update|auto.?update|update_check|check_for_update|latest_release|releases/latest|\| *(sh|bash)\b|install\.sh|download.*(binary|release)|eval "\$\(curl'

echo
echo "== New or changed dependencies"
base=$(git merge-base "${range%%...*}" "${range##*...}" 2>/dev/null || git rev-parse "${range%%..*}")
dep=$(python3 - "$base" "${range##*.}" <<'PY'
import json, re, subprocess, sys
def show(rev, path):
    r = subprocess.run(["git", "show", f"{rev}:{path}"], capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else ""
def cargo(rev):
    return {(m[0], m[1]) for m in re.findall(r'\[\[package\]\]\nname = "([^"]+)"\nversion = "([^"]+)"', show(rev, "Cargo.lock"))}
def npm(rev):
    out = set()
    for path in subprocess.run(["git", "ls-tree", "-r", "--name-only", rev], capture_output=True, text=True).stdout.split():
        if path.endswith("package.json") and "node_modules" not in path and not path.startswith("site/"):
            try:
                d = json.loads(show(rev, path))
            except Exception:
                continue
            for sec in ("dependencies", "devDependencies", "optionalDependencies"):
                out |= {(f"{k} ({path})", str(v)) for k, v in (d.get(sec) or {}).items()}
    return out
a, b = sys.argv[1], sys.argv[2]
for kind, f in (("cargo", cargo), ("npm", npm)):
    old = dict(f(a)); new = f(b)
    for name, ver in sorted(new):
        if name not in old: print(f"{kind} NEW     {name} {ver}")
        elif old[name] != ver and (name, old[name]) not in new: print(f"{kind} CHANGED {name} {old[name]} -> {ver}")
PY
)
if [[ -n "$dep" ]]; then printf '%s\n' "$dep"; hits=$((hits + $(printf '%s\n' "$dep" | grep -c NEW || true))); else echo "(none)"; fi

echo
if (( hits )); then
  echo "verdict=sync_audit_review hits=$hits — every hit must be read and judged before merging (see fork/SYNC.md)"
  exit 1
fi
echo "verdict=sync_audit_clean hits=0"
