import { expect, test, type Page } from '@playwright/test';

import {
  baselineSecurityHeaders,
  crossOriginIsolationHeaders,
  cspHeaders,
} from '../../scripts/security_headers.mjs';

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? 'http://127.0.0.1:4173';

function assertResponseHeadersContain(actual: Record<string, string>, expected: Record<string, string>): void {
  for (const [key, value] of Object.entries(expected)) {
    expect(actual[key.toLowerCase()]).toBe(value);
  }
}

async function assertCrossOriginIsolated(page: Page) {
  const result = await page.evaluate(() => {
    let sharedWasmMemory = false;
    let sharedWasmError: string | null = null;
    try {
      const mem = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
      sharedWasmMemory = mem.buffer instanceof SharedArrayBuffer;
    } catch (err) {
      const msg = err instanceof Error ? err.message : err;
      sharedWasmError = String(msg ?? "Error")
        .replace(/[\x00-\x1F\x7F]/g, " ")
        .replace(/\s+/g, " ")
        .trim()
        .slice(0, 512);
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
  const resp = await page.goto('/', { waitUntil: 'load' });
  expect(resp).toBeTruthy();
  assertResponseHeadersContain(resp!.headers(), crossOriginIsolationHeaders);
  assertResponseHeadersContain(resp!.headers(), baselineSecurityHeaders);
  await assertCrossOriginIsolated(page);
});

test('preview server is cross-origin isolated (COOP/COEP)', async ({ page }) => {
  const resp = await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: 'load' });
  expect(resp).toBeTruthy();
  assertResponseHeadersContain(resp!.headers(), crossOriginIsolationHeaders);
  assertResponseHeadersContain(resp!.headers(), baselineSecurityHeaders);
  assertResponseHeadersContain(resp!.headers(), cspHeaders);
  await assertCrossOriginIsolated(page);
});
