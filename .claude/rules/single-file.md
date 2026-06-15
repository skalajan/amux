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

## Sanctioned exception: the session chat tab

The session **chat tab** is a deliberate, approved exception — see
[`../../docs/session-chat.md`](../../docs/session-chat.md). Its feature code lives
in **referenced files** (`chat.js`, `chat.css`, `amux_chat.py`), with only a
**~20-line inline footprint** in `amux-server.py` (a `<script>`/`<link>` tag, one
tab-registry entry + container, a static-asset route, one `_route_inner` dispatch
line) confined to marked `# ── session-chat ──` fences. The Python module is wired
via dependency injection — `amux-server.py` calls `amux_chat.init(ctx)` at startup
(the hyphenated filename prevents a reverse `import`).

This exception exists because the feature is crucial, must be first-class, and the
split keeps upstream-merge surface tiny (see [`extend-via-sidecar.md`](extend-via-sidecar.md)).
It does **not** license general fragmentation: other features still follow the
sidecar-first order in `extend-via-sidecar.md`. Only `chat.*` / `amux_chat.py` and
those fences are exempt; everything else stays in the single file.
