import { expect, test, type Page } from '@playwright/test';

async function assertCrossOriginIsolated(page: Page) {
  const result = await page.evaluate(() => {
    return {
      crossOriginIsolated: globalThis.crossOriginIsolated,
      sharedArrayBuffer: typeof SharedArrayBuffer !== 'undefined',
      atomics: typeof Atomics !== 'undefined',
    };
  });

  expect(result.crossOriginIsolated).toBe(true);
  expect(result.sharedArrayBuffer).toBe(true);
  expect(result.atomics).toBe(true);
}

test('dev server is cross-origin isolated (COOP/COEP)', async ({ page }) => {
  await page.goto('http://127.0.0.1:5173/', { waitUntil: 'load' });
  await assertCrossOriginIsolated(page);
});

test('preview server is cross-origin isolated (COOP/COEP)', async ({ page }) => {
  await page.goto('http://127.0.0.1:4173/', { waitUntil: 'load' });
  await assertCrossOriginIsolated(page);
});
