import { test, expect } from '@playwright/test';

// BLOCK THE SERVICE WORKER (AF-46). Nothing here tests offline behaviour, but
// sw.js reloads the page on `controllerchange` (app.js:24253) as soon as a
// freshly-registered worker claims the client — which, on the clean profile
// each project now gets, happens right where these tests call page.evaluate.
// The result was "Execution context was destroyed, most likely because of a
// navigation" on a spec about CSS geometry: a red that says nothing about the
// menu it is guarding. sw-fail-bar.spec.ts owns the worker's real behaviour.
test.use({ serviceWorkers: 'block' });

// Ethan, 2026-08-13: "I still can't view the tabs when I click the box which was
// historically a drop-down list of all the tabs." The ⊞ button (#peek-tab-customize)
// opens #peek-tab-customizer-menu — the show/hide/reorder list of every worker tab.
// A bottom-sheet fix shipped the same day and he still could not see it, so this
// asserts what a USER can see (geometry on screen), never that the element exists.
//
// The customizer is static markup, so it needs a visible overlay, not a live worker —
// openPeek() is called directly rather than seeding a session the harness has none of.
async function openPeek(page) {
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).openPeek === 'function', { timeout: 20000 });
  await page.evaluate(() => (window as any).openPeek('e2e-probe'));
  await page.waitForSelector('#peek-overlay', { state: 'visible', timeout: 15000 });
}

test('the tab customizer opens and its rows are visible on screen', async ({ page }, info) => {
  await openPeek(page);
  const btn = page.locator('#peek-tab-customize');
  await expect(btn, 'the ⊞ button itself is missing').toBeVisible();
  await btn.click();

  const menu = page.locator('#peek-tab-customizer-menu');
  await expect(menu, 'menu did not become visible').toBeVisible();

  const vp = page.viewportSize()!;
  const box = await menu.boundingBox();
  expect(box, 'menu has no layout box').not.toBeNull();
  expect(box!.height, 'menu rendered with zero height').toBeGreaterThan(20);
  expect(box!.width, 'menu rendered with zero width').toBeGreaterThan(40);
  // On screen, not merely in the DOM — this is the actual complaint.
  expect(box!.y + box!.height, `menu is above the fold (y=${box!.y})`).toBeGreaterThan(0);
  expect(box!.y, `menu starts below the fold (y=${box!.y}, vh=${vp.height})`).toBeLessThan(vp.height);
  expect(box!.x + box!.width, 'menu is off-screen left').toBeGreaterThan(0);
  expect(box!.x, 'menu is off-screen right').toBeLessThan(vp.width);

  const items = menu.locator('.tab-customizer-item');
  expect(await items.count(), 'menu lists no tabs').toBeGreaterThan(3);
  const row = items.nth(1);
  await expect(row, 'a tab row is not visible').toBeVisible();
  const ib = await row.boundingBox();
  expect(ib!.height, 'a tab row has zero height').toBeGreaterThan(10);
  expect(ib!.y + ib!.height, 'first tab row is above the viewport').toBeGreaterThan(0);
  expect(ib!.y, 'first tab row is below the viewport').toBeLessThan(vp.height);

  await page.screenshot({ path: `../test-results/tabcust-${info.project.name}.png` });
});

// THE GLOBAL customizer, which the test above does NOT cover. There are three
// menus sharing the .tab-customizer-menu class (index.html:499, :1948, :2199)
// and the peek one is only the second. Ethan's words — "a drop-down list of ALL
// the tabs" — describe the main nav (Sessions/Board/Cost/.../Terminal/MCP)
// better than the per-worker one, and the mobile bottom-sheet fix was written
// against a CSS comment that names the global menu as the clipped case:
// position:absolute under a bar with overflow-x:auto. Verifying one menu and
// reporting "the tab customizer works" is how the wrong control gets cleared.
test('the GLOBAL tab customizer opens and its rows are visible on screen', async ({ page }, info) => {
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).toggleTabCustomizer === 'function',
                             { timeout: 20000 });

  const btn = page.locator('.tab-customize-wrap button[onclick*="toggleTabCustomizer"]');
  await expect(btn, 'the global ⊞ button itself is missing').toBeVisible();
  await btn.click();

  const menu = page.locator('#tab-customizer-menu');
  await expect(menu, 'global menu did not become visible').toBeVisible();

  const vp = page.viewportSize()!;
  const box = await menu.boundingBox();
  expect(box, 'global menu has no layout box').not.toBeNull();
  expect(box!.height, 'global menu rendered with zero height').toBeGreaterThan(20);
  expect(box!.width, 'global menu rendered with zero width').toBeGreaterThan(40);
  expect(box!.y + box!.height, `global menu is above the fold (y=${box!.y})`).toBeGreaterThan(0);
  expect(box!.y, `global menu starts below the fold (y=${box!.y}, vh=${vp.height})`).toBeLessThan(vp.height);
  expect(box!.x + box!.width, 'global menu is off-screen left').toBeGreaterThan(0);
  expect(box!.x, 'global menu is off-screen right').toBeLessThan(vp.width);

  // Not merely on-screen: NOT CLIPPED by an ancestor's overflow. A menu that
  // drops out of an overflow:auto bar has a correct boundingBox and is
  // invisible to the user, which is exactly the reported symptom — so the box
  // checks above cannot catch it and this one has to.
  const clipped = await menu.evaluate((el: Element) => {
    const r = el.getBoundingClientRect();
    for (let p = el.parentElement; p; p = p.parentElement) {
      const s = getComputedStyle(p);
      if (s.overflow === 'visible' && s.overflowX === 'visible' && s.overflowY === 'visible') continue;
      if (getComputedStyle(el).position === 'fixed') continue; // fixed escapes non-transformed ancestors
      const pr = p.getBoundingClientRect();
      if (r.bottom > pr.bottom + 1 || r.top < pr.top - 1)
        return `${p.tagName}.${p.className} clips it (menu ${r.top}-${r.bottom} vs parent ${pr.top}-${pr.bottom})`;
    }
    return null;
  });
  expect(clipped, `global menu is clipped by an overflow ancestor: ${clipped}`).toBeNull();

  const items = menu.locator('.tab-customizer-item');
  expect(await items.count(), 'global menu lists no tabs').toBeGreaterThan(3);
  const row = items.nth(1);
  await expect(row, 'a global tab row is not visible').toBeVisible();
  const ib = await row.boundingBox();
  expect(ib!.height, 'a global tab row has zero height').toBeGreaterThan(10);
  expect(ib!.y + ib!.height, 'first global tab row is above the viewport').toBeGreaterThan(0);
  expect(ib!.y, 'first global tab row is below the viewport').toBeLessThan(vp.height);

  await page.screenshot({ path: `../test-results/tabcust-global-${info.project.name}.png` });
});
