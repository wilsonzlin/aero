import { expect, test, type Page } from '@playwright/test';

async function assertCrossOriginIsolated(page: Page) {
  const result = await page.evaluate(() => {
    let sharedWasmMemory = false;
    let sharedWasmError: string | null = null;
    try {
      const mem = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
      sharedWasmMemory = mem.buffer instanceof SharedArrayBuffer;
    } catch (err) {
      sharedWasmError = err instanceof Error ? err.message : String(err);
    }

    return {
      crossOriginIsolated: globalThis.crossOriginIsolated,
      sharedArrayBuffer: typeof SharedArrayBuffer !== 'undefined',
      atomics: typeof Atomics !== 'undefined',
      sharedWasmMemory,
      sharedWasmError,
    };
  });

  expect(result.crossOriginIsolated).toBe(true);
  expect(result.sharedArrayBuffer).toBe(true);
  expect(result.atomics).toBe(true);
  expect(result.sharedWasmMemory).toBe(true);
}

test('dev server is cross-origin isolated (COOP/COEP)', async ({ page }) => {
  await page.goto('http://127.0.0.1:5173/', { waitUntil: 'load' });
  await assertCrossOriginIsolated(page);
});

test('preview server is cross-origin isolated (COOP/COEP)', async ({ page }) => {
  await page.goto('http://127.0.0.1:4173/', { waitUntil: 'load' });
  await assertCrossOriginIsolated(page);
});
