import { expect, test } from '@playwright/test';

import { canonicalSecurityHeaders } from '../../scripts/security_headers.mjs';

const PREVIEW_ORIGIN = 'http://127.0.0.1:4173';

function normalizeHeaderKey(key: string): string {
  return key.toLowerCase();
}

function normalizeCsp(value: string): string {
  return value
    .split(';')
    .map((part) => part.trim().replace(/\s+/g, ' '))
    .filter(Boolean)
    .join('; ');
}

function normalizeHeaderValue(key: string, value: string): string {
  if (normalizeHeaderKey(key) === 'content-security-policy') return normalizeCsp(value);
  return value.trim();
}

function assertHeaderSubset(
  actual: Record<string, string>,
  expected: Record<string, string>,
  context: string,
): void {
  for (const [rawKey, rawValue] of Object.entries(expected)) {
    const key = normalizeHeaderKey(rawKey);
    const expectedValue = normalizeHeaderValue(rawKey, rawValue);
    const actualValue = actual[key];
    expect(
      actualValue,
      `${context}: missing header ${rawKey} (expected ${JSON.stringify(expectedValue)})`,
    ).toBeTruthy();
    expect(normalizeHeaderValue(rawKey, actualValue), `${context}: mismatched ${rawKey}`).toBe(expectedValue);
  }
}

test('preview server sets canonical security headers on HTML/JS/worker/WASM and CSP gates eval()', async ({
  page,
}) => {
  const mainResponse = await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: 'load' });
  expect(mainResponse, 'navigation should return a response').toBeTruthy();

  await expect(page.evaluate(() => globalThis.crossOriginIsolated)).resolves.toBe(true);

  assertHeaderSubset(mainResponse!.headers(), canonicalSecurityHeaders, 'main document');

  // Ensure the CSP is present and has the key directives Aero depends on.
  const csp = mainResponse!.headers()['content-security-policy'];
  expect(csp).toBeTruthy();
  expect(csp).toContain("script-src 'self' 'wasm-unsafe-eval'");
  expect(csp).toContain("worker-src 'self' blob:");

  // JS asset: derive entrypoint from the HTML.
  const entryScriptSrc = await page.evaluate(() => {
    const script = document.querySelector<HTMLScriptElement>('script[type="module"][src]');
    return script?.getAttribute('src') ?? null;
  });
  expect(entryScriptSrc).toBeTruthy();

  const entryScriptResp = await page.request.get(new URL(entryScriptSrc!, PREVIEW_ORIGIN).toString());
  expect(entryScriptResp.ok(), 'entry JS should return 2xx').toBe(true);
  assertHeaderSubset(entryScriptResp.headers(), canonicalSecurityHeaders, 'entry JS asset');

  // Worker script: load a known module worker from `web/public/assets/` and validate
  // that the CSP is applied in a real script context (not `page.evaluate`, which
  // Playwright intentionally runs with CSP bypass enabled).
  const workerPath = '/assets/security_headers_worker.js';
  const workerResp = await page.request.get(`${PREVIEW_ORIGIN}${workerPath}`);
  expect(workerResp.ok(), 'worker script should return 2xx').toBe(true);
  assertHeaderSubset(workerResp.headers(), canonicalSecurityHeaders, 'worker script asset');

  const workerResult = await page.evaluate(async (url) => {
    return await new Promise<string>((resolve, reject) => {
      const worker = new Worker(url, { type: 'module' });
      worker.addEventListener('message', (ev) => {
        worker.terminate();
        resolve(String(ev.data));
      });
      worker.addEventListener('error', (ev) => {
        worker.terminate();
        reject(ev instanceof ErrorEvent ? ev.message : String(ev));
      });
    });
  }, workerPath);
  expect(workerResult).toBe('ok');

  // WASM asset: fetch a known `.wasm` file from `web/public/assets/`.
  const wasmPath = '/assets/security_headers_test.wasm';
  const wasmResp = await page.request.get(`${PREVIEW_ORIGIN}${wasmPath}`);
  expect(wasmResp.ok(), 'wasm asset should return 2xx').toBe(true);
  assertHeaderSubset(wasmResp.headers(), canonicalSecurityHeaders, 'wasm asset');
});
