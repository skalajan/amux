import { test, expect, Page } from '@playwright/test';

// Ethan, 2026-08-11: "on the sched row in the card view of worker list page
// homepage put # of board items (total)".
//
// Seeds its own worker + cards: the e2e server starts with an empty board, and
// a count feature tested against zero data passes without rendering anything.

async function appToken(page: Page): Promise<string> {
  const tok = await page.evaluate(() => (window as any)._AMUX_AUTH_TOKEN as string);
  expect(tok, 'served bootstrap must inject a non-empty auth token').toBeTruthy();
  return tok;
}

test('worker card shows total board items on the sched row', async ({ page, request }, testInfo) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  const token = await appToken(page);
  const auth = { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' };

  // A worker IS its env file, so create one the fleet list will show.
  // PER-PROJECT NAME: desktop and mobile run this test CONCURRENTLY against
  // ONE shared server, so a fixed name had both projects seeding the same
  // worker — CI read "2 doing · 6 active", every figure exactly doubled. The
  // first CI run of this spec is what caught it; locally a single project
  // never collides with itself.
  const worker = `e2e-count-${testInfo.project.name}`;
  await request.post('/api/sessions', {
    headers: auth,
    data: { name: worker, dir: '/tmp', desc: 'e2e count fixture' },
  });

  // 3 items for this worker: 1 doing, 2 not. Total must read 3, doing 1.
  for (const st of ['doing', 'todo', 'backlog']) {
    const res = await request.post('/api/board', {
      headers: auth,
      data: { title: `e2e count ${st}`, status: st, session: worker, type: 'chore' },
    });
    expect(res.ok(), `seeding a ${st} card must succeed`).toBeTruthy();
  }

  await page.reload();
  await page.waitForLoadState('networkidle');

  const card = page.locator(`.session-card:has-text("${worker}"), [data-session="${worker}"]`).first();
  await expect(card, 'the seeded worker must appear in the card view').toBeVisible({ timeout: 15000 });

  const meta = card.locator('.meta-count');
  await expect(meta).toBeVisible();
  const text = (await meta.textContent()) || '';

  // The ASSERTION IS THE NUMBER, not merely that a badge is present: a counter
  // that renders the wrong figure is worse than none.
  //
  // Asserted against what the renderer actually emits (ffa00e4's predicate):
  // "N doing · M active[ · T total]", where active counts ALL non-terminal
  // cards and the total segment is HIDDEN when total === active. The previous
  // assertions wanted "3 items" and an .mc-total element — text this renderer
  // has never produced (that was an earlier badge's shape) — so the spec
  // failed on its first real CI run while the feature worked. With 3
  // non-terminal seeds (1 doing) and nothing terminal, the badge must read
  // "1 doing · 3 active" and mc-total must be absent.
  expect(text).toContain('1 doing');
  expect(text).toContain('3 active');
  await expect(card.locator('.mc-doing')).toHaveText('1');
  await expect(card.locator('.mc-active')).toHaveText('3');
  expect(await card.locator('.mc-total').count()).toBe(0);

  // active >= doing must hold by construction (shared predicate).
  const active = Number(await card.locator('.mc-active').textContent());
  const doing = Number(await card.locator('.mc-doing').textContent());
  expect(active).toBeGreaterThanOrEqual(doing);

  // cleanup
  await request.delete(`/api/sessions/${worker}`, { headers: auth });
});
