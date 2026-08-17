// UX task-series differential (Ethan's method): perform the same tasks in
// the PYTHON dashboard (the oracle, https://localhost:8822) and the RUST
// dashboard (https://localhost:8824), notate what each shows, report gaps.
// Both serve the SAME live DB, so every data divergence is a server gap,
// not a data difference.
//
// Run: node e2e/parity-tasks.mjs   (from repo root; both servers must be up)
// Writes: docs/rust-migration/ux-parity-report.md
//
// Probe-artifact rules learned 2026-08-09 (ethos #7 — a probe must not
// manufacture divergence):
//   - JSON objects are compared with SORTED keys (step C flagged identical
//     tag buckets because insertion order differed).
//   - Field-set steps (B, E) compare FULL sorted key sets; a RUST-ONLY
//     extra field is parity (additive), a PYTHON-ONLY missing field is the
//     gap. The old slice(0,12) truncated the two sides differently.
//   - Rendered-number steps read VISIBLE elements only, after rendering
//     the view a user would actually be looking at. The old run read the
//     HIDDEN board chips (painted before `sessions` loaded, repainted only
//     when the board tab opens) and the never-shown rate-limit pill.
import { chromium } from 'playwright';
import * as fs from 'fs';

const SERVERS = [
  { name: 'python', base: 'https://localhost:8822' },
  { name: 'rust', base: 'https://localhost:8824' },
];
// Fork-local calibration (skalajan/amux, 2026-08-17): upstream's probe names
// ('backend', 'gtm-engine', 'social-media', 'mixpeek-general') do not exist on
// this fleet, so every probe resolved to null and step A compared nothing on
// both sides — identical nulls read as PARITY. A probe that cannot observe the
// thing it names is theatre (ethos rule 7), so these are OUR live session names.
const PROBE_SESSIONS = ['amux-helper', 'p1', 's1', 's3', 'seventyy-ci'];
const ts = Date.now();
const PARITY_TITLE = `PARITY-${ts}`;
const results = {}; // step -> { python: fact, rust: fact }

function note(step, server, fact) {
  results[step] = results[step] || {};
  results[step][server] = fact;
}

// Key-sorted stringify so object insertion order can never read as a gap.
function stableStringify(v) {
  return JSON.stringify(v, (_, val) =>
    val && typeof val === 'object' && !Array.isArray(val)
      ? Object.fromEntries(Object.entries(val).sort(([a], [b]) => (a < b ? -1 : 1)))
      : val,
  );
}

async function runSeries(server) {
  const browser = await chromium.launch();
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true, colorScheme: 'dark' });
  const page = await ctx.newPage();
  const created = []; // board ids to archive in cleanup
  try {
    await page.goto(server.base + '/', { waitUntil: 'domcontentloaded', timeout: 30000 });
    // Dismiss first-run overlays (idioms from golden.spec.ts).
    await page.waitForTimeout(2500);
    for (const sel of ['.wt-skip', '#sw-fail-bar button', '#sw-fail-bar [onclick*="remove"]']) {
      await page.locator(sel).first().click({ timeout: 1000 }).catch(() => {});
    }
    const token = await page.evaluate(() => window._AMUX_AUTH_TOKEN);

    // A. Session list facts — including AMUX-2588 (preview_lines must be an
    // ARRAY the SPA can .map()) and AMUX-2589 (status vocabulary; the
    // lightning button expands running sessions whose status !== 'idle').
    await page.waitForFunction(
      "typeof sessions !== 'undefined' && Array.isArray(sessions) && sessions.length > 10",
      { timeout: 20000 },
    );
    const sessFacts = await page.evaluate((names) => {
      const s = sessions;
      const shape = (v) =>
        Array.isArray(v)
          ? v.every((x) => typeof x === 'string')
            ? 'array-of-strings'
            : 'array-mixed'
          : typeof v;
      const pick = (n) => {
        const x = s.find((y) => y.name === n);
        return x
          ? {
              status: x.status ?? null,
              hasPreview: !!x.preview,
              // Shape, not content: the two series run minutes apart against
              // LIVE panes, so exact preview text/lengths legitimately move.
              previewLinesShape: shape(x.preview_lines),
              tags: (x.tags || []).join(','),
              flags: !!x.flags,
              taskName: x.task_name || '',
              archived: !!x.archived,
            }
          : 'ABSENT';
      };
      return {
        total: s.length,
        running: s.filter((x) => x.running).length,
        archivedCount: s.filter((x) => x.archived).length,
        previewLinesShapes: [...new Set(s.map((x) => shape(x.preview_lines)))].sort(),
        // Compared as SUBSET (rust ⊆ python): the Rust derivation cannot
        // reproduce Python's ""-for-running cell (pane regex), but it must
        // never invent a status Python does not emit.
        runningStatusSet: [...new Set(s.filter((x) => x.running).map((x) => x.status ?? ''))].sort(),
        // steering is a LIST on every card (py:20373). Entry keys are
        // compared only when BOTH series happened to catch a queued entry —
        // the queue drains between the two runs, so presence itself races.
        steeringAllArrays: s.every((x) => Array.isArray(x.steering)),
        steeringEntryKeys: (() => {
          const withQ = s.find((x) => Array.isArray(x.steering) && x.steering.length);
          return withQ ? Object.keys(withQ.steering[0]).sort() : null;
        })(),
        probes: Object.fromEntries(names.map((n) => [n, pick(n)])),
      };
    }, PROBE_SESSIONS);
    note('A.session-list', server.name, sessFacts);

    // B. Board in-memory + rendered columns. `fields` compared as a full
    // sorted key set (rust-extra = parity, python-only = gap).
    await page.waitForFunction(
      // Fork-local calibration: upstream waited for >50 because their board
      // carried 1000+ cards. This fork's board has 7, so >50 could never be
      // satisfied and the whole harness died here on a timeout before running
      // a single comparison. Waiting for >0 keeps the "board actually loaded"
      // guarantee without encoding someone else's data volume.
      "typeof boardItems !== 'undefined' && Array.isArray(boardItems) && boardItems.length > 0",
      { timeout: 20000 },
    );
    const boardFacts = await page.evaluate(() => {
      const byStatus = {};
      for (const it of boardItems) byStatus[it.status] = (byStatus[it.status] || 0) + 1;
      const sample = boardItems.find((i) => i.status === 'todo') || boardItems[0];
      return {
        total: boardItems.length,
        byStatus,
        fields: sample ? Object.keys(sample).sort() : [],
      };
    });
    note('B.board-data', server.name, boardFacts);

    // C. Groups derivation (tag buckets the SPA computes). Sorted in the
    // stable stringify at diff time.
    const groupFacts = await page.evaluate(() => {
      const buckets = {};
      for (const s of sessions) for (const t of s.tags || []) buckets[t] = (buckets[t] || 0) + 1;
      return buckets;
    });
    note('C.groups', server.name, groupFacts);

    // D. Board write flow through the API the UI uses (same wire path):
    // create -> edit desc -> todo->doing -> archive (cleanup).
    const api = async (method, path, body) => {
      return await page.evaluate(
        async ({ method, path, body, token }) => {
          const r = await fetch(path, {
            method,
            headers: {
              'Content-Type': 'application/json',
              Authorization: 'Bearer ' + token,
              'X-Amux-Session': 'parity-harness',
            },
            body: body ? JSON.stringify(body) : undefined,
          });
          let j = null;
          try { j = await r.json(); } catch {}
          return { status: r.status, body: j };
        },
        { method, path, body, token },
      );
    };
    const createRes = await api('POST', '/api/board', {
      title: `${PARITY_TITLE}-${server.name}`,
      status: 'todo',
      desc: 'parity harness artifact',
    });
    const cardId = createRes.body?.id;
    if (cardId) created.push(cardId);
    const editRes = cardId
      ? await api('PATCH', `/api/board/${cardId}`, { desc: 'edited by parity harness' })
      : { status: 0 };
    const moveRes = cardId
      ? await api('PATCH', `/api/board/${cardId}`, { status: 'doing' })
      : { status: 0 };
    note('D.board-write-flow', server.name, {
      create: createRes.status,
      createdId: cardId ? 'yes' : 'NO',
      edit: editRes.status,
      move: moveRes.status,
    });

    // E. Schedules tab data. Full sorted key set (see B).
    const schedRes = await api('GET', '/api/schedules');
    const schedArr = Array.isArray(schedRes.body) ? schedRes.body : schedRes.body?.items || [];
    note('E.schedules', server.name, {
      status: schedRes.status,
      count: schedArr.length,
      enabled: schedArr.filter((s) => s.enabled === 1 || s.enabled === true).length,
      fields: schedArr[0] ? Object.keys(schedArr[0]).sort() : [],
    });

    // F. Calendar events.
    const calRes = await api('GET', '/api/cal-events');
    note('F.calendar', server.name, {
      status: calRes.status,
      count: Array.isArray(calRes.body) ? calRes.body.length : 'non-array',
    });

    // G. Settings surfaces.
    const usage = await api('GET', '/api/usage');
    const prefs = await api('GET', '/api/prefs');
    note('G.settings-backends', server.name, {
      usage: usage.status,
      usageHasWindows: !!(usage.body && (usage.body.five_hour || usage.body.windows || usage.body.limits)),
      prefsCount: prefs.body ? Object.keys(prefs.body).length : 0,
    });

    // H. Session verb (the lightning path) — READ-ONLY probe: peek with tiny
    // size; asserts the verb ROUTE works, mutating nothing.
    const peek = await api('GET', `/api/sessions/backend/peek?lines=5`);
    note('H.session-verb-peek', server.name, {
      status: peek.status,
      hasOutput: !!(peek.body && (peek.body.output || peek.body.history)),
    });

    // H2. The new tab endpoints (AMUX-2586 #6): skills, slash-commands, map.
    const skills = await api('GET', '/api/skills');
    const slash = await api('GET', '/api/slash-commands');
    const mapDoc = await api('GET', '/api/map');
    const statuses = await api('GET', '/api/board/statuses');
    note('H2.tab-endpoints', server.name, {
      skills: skills.status,
      skillsCount: Array.isArray(skills.body) ? skills.body.length : 'non-array',
      skillFields: Array.isArray(skills.body) && skills.body[0] ? Object.keys(skills.body[0]).sort() : [],
      slash: slash.status,
      slashCount: Array.isArray(slash.body) ? slash.body.length : 'non-array',
      map: mapDoc.status,
      mapPins: mapDoc.body?.pins?.length ?? 'absent',
      mapHasSettings: !!mapDoc.body?.settings,
      // Kanban columns (AMUX-2596): both origins read the same statuses
      // table, so the id list must match exactly.
      statusesIds: Array.isArray(statuses.body) ? statuses.body.map((s) => s.id) : 'non-array',
    });

    // J. People/CRM is REMOVED on the Rust side (Ethan). Runs BEFORE step I
    // so the main tab bar is on screen (the session peek can cover it, which
    // would make this check vacuous). Verdict rule: rust must show NO
    // visible People/CRM tab; python's value is informational.
    const crm = await page.evaluate(() => {
      const els = [...document.querySelectorAll('[onclick*="switchView(\'crm\')"], #tab-crm')];
      const visible = els.filter((e) => e.offsetParent !== null);
      const peopleTabs = [...document.querySelectorAll('button, [role=tab]')].filter(
        (e) => e.offsetParent !== null && /(^|\s)People(\s|$)/.test((e.textContent || '').trim()),
      );
      return { visibleCrmTab: visible.length > 0 || peopleTabs.length > 0 };
    });
    note('J.crm-tab', server.name, crm);

    // I. Tab numbers the user ACTUALLY sees (Ethan: board/messages counts
    // must be accurate). Two surfaces, each read the way a user reaches it:
    //   1. the BOARD tab's view chips — open the board view first, so the
    //      chips repaint with `sessions` loaded (switchView('board') calls
    //      renderBoard(); reading the pre-paint hidden DOM was the probe
    //      artifact that reported "Unowned 677" while the computed count
    //      was 11);
    //   2. the 'backend' SESSION view's tab badges.
    // Only VISIBLE elements count — a user cannot see the others.
    try {
      await page.evaluate(() => switchView('board'));
      await page.waitForTimeout(2000);
      const boardChips = await page.evaluate(() =>
        [...document.querySelectorAll('.board-view-chip')]
          .filter((e) => e.offsetParent !== null)
          .map((e) => (e.textContent || '').trim())
          .filter((t) => /\d/.test(t) && t.length < 30),
      );
      await page.evaluate(() => {
        switchView('sessions');
        const row = [...document.querySelectorAll('[onclick]')].find((e) =>
          (e.getAttribute('onclick') || '').includes("'backend'") &&
          (e.getAttribute('onclick') || '').match(/openSession|selectSession|showSession/),
        );
        if (row) row.click();
        else if (typeof openSession === 'function') openSession('backend');
      });
      await page.waitForTimeout(2500);
      const sessionTabs = await page.evaluate(() =>
        [...document.querySelectorAll('.tab, [role=tab], .session-tab, .peek-tab, button')]
          .filter((e) => e.offsetParent !== null)
          .map((e) => (e.textContent || '').trim())
          .filter((t) => /\d/.test(t) && t.length < 30)
          .slice(0, 12),
      );
      note('I.worker-tab-numbers', server.name, { boardChips, sessionTabs });
    } catch (e) {
      note('I.worker-tab-numbers', server.name, { error: String(e).slice(0, 120) });
    }

    return { created, token };
  } finally {
    // CLEANUP: archive every PARITY- artifact this run created — and CHECK
    // the response (a fire-and-forget cleanup left PH- cards behind on the
    // live board on a previous run).
    for (const id of created) {
      const res = await page
        .evaluate(
          async ({ id }) => {
            const t = window._AMUX_AUTH_TOKEN;
            const r = await fetch(`/api/board/${id}`, {
              method: 'PATCH',
              headers: {
                'Content-Type': 'application/json',
                Authorization: 'Bearer ' + t,
                'X-Amux-Session': 'parity-harness',
              },
              body: JSON.stringify({ archived: 1 }),
            });
            return r.status;
          },
          { id },
        )
        .catch((e) => String(e));
      if (res !== 200) {
        console.error(`CLEANUP FAILED on ${server.name}: card ${id} archive -> ${res} (archive it by hand)`);
      }
    }
    await browser.close();
  }
}

for (const server of SERVERS) {
  console.log(`== running series on ${server.name} (${server.base})`);
  await runSeries(server);
}

// ---- diff + report --------------------------------------------------------
// Per-step comparators. Default: stable (key-sorted) JSON equality.
// Field-set steps: python-only missing fields are gaps, rust-only extras are
// parity. Step J: rust must hide the CRM tab regardless of python.
function fieldSetVerdict(p, r, fieldKeys) {
  const strip = (o) => {
    const c = { ...o };
    for (const k of fieldKeys) delete c[k];
    return c;
  };
  if (stableStringify(strip(p)) !== stableStringify(strip(r))) return { same: false, why: 'facts differ' };
  for (const k of fieldKeys) {
    const missing = (p[k] || []).filter((f) => !(r[k] || []).includes(f));
    if (missing.length) return { same: false, why: `rust missing fields: ${missing.join(',')}` };
    const extra = (r[k] || []).filter((f) => !(p[k] || []).includes(f));
    if (extra.length) return { same: true, why: `rust-only extras (parity): ${extra.join(',')}` };
  }
  return { same: true, why: '' };
}

const COMPARATORS = {
  'A.session-list': (p, r) => {
    const strip = (o) => {
      const c = { ...o };
      delete c.runningStatusSet;
      delete c.steeringEntryKeys;
      return c;
    };
    if (stableStringify(strip(p)) !== stableStringify(strip(r))) return { same: false, why: 'facts differ' };
    const invented = (r.runningStatusSet || []).filter((s) => !(p.runningStatusSet || []).includes(s));
    if (invented.length) return { same: false, why: `rust invents statuses: ${invented.join(',')}` };
    // Entry keys compared only when both series sampled a queued entry.
    if (p.steeringEntryKeys && r.steeringEntryKeys &&
        stableStringify(p.steeringEntryKeys) !== stableStringify(r.steeringEntryKeys)) {
      return { same: false, why: `steering entry keys differ: py=${p.steeringEntryKeys} rs=${r.steeringEntryKeys}` };
    }
    return { same: true, why: '' };
  },
  'B.board-data': (p, r) => fieldSetVerdict(p, r, ['fields']),
  'E.schedules': (p, r) => fieldSetVerdict(p, r, ['fields']),
  'H2.tab-endpoints': (p, r) => fieldSetVerdict(p, r, ['skillFields']),
  'J.crm-tab': (p, r) =>
    r && r.visibleCrmTab === false
      ? { same: true, why: `python shows it: ${p?.visibleCrmTab} (intended difference)` }
      : { same: false, why: 'rust still shows a People/CRM tab' },
};

let md = `# UX Task-Series Parity Report\n\nGenerated ${new Date(ts).toISOString()} by e2e/parity-tasks.mjs.\nBoth servers serve the SAME live DB — every divergence is a server gap.\n\n| Step | Python (oracle) | Rust | Verdict |\n|---|---|---|---|\n`;
let gaps = 0;
for (const [step, r] of Object.entries(results)) {
  const cmp = COMPARATORS[step] || ((a, b) => ({ same: stableStringify(a) === stableStringify(b), why: '' }));
  const { same, why } = cmp(r.python, r.rust);
  const p = stableStringify(r.python);
  const q = stableStringify(r.rust);
  if (!same) gaps++;
  const verdict = same ? `PARITY${why ? ` — ${why}` : ''}` : `DIVERGES${why ? ` — ${why}` : ''}`;
  md += `| ${step} | \`${p?.slice(0, 160)}\` | \`${q?.slice(0, 160)}\` | ${verdict} |\n`;
  console.log(`${same ? 'PARITY  ' : 'DIVERGES'} ${step}${why ? ` — ${why}` : ''}`);
  if (!same) {
    console.log(`  py: ${p?.slice(0, 220)}`);
    console.log(`  rs: ${q?.slice(0, 220)}`);
  }
}
md += `\n${gaps === 0 ? 'FULL PARITY across the task series.' : `${gaps} step(s) diverge — see rows above.`}\n`;
fs.writeFileSync('docs/rust-migration/ux-parity-report.md', md);
console.log(`\nreport: docs/rust-migration/ux-parity-report.md — ${gaps} divergent step(s)`);
process.exit(0);
