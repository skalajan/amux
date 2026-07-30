/* ── session-chat (Scope B2) ──────────────────────────────────────────────────
 * Dashboard chat client: a per-session, turn-level thread view rendered inside
 * the peek overlay as a sibling tab to Terminal / Messages. The raw terminal
 * stays in the Terminal tab; this tab shows TURNS (owner messages, session
 * replies, system events) with delivery status and a composer.
 *
 * Consumes the B1 live-update plumbing already landed in amux-server.py
 * (AMUX-LOCAL:session-chat): both the `chat` SSE branch and the polling fallback
 * funnel through window._chatPoll(), which dispatches an `amux:chat-thread`
 * CustomEvent carrying the merged thread. We merge those items by stable id (so
 * an SSE push and a poll delivering the same turn never double-render) and
 * re-render. Writes go through apiCall -> POST /api/chat, which rides the
 * existing _authHeaders fetch wrapper (X-Amux-Write-Token) — the chat tab never
 * bypasses the Scope A write gate.
 *
 * Hard deps (all defined by amux-server.py's inline script, which runs before
 * this deferred file): apiCall, esc, renderMarkdown, sessions, wakeSession,
 * _chatPoll, _chatActiveSession, _chatCursor. Upstream has no chat.js, so this
 * file is conflict-immune on merge. See docs/session-chat.md, MODIFICATIONS.md,
 * .omc/plans/chat-layer-auth.md §5.
 * ─────────────────────────────────────────────────────────────────────────── */
(function () {
  'use strict';

  var LIVE_STATUSES = { active: 1, waiting: 1, idle: 1 };
  var _items = new Map();          // id -> thread item (dedup key = stable id)
  var _shellBuilt = false;
  var _renderTimer = 0;
  // Bug-2 (poll-flood): the inline `_chatOnSSE` now only dispatches the raw,
  // fleet-wide `chat` SSE payload here. We throttle SSE-driven refetches of the
  // OPEN thread to at most one GET /api/chat per CHAT_POLL_THROTTLE_MS.
  var CHAT_POLL_THROTTLE_MS = 2000;
  var _pollTimer = 0;
  var _pollPending = false;
  // Reply-summary collapse (docs/reply-summary.md): a session bubble collapses to
  // its `summary` (server-parsed "⌁" marker or the background Haiku fill-in) when
  // present, else to a client-side truncation for long unmarked replies. Expand
  // state is per-message, in-memory only (no persistence across tab reopen).
  var CHAT_COLLAPSE_CHARS = 600;
  var _expanded = new Set();       // message ids the user has manually expanded

  function _panel() { return document.getElementById('peek-chat-panel'); }

  function _isCollapsible(item) {
    return item.role === 'session' &&
      !!(item.summary || (item.text || '').length > CHAT_COLLAPSE_CHARS);
  }

  function _sessionStatus(name) {
    var s = sessions.find(function (x) { return x && x.name === name; });
    return s ? (s.status || '') : '';
  }

  function _fmtTime(ts) {
    if (!ts) return '';
    try {
      return new Date(ts * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    } catch (e) { return ''; }
  }

  function _bodyHtml(item) {
    // Session replies are markdown; owner/system text is shown verbatim (escaped,
    // CSS white-space:pre-wrap preserves newlines) so a typed message is exact.
    if (item.role !== 'session') return esc(item.text || '');
    if (_isCollapsible(item) && !_expanded.has(item.id)) {
      // A real summary is one plain sentence (no markdown, per the marker
      // contract) — escape it verbatim. Without one, fall back to a client-side
      // truncated preview of the (still-markdown) full text.
      if (item.summary) return esc(item.summary);
      var t = (item.text || '');
      return renderMarkdown(t.slice(0, CHAT_COLLAPSE_CHARS).trim() + '…');
    }
    return renderMarkdown(item.text || '');
  }

  function _msgHtml(item) {
    var role = item.role === 'owner' ? 'owner' : (item.role === 'system' ? 'system' : 'session');
    var pending = item._pending ? ' pending' : '';
    var origin = (item.origin || '').toString();
    var metaBits = [];
    if (role !== 'system' && origin) {
      metaBits.push('<span class="chat-origin ' + esc(origin) + '">' + esc(origin) + '</span>');
    }
    if (role === 'owner') {
      var dl = item._failed ? 'failed' : (item.delivery || (item._pending ? 'pending' : ''));
      if (dl) {
        var label = dl === 'delivered' ? '✓ delivered'
                  : dl === 'pending' ? '• sending'
                  : dl === 'failed' ? '✕ failed' : dl;
        metaBits.push('<span class="chat-delivery ' + esc(dl) + '">' + esc(label) + '</span>');
      }
    }
    var t = _fmtTime(item.ts);
    if (t) metaBits.push('<span class="chat-time">' + esc(t) + '</span>');
    var meta = metaBits.length ? '<div class="chat-meta">' + metaBits.join('') + '</div>' : '';
    var expandBtn = '';
    if (_isCollapsible(item)) {
      var open = _expanded.has(item.id);
      expandBtn = '<button type="button" class="chat-expand" data-expand-id="' + esc(item.id) + '">' +
                  (open ? 'skrýt ▴' : 'zobrazit vše ▾') + '</button>';
    }
    return '<div class="chat-msg ' + role + pending + '" data-id="' + esc(item.id) + '">' +
             '<div class="chat-bubble">' + _bodyHtml(item) + '</div>' + expandBtn + meta +
           '</div>';
  }

  function _sortedItems() {
    var arr = Array.from(_items.values());
    arr.sort(function (a, b) {
      var ta = a.ts || 0, tb = b.ts || 0;
      if (ta !== tb) return ta - tb;
      var sa = (a.seq == null ? -1 : a.seq), sb = (b.seq == null ? -1 : b.seq);
      return sa - sb;
    });
    return arr;
  }

  function _render() {
    var panel = _panel();
    if (!panel) return;
    var thread = panel.querySelector('.chat-thread');
    if (!thread) return;
    var atBottom = (thread.scrollHeight - thread.scrollTop - thread.clientHeight) < 60;
    var arr = _sortedItems();
    if (!arr.length) {
      thread.innerHTML = '<div class="chat-empty">No messages yet. Say something to this session.</div>';
    } else {
      thread.innerHTML = arr.map(_msgHtml).join('');
    }
    if (atBottom) thread.scrollTop = thread.scrollHeight;
    _syncComposer();
  }

  function _scheduleRender() {
    // Coalesce bursts (SSE + poll can both deliver in the same tick) via a short
    // timer rather than requestAnimationFrame — rAF is throttled to zero when the
    // tab/panel isn't painting (backgrounded tab, headless), which would strand
    // incoming turns unrendered; a timer fires regardless of paint state.
    if (_renderTimer) return;
    _renderTimer = setTimeout(function () { _renderTimer = 0; _render(); }, 16);
  }

  function _syncComposer() {
    var panel = _panel();
    if (!panel) return;
    var name = window._chatActiveSession || '';
    var status = _sessionStatus(name);
    var live = !!LIVE_STATUSES[status];
    var composer = panel.querySelector('.chat-composer');
    var stopped = panel.querySelector('.chat-stopped');
    if (composer) composer.style.display = live ? '' : 'none';
    if (stopped) {
      stopped.style.display = live ? 'none' : '';
      var lbl = stopped.querySelector('.chat-stopped-label');
      if (lbl) lbl.textContent = status
        ? ('Session is ' + status + ' — wake it to send messages.')
        : 'Session is not running — wake it to send messages.';
    }
  }

  function _mergeThread(detail) {
    if (!detail || !Array.isArray(detail.thread)) return;
    if (detail.session && detail.session !== window._chatActiveSession) return;
    detail.thread.forEach(function (it) {
      if (!it || !it.id) return;
      // A real (server) row supersedes any optimistic placeholder with the same id.
      _items.set(it.id, it);
    });
    _scheduleRender();
  }

  function _scheduleSSEPoll() {
    // Trailing-edge throttle: fire at most one poll per CHAT_POLL_THROTTLE_MS even
    // under a sustained fleet-wide `chat` burst. On the first hit we arm a timer;
    // when it fires we poll (if anything was pending) and re-arm a cooldown, so a
    // continuous burst still refreshes every window and a quiet window stops it —
    // never a poll-per-event flood.
    _pollPending = true;
    if (_pollTimer) return;
    var fire = function () {
      if (!_pollPending) { _pollTimer = 0; return; }
      _pollPending = false;
      if (window._chatActiveSession && window._chatPoll) window._chatPoll();
      _pollTimer = setTimeout(fire, CHAT_POLL_THROTTLE_MS);
    };
    _pollTimer = setTimeout(fire, CHAT_POLL_THROTTLE_MS);
  }

  function _send() {
    var panel = _panel();
    if (!panel) return;
    var input = panel.querySelector('.chat-input');
    var name = window._chatActiveSession || '';
    if (!input || !name) return;
    var text = (input.value || '').trim();
    if (!text) return;
    var id = 'chat-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 8);
    // Optimistic echo: render immediately with the client id we also send, so the
    // authoritative row (same id) replaces this placeholder on the next poll.
    _items.set(id, {
      id: id, role: 'owner', origin: 'dashboard', text: text,
      ts: Math.floor(Date.now() / 1000), seq: null, delivery: 'pending', _pending: true
    });
    input.value = '';
    input.style.height = '';
    _render();
    apiCall('/api/chat', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ session: name, text: text, id: id, origin: 'dashboard' })
    }).then(function (r) {
      // apiCall returns null on server error / offline-queue: mark the echo failed.
      if (!r) { var it = _items.get(id); if (it) { it._failed = true; it._pending = false; _scheduleRender(); } }
    });
  }

  function _buildShell() {
    var panel = _panel();
    if (!panel) return;
    panel.innerHTML =
      '<div class="chat-wrap">' +
        '<div class="chat-thread"></div>' +
        '<div class="chat-composer">' +
          '<textarea class="chat-input" rows="1" placeholder="Message this session…" ' +
            'autocomplete="off" autocapitalize="sentences" spellcheck="true" enterkeyhint="send"></textarea>' +
          '<button type="button" class="chat-send">Send</button>' +
        '</div>' +
        '<div class="chat-stopped" style="display:none;">' +
          '<span class="chat-stopped-label"></span>' +
          '<button type="button" class="chat-wake-btn">Wake</button>' +
        '</div>' +
      '</div>';
    var input = panel.querySelector('.chat-input');
    var sendBtn = panel.querySelector('.chat-send');
    var wakeBtn = panel.querySelector('.chat-wake-btn');
    if (input) {
      input.addEventListener('input', function () {
        input.style.height = 'auto';
        input.style.height = Math.min(input.scrollHeight, 140) + 'px';
      });
      input.addEventListener('keydown', function (e) {
        // Enter sends; Shift+Enter inserts a newline (chat convention).
        if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); _send(); }
      });
    }
    if (sendBtn) sendBtn.addEventListener('click', _send);
    if (wakeBtn) wakeBtn.addEventListener('click', function () {
      var name = window._chatActiveSession || '';
      if (name) wakeSession(name);
    });
    // Delegated on the (never-replaced) thread element, not per-button: _render()
    // rebuilds innerHTML on every update, so a direct per-button listener would be
    // lost on the very next render.
    var thread = panel.querySelector('.chat-thread');
    if (thread) {
      thread.addEventListener('click', function (e) {
        var btn = e.target.closest && e.target.closest('.chat-expand');
        if (!btn) return;
        var id = btn.getAttribute('data-expand-id');
        if (_expanded.has(id)) _expanded.delete(id); else _expanded.add(id);
        _render();
      });
    }
    _shellBuilt = true;
  }

  // ── Public hooks called from amux-server.py's minimal inline footprint ──────
  function _open(session) {
    if (!session) return;
    if (!_shellBuilt) _buildShell();
    _items.clear();
    _expanded.clear();
    window._chatActiveSession = session;
    window._chatCursor = 0;               // reset the B1 cursor -> next poll is a full load
    _render();
    _chatPoll();                          // initial full load + reconnect backfill (B1)
    var panel = _panel();
    var input = panel && panel.querySelector('.chat-input');
    if (input) { try { input.focus(); } catch (e) {} }
  }

  function _close() { window._chatActiveSession = ''; }

  // The B1 plumbing dispatches this with the merged thread (SSE + polling both
  // route here); merge-by-id dedups overlapping SSE/poll deliveries.
  window.addEventListener('amux:chat-thread', function (e) { _mergeThread(e.detail); });
  // `chat` SSE events are fleet-wide. This handler owns the Bug-2 poll-flood fix:
  //   (a) ignore events whose session != the open chat session (zero polls when
  //       nothing relevant arrives),
  //   (b) trailing-edge debounce refetches to <=1 per CHAT_POLL_THROTTLE_MS,
  //   (c) a `summary` update resets the cursor so the throttled full refetch
  //       surfaces the in-place fill-in of an already-delivered row.
  window.addEventListener('amux:chat', function (e) {
    _syncComposer();
    var payload = (e && e.detail) || [];
    var open = window._chatActiveSession || '';
    if (!open || !Array.isArray(payload)) return;
    var hit = false, hasSummary = false;
    for (var i = 0; i < payload.length; i++) {
      var p = payload[i];
      if (!p || p.session !== open) continue;
      hit = true;
      if (p.kind === 'summary') hasSummary = true;
    }
    if (!hit) return;
    if (hasSummary) window._chatCursor = 0;
    _scheduleSSEPoll();
  });

  window._chatTabOpen = _open;
  window._chatTabClose = _close;
})();
