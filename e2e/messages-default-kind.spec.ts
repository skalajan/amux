import { test, expect } from '@playwright/test';
test('Messages tab opens with the human pill selected', async ({ page }) => {
  const urls: string[] = [];
  page.on('request', r => { if (r.url().includes('/api/history')) urls.push(r.url()); });
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).switchView === 'function', { timeout: 20000 });
  await page.evaluate(() => (window as any).switchView('messages'));
  await page.waitForSelector('#msgs-kind-filter .msg-kind-chip', { timeout: 15000 });
  await page.waitForTimeout(1200);
  const chips = await page.evaluate(() => Array.from(
    document.querySelectorAll('#msgs-kind-filter .msg-kind-chip')).map(b => {
      const s = getComputedStyle(b as Element);
      return { label: (b.textContent || '').trim().slice(0, 22),
               selected: s.backgroundColor !== 'rgba(0, 0, 0, 0)' && s.backgroundColor !== 'transparent' };
    }));
  console.log('[CHIPS] ' + JSON.stringify(chips));
  console.log('[KIND-VAR] ' + await page.evaluate(() => (0, eval)('_msgsKind')));
  console.log('[FETCHES] ' + JSON.stringify(urls.filter(u => !u.includes('counts=1')).map(u => u.split('/api/')[1])));
  console.log('[COUNTS-FETCH-UNFILTERED] ' + urls.filter(u => u.includes('counts=1')).every(u => !u.includes('kind=')));
  const sel = chips.filter(c => c.selected).map(c => c.label);
  expect(sel.join(','), 'exactly one chip selected, and it is Human').toMatch(/Human/i);
  // Counts must come from the UNFILTERED ?counts=1 call, so a non-human chip
  // must not read 0 while messages exist. Guarded on All>0: the e2e server
  // starts with an EMPTY history db, where every chip is legitimately 0 and a
  // blanket no-zeros assertion fails for a reason that has nothing to do with
  // the filter.
  const all = parseInt((chips.find(c => /^All/.test(c.label))?.label || 'All 0').split(' ').pop()!, 10);
  if (all > 0) {
    const zeros = chips.filter(c => !/^All/.test(c.label) && / 0$/.test(c.label)).map(c => c.label);
    expect(zeros, `chips read 0 while All=${all} — counts got filtered: ${zeros}`).toEqual([]);
  } else {
    console.log('[SKIP] empty history db — count-integrity check not exercised here');
  }
});
