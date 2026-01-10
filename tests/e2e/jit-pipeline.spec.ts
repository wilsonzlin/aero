import { expect, test } from '@playwright/test';

test('Tier-1 JIT pipeline compiles, installs, and executes a block', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium', 'Smoke test currently targets chromium WASM threads support');

  await page.goto('http://127.0.0.1:4173/', { waitUntil: 'load' });

  await page.waitForFunction(() => {
    return (window as any).__jit_smoke_result !== undefined;
  });

  const result = await page.evaluate(() => (window as any).__jit_smoke_result);
  expect(result).toBeTruthy();
  expect(result.type).toBe('CpuWorkerResult');
  if (result.type !== 'CpuWorkerResult') {
    throw new Error(`JIT smoke test failed: ${result.reason ?? 'unknown error'}`);
  }
  expect(result.jit_executions).toBeGreaterThan(0);
  expect(result.helper_executions).toBeGreaterThan(0);
  expect(result.interp_executions).toBeGreaterThan(0);
  expect(result.installed_table_index).not.toBeNull();
});
