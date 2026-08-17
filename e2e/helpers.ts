import { expect, BrowserContext, Locator } from '@playwright/test';

/**
 * Click something inside a scroll-snap container (the board kanban) reliably.
 *
 * WHY THIS EXISTS (AF-47, measured 2026-08-13). `.board-columns` is
 * `scroll-snap-type: x proximity` and each `.board-col` is `flex: 0 0 86vw`, so
 * at phone width the "done" column starts around x=1000. Playwright's click
 * scrolls the target into view and then clicks its centre — but under WebKit the
 * snap animation is STILL RUNNING when those coordinates are computed, so the
 * click lands where the button used to be. Measured: the handler was never
 * invoked (a wrapper counted 0 calls) and no request was issued at all, while
 * the same code on Chromium at 375px invoked it and got a 200.
 *
 * The damage was not the flake, it was the DIAGNOSIS it induced. The test failed
 * on "the card is still on the board", which reads as `clearDone()` being broken
 * on iOS. It cost a board card filed against the product — claiming the button
 * was unreachable at phone width — when the layout was correct by design and the
 * only broken thing was the click. Scroll first, let the snap settle, then click.
 *
 * The viewport assertion below is the point, not a nicety: it makes the NEXT
 * occurrence say "the element is still outside the viewport after scrolling"
 * instead of blaming whatever the button was supposed to do. A silent miss is
 * indistinguishable from a broken feature, which is exactly how this one was
 * misfiled the first time.
 */
export async function clickSnapped(target: Locator, label = 'element') {
  // RETRY THE WHOLE SEQUENCE, because the board re-renders underneath it.
  // renderBoard() replaces the subtree on every refresh, and while
  // locator.click() re-resolves the locator on each actionability retry,
  // scrollIntoViewIfNeeded() does NOT — it throws "Element is not attached to
  // the DOM" the moment a refresh lands between resolving and scrolling. The
  // first version of this helper fixed WebKit and broke desktop and mobile that
  // way, which is its own small lesson: adding a step before a click gives up
  // the retry semantics the click had for free.
  const deadline = Date.now() + 15_000;
  let last: unknown;
  while (Date.now() < deadline) {
    try {
      await target.scrollIntoViewIfNeeded({ timeout: 3_000 });
      // Snap animates AFTER the programmatic scroll; wait for the box to come
      // to rest inside the viewport rather than sleeping a guessed constant.
      await expect
        .poll(
          async () =>
            target.evaluate((el) => {
              const r = el.getBoundingClientRect();
              return r.left >= 0 && r.right <= window.innerWidth ? 1 : 0;
            }),
          { timeout: 3_000 },
        )
        .toBe(1);
      await target.click({ timeout: 3_000 });
      return;
    } catch (e) {
      last = e; // re-render or unsettled snap: re-resolve and try again
    }
  }
  throw new Error(
    `clickSnapped: ${label} never came to rest inside the viewport and clickable within 15s.\n` +
      `It is inside a scroll-snap container (.board-columns); either the snap is fighting the ` +
      `programmatic scroll, or the board is re-rendering faster than the click can land.\n` +
      `This is a HARNESS problem, not the feature under test — do NOT file it against the ` +
      `product. AF-47 was filed that way once: a silent miss looks exactly like a broken button.\n` +
      `Last error: ${last}`,
  );
}


/**
 * Grant clipboard write where the permission EXISTS, and nowhere else.
 *
 * WebKit has no 'clipboard-write' permission, and asking for it is a hard error
 * — `browserContext.grantPermissions: Unknown permission: clipboard-write` —
 * which fails the test before the page loads. It allows the write under a user
 * gesture instead, which a real click is.
 *
 * A helper rather than an inline `if`, because the inline version is exactly
 * what did not get carried forward: it was written once in feedback-smoke.spec
 * (AMUX-3057), then omitted from msg-id-copy.spec, which reached CI as a
 * deterministic ios-safari failure. One import is harder to forget than one
 * remembered condition, and if the engine gap ever changes there is a single
 * place to change it.
 */
export async function grantClipboard(context: BrowserContext, browserName: string) {
  if (browserName === 'webkit') return;
  await context.grantPermissions(['clipboard-write']);
}
