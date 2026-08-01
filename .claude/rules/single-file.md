---
description: When editing amux-server.py — the single-file constraint
globs: ["amux-server.py"]
---

amux-server.py is one file containing Python server + inline HTML/CSS/JS. By
default, do not split it into multiple files or create separate modules. Always
verify syntax after edits:

```bash
python3 -c "import ast; ast.parse(open('amux-server.py').read())"
```

The PostToolUse hook validates this automatically, but if you're making batch edits, run it manually before committing.

## Implemented exception: the session chat tab

The session **chat tab** is a deliberate, approved exception — see
[`../../docs/session-chat.md`](../../docs/session-chat.md). **It is
IMPLEMENTED** (2026-07-31): `chat.js` and `chat.css` exist at repo root and
carry the tab's UI; the inline footprint in `amux-server.py` (script/link tags,
tab registry entry + container, static-asset route, dispatch) is
sentinel-wrapped in `# AMUX-LOCAL:session-chat` … `# /AMUX-LOCAL:session-chat`
comments — never the `# ── … ──` house-divider style (see
[`extend-via-sidecar.md`](extend-via-sidecar.md)'s sentinel convention) — and
registered in the MODIFICATIONS.md Local Delta Registry (`session-chat` row).
One deviation from the original design: **`amux_chat.py` was never created** —
the Python chat core (schema, `_chat_extract_turns`, `_chat_populate_replies`,
summary worker) lives inline in `amux-server.py` inside the same sentinel
fences instead of a DI module. Do not create `amux_chat.py` retroactively;
extend the sentinel-fenced blocks and keep the registry row current.

This exception exists because the feature is crucial, must be first-class, and the
split keeps upstream-merge surface tiny (see [`extend-via-sidecar.md`](extend-via-sidecar.md)).
It does **not** license general fragmentation: other features still follow the
sidecar-first order in `extend-via-sidecar.md`. Only `chat.*` / `amux_chat.py` and
their sentinel-wrapped footprint are exempt; everything else stays in the single file.
