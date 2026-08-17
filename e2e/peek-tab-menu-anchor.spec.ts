import { test, expect, devices } from '@playwright/test';
// Ethan's screenshot is a WIDE viewport (menu was right-pinned, button far left),
// so assert the anchor on a tablet-width viewport too, not just the phone.
for (const vp of [{name:'phone', w:375, h:812}, {name:'tablet', w:834, h:1112}]) {
  test(`peek customizer anchors under its button @${vp.name}`, async ({ browser }) => {
    const ctx = await browser.newContext({ ...devices['iPhone 11 Pro'],
      viewport: { width: vp.w, height: vp.h }, serviceWorkers: 'block' });
    const page = await ctx.newPage();
    await page.goto('/');
    await page.waitForFunction(() => typeof (window as any).openPeek === 'function', { timeout: 20000 });
    await page.evaluate(() => (window as any).openPeek('e2e-probe'));
    await page.waitForSelector('#peek-overlay', { state: 'visible', timeout: 15000 });
    await page.locator('#peek-tab-customize').click();
    await page.waitForTimeout(400);
    const r = await page.evaluate(() => {
      const m = document.getElementById('peek-tab-customizer-menu')!;
      const b = m.getBoundingClientRect();
      const btn = document.getElementById('peek-tab-customize')!.getBoundingClientRect();
      return { mLeft: Math.round(b.left), mRight: Math.round(b.right), w: Math.round(b.width),
               btnLeft: Math.round(btn.left), vw: innerWidth,
               gap: Math.round(b.left - btn.left) };
    });
    console.log(`[${vp.name}] ` + JSON.stringify(r));
    expect(r.mRight, 'right edge off screen').toBeLessThanOrEqual(r.vw);
    expect(r.mLeft, 'left edge off screen').toBeGreaterThanOrEqual(0);
    // The real invariant: LEFT-ANCHORED TO THE BUTTON WHEN THERE IS ROOM, and
    // otherwise pushed only as far left as needed to stay on screen. Asserting
    // "left == button.left" unconditionally is wrong — the button sits at the END
    // of the tab strip, so on a phone a 359px menu under a button at x=321 would
    // end at 680 on a 375px viewport. Clamping is the feature, not a miss.
    const ideal = Math.min(r.btnLeft, r.vw - r.w - 8);
    const expected = Math.max(8, ideal);
    expect(r.mLeft, `menu left ${r.mLeft} != expected ${expected} (btn ${r.btnLeft}, w ${r.w}, vw ${r.vw})`).toBe(expected);
    await ctx.close();
  });
}
