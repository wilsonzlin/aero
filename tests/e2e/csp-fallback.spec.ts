import { expect, test } from '@playwright/test';

test('strict CSP disables dynamic wasm compilation, app falls back and still runs', async ({ page }) => {
  await page.goto('http://127.0.0.1:4180/csp/strict/?bench=1', { waitUntil: 'load' });

  await page.waitForFunction(() => (window as any).__aeroWasmJitCspPoc?.ready === true);

  const state = await page.evaluate(() => (window as any).__aeroWasmJitCspPoc);

  expect(state.capabilities.cross_origin_isolated).toBe(true);
  expect(state.capabilities.jit_dynamic_wasm).toBe(false);
  expect(state.execution.result).toBe(84);
  expect(state.execution.selected_tier).toBe('js-interpreter');
});

test('wasm-unsafe-eval enables dynamic wasm compilation on engines that implement it', async ({ page }, testInfo) => {
  await page.goto('http://127.0.0.1:4180/csp/wasm-unsafe-eval/?bench=1', { waitUntil: 'load' });
  await page.waitForFunction(() => (window as any).__aeroWasmJitCspPoc?.ready === true);

  const state = await page.evaluate(() => (window as any).__aeroWasmJitCspPoc);

  expect(state.capabilities.cross_origin_isolated).toBe(true);
  expect(state.execution.result).toBe(84);

  // Browser support differs (notably WebKit/Safari). Assert the matrix we currently observe.
  if (testInfo.project.name === 'chromium' || testInfo.project.name === 'firefox' || testInfo.project.name === 'webkit') {
    expect(state.capabilities.jit_dynamic_wasm).toBe(true);
    expect(state.execution.selected_tier).toBe('dynamic-wasm');
  }
});
