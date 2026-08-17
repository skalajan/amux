import { test, expect } from '@playwright/test';

// OPT BACK IN: this is the one spec whose subject IS the service worker, so the
// config's global `serviceWorkers: 'block'` would make every assertion here
// vacuous — the bar it checks for exists precisely because registration fails.
test.use({ serviceWorkers: 'allow' });

// PINNED TO 375px, not left to whichever project runs it. The bug and its
// control are both WIDTH-DEPENDENT: at 375 the bar's message wraps to three or
// four lines and swallows the action row, while at desktop width it is a single
// line that clears Save on its own. Run under the desktop project, the control
// stopped reproducing the bug and reported failure — a control that silently
// declines to reproduce is the "check that cannot fail" problem inverted, and
// it points at the code rather than at itself.
test.use({ viewport: { width: 375, height: 667 } });

// AMUX-2584. The sw-fail-bar is position:fixed bottom:0 at z-index 9999;
// .board-edit-overlay is z-index 600 and its Save/Cancel row is
// position:sticky bottom:0 INSIDE the box. So the bar lands on exactly the
// strip Save occupies, and at 375px it wraps to several lines and swallows the
// whole action row — the modal opens, looks correct, and Save cannot be tapped.
//
// VISIBILITY IS THE WRONG ASSERTION and is why this survived: a covered button
// is still "visible" to Playwright and still passes toBeVisible(). Occlusion is
// only observable by asking what is actually at the point a finger would land,
// so this uses elementFromPoint at the button's centre.

async function openBoardEdit(page: any) {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  // The ADD modal, not an existing card: it is the same .board-edit-overlay
  // with the same sticky .board-edit-actions row, and needs no seeded data —
  // the e2e server starts with an empty board, which silently skipped this
  // whole spec on the first run.
  await page.evaluate(() => (window as any).openBoardAdd('todo'));
  await expect(page.locator('#board-edit-overlay')).toHaveClass(/active/);
}

/** Inject the real bar markup at the real height it renders at 375px. */
async function injectFailBar(page: any, publishVar: boolean) {
  await page.evaluate((publish: boolean) => {
    const bar = document.createElement('div');
    bar.id = 'sw-fail-bar';
    bar.style.cssText = 'position:fixed;left:0;right:0;bottom:0;z-index:9999;'
      + 'background:#7a2d2d;color:#fff;font-size:0.78rem;line-height:1.45;'
      + 'padding:12px 14px;display:flex;gap:10px;align-items:flex-start;';
    bar.innerHTML = '<div style="flex:1;min-width:0;">Offline mode is OFF. This PWA was '
      + 'installed from <b>https://localhost:8824</b>, whose self-signed certificate blocks '
      + 'the service worker — so nothing can be cached, and on cellular this address is '
      + 'unreachable. Open <b>https://amux.io</b> and re-add it to your home screen.</div>';
    document.body.appendChild(bar);
    if (publish) {
      document.documentElement.style.setProperty('--sw-fail-h', bar.offsetHeight + 'px');
    }
  }, publishVar);
}

/** What is actually under the centre of the Save button? */
async function elementAtSaveCentre(page: any): Promise<string> {
  const save = page.locator('.board-edit-actions button', { hasText: /save/i }).first();
  await expect(save).toBeVisible();
  const box = await save.boundingBox();
  if (!box) throw new Error('Save has no bounding box');
  return await page.evaluate(({ x, y }: any) => {
    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    if (!el) return 'nothing';
    return el.closest('#sw-fail-bar') ? 'sw-fail-bar' : (el.tagName + ':' + (el.textContent || '').trim().slice(0, 12));
  }, { x: box.x + box.width / 2, y: box.y + box.height / 2 });
}

test('board edit Save stays tappable under the sw-fail-bar at 375px', async ({ page }) => {
  await openBoardEdit(page);
  await injectFailBar(page, true);
  await page.waitForTimeout(150); // let the layout settle after the var lands
  const hit = await elementAtSaveCentre(page);
  expect(hit, 'the sw-fail-bar must not cover Save').not.toBe('sw-fail-bar');
});

test('CONTROL: without the height variable the bar does cover Save', async ({ page }) => {
  // Proves the assertion above can fail — i.e. that it is testing occlusion and
  // not merely that a Save button exists. If this control ever stops covering
  // Save, the test above has stopped discriminating and both need re-deriving.
  await openBoardEdit(page);
  await injectFailBar(page, false);
  await page.evaluate(() => document.documentElement.style.setProperty('--sw-fail-h', '0px'));
  await page.waitForTimeout(150);
  const hit = await elementAtSaveCentre(page);
  expect(hit, 'control must reproduce the reported bug').toBe('sw-fail-bar');
});
