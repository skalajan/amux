"""Deterministic test of the localhost write-auth gate (Milestone A / Scope A).

Loads the REAL pure helpers `_is_write_path` (deny-by-default write classifier) and
`_write_auth_ok` (credential check) out of amux-server.py via AST — no server needed,
mirroring tests/test_steering_delivery.py. Drives them against a full matrix:

  - no-credential write            -> reject
  - file-token write               -> allow
  - Bearer with AUTH_TOKEN set      -> allow
  - Bearer when AUTH_TOKEN==""       -> reject   (none-mode, AMUX_AUTH_TOKEN=none)
  - file-token when AUTH_TOKEN==""   -> allow    (AC-A4: enforcement independent of AUTH_TOKEN)
  - _UI_TOKEN / derived sha256       -> reject   (AC-A1-neg: no computable constant grants write)
  - read paths                      -> not a write (token-free)
  - unknown POST /api/future-*       -> classified as write (deny-by-default, AC-A4/N5)
  - OPTIONS/GET/HEAD                 -> not writes
  - public relay/share paths         -> not writes (left loose)
"""
import ast, os, hmac, hashlib

SERVER = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      "amux-server.py")

# ── seed namespace: the globals the two helpers reference ─────────────────────
_FILE_SECRET = "REAL-random-write-secret-xyz"
_BEARER = "the-remote-auth-token"

ns = {
    "_hmac": hmac,
    "_WRITE_TOKEN": _FILE_SECRET,
    "AUTH_TOKEN": _BEARER,
    # _is_write_path reads these two (public paths are left loose):
    "_PUBLIC_PATHS": frozenset({"/", "/manifest.json", "/api/calendar.ics"}),
    "_PUBLIC_PREFIXES": ("/s/", "/api/share/", "/invite/", "/proxy/", "/api/branding/"),
}

# ── AST-load the real helpers (like test_steering_delivery.py) ────────────────
tree = ast.parse(open(SERVER, encoding="utf-8").read())
_want_func = {"_is_write_path", "_write_auth_ok"}
for node in tree.body:
    if isinstance(node, ast.FunctionDef) and node.name in _want_func:
        exec(compile(ast.Module(body=[node], type_ignores=[]), SERVER, "exec"), ns)

is_write = ns["_is_write_path"]
auth_ok = ns["_write_auth_ok"]
assert callable(is_write) and callable(auth_ok), "helpers not extracted from amux-server.py"


def _H(**kw):
    """Build a case-sensitive header dict the way http.client / handler exposes it.
    The helpers use plain dict .get(), so keys must match exactly as sent."""
    return dict(kw)


# ── _is_write_path: DENY-BY-DEFAULT classifier ────────────────────────────────
# Reads are never writes.
assert is_write("GET", "/api/sessions") is False, "GET is not a write"
assert is_write("HEAD", "/api/board") is False, "HEAD is not a write"
assert is_write("OPTIONS", "/api/sessions/x/send") is False, "OPTIONS is not a write"
# Non-/api/ paths are not writes (static assets etc.).
assert is_write("POST", "/") is False, "non-/api/ POST is not a write"
assert is_write("POST", "/proxy/foo") is False, "/proxy/ prefix POST is not an /api/ write"
# Public relay/share paths are left loose.
assert is_write("POST", "/api/share/abc") is False, "public share path left loose"
assert is_write("POST", "/api/branding/x") is False, "public branding path left loose"
assert is_write("POST", "/api/calendar.ics") is False, "public calendar path left loose"
# Known writes classify as writes.
assert is_write("POST", "/api/sessions/x/send") is True, "send is a write"
assert is_write("POST", "/api/sessions/x/steer") is True, "steer is a write"
assert is_write("DELETE", "/api/board/AMUX-1") is True, "board delete is a write"
assert is_write("PATCH", "/api/board/AMUX-1") is True, "board patch is a write"
# DENY-BY-DEFAULT: an UNKNOWN/future /api/ write route is a write on arrival (N5).
assert is_write("POST", "/api/future-endpoint-that-upstream-adds-later") is True, \
    "deny-by-default: unknown POST /api/... must classify as a write (N5)"
assert is_write("PUT", "/api/whatever") is True, "deny-by-default: PUT /api/... is a write"
print("is_write ok — deny-by-default writes, loose reads/public paths")

# ── _write_auth_ok: credential rule (AC-A3) ───────────────────────────────────
# No credential -> reject.
assert auth_ok(_H()) is False, "no-credential write must be rejected"
assert auth_ok(_H(**{"X-Amux-Write-Token": ""})) is False, "empty write token rejected"
# The real 0600 file secret -> allow.
assert auth_ok(_H(**{"X-Amux-Write-Token": _FILE_SECRET})) is True, "file-token write allowed"
# A wrong write token -> reject.
assert auth_ok(_H(**{"X-Amux-Write-Token": "not-the-secret"})) is False, "wrong write token rejected"
# Bearer with AUTH_TOKEN set -> allow; wrong Bearer -> reject.
assert auth_ok(_H(**{"Authorization": f"Bearer {_BEARER}"})) is True, "valid Bearer allowed"
assert auth_ok(_H(**{"Authorization": "Bearer wrong"})) is False, "wrong Bearer rejected"
# AC-A1-neg: no computable constant grants a write.
_ui_token = hashlib.sha256(("amux-ui-guard:" + _BEARER).encode()).hexdigest()[:40]
assert auth_ok(_H(**{"X-Amux-UI-Token": _ui_token})) is False, "_UI_TOKEN header must NOT grant write"
assert auth_ok(_H(**{"X-Amux-Write-Token": _ui_token})) is False, "_UI_TOKEN value must NOT grant write"
_derived = hashlib.sha256(("amux-write:" + _BEARER).encode()).hexdigest()
assert auth_ok(_H(**{"X-Amux-Write-Token": _derived})) is False, "derived sha256 must NOT grant write"
assert auth_ok(_H(**{"X-Amux-Session": "some-session"})) is False, "X-Amux-Session must NOT grant write"
print("write_auth_ok ok — only file secret or Bearer AUTH_TOKEN; no _UI_TOKEN/derived/session")

# ── none-mode column (AMUX_AUTH_TOKEN=none -> AUTH_TOKEN=="") ──────────────────
# Enforcement must be INDEPENDENT of AUTH_TOKEN (C1/S4). Mutate the shared global
# the helper reads at call time.
ns["AUTH_TOKEN"] = ""
# Bearer can no longer authorize anything (there is no server token to match).
assert auth_ok(_H(**{"Authorization": "Bearer "})) is False, "none-mode: empty Bearer rejected"
assert auth_ok(_H(**{"Authorization": f"Bearer {_BEARER}"})) is False, \
    "none-mode: a stale Bearer must NOT authorize when AUTH_TOKEN==''"
# The file secret STILL grants a write under none-mode (AC-A4).
assert auth_ok(_H(**{"X-Amux-Write-Token": _FILE_SECRET})) is True, \
    "none-mode: the 0600 file secret still authorizes writes (AC-A4)"
# And a tokenless write is still rejected under none-mode.
assert auth_ok(_H()) is False, "none-mode: tokenless write still rejected (AC-A4)"
ns["AUTH_TOKEN"] = _BEARER  # restore
print("none-mode ok — write enforcement independent of AUTH_TOKEN (AC-A4)")

# ── tunnel-relay reasoning (AC-A5) ────────────────────────────────────────────
# A tunnel-relay write arrives on loopback (127.0.0.1) and now hits the hoisted
# gate BEFORE the localhost bypass, so without the token it is a write -> reject.
assert is_write("POST", "/api/sessions/x/send") is True and auth_ok(_H()) is False, \
    "AC-A5: relayed tokenless write classifies as write and is rejected"
print("tunnel-relay ok — loopback-arriving write without token is rejected (AC-A5)")

print("\nALL WRITE-AUTH CHECKS PASSED")


def test_write_auth_matrix():
    """The matrix above executes at import time and raises on any regression;
    reaching here means the deny-by-default classifier and the credential rule
    (file secret OR Bearer-when-set, never _UI_TOKEN/derived, none-mode-safe) hold."""
    assert True
