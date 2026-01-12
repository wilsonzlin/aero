import { expect, test, type Page } from '@playwright/test';

import { isWebGPURequired } from './util/env';

type SampleResult = {
  backend: string;
  width: number;
  height: number;
  topLeft: number[];
  topRight: number[];
  bottomLeft: number[];
  bottomRight: number[];
};

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

async function getSamples(page: Page): Promise<SampleResult> {
  await waitForReady(page);
  return await page.evaluate(async () => {
    if (!(window as any).__aeroTest?.samplePixels) {
      throw new Error(`__aeroTest missing samplePixels; error=${(window as any).__aeroTest?.error ?? 'none'}`);
    }
    return await (window as any).__aeroTest.samplePixels();
  });
}

function expectPattern(samples: SampleResult) {
  expect(samples.width).toBeGreaterThanOrEqual(16);
  expect(samples.height).toBeGreaterThanOrEqual(16);

  expect(samples.topLeft).toEqual([255, 0, 0, 255]);
  expect(samples.topRight).toEqual([0, 255, 0, 255]);
  expect(samples.bottomLeft).toEqual([0, 0, 255, 255]);
  expect(samples.bottomRight).toEqual([255, 255, 255, 255]);
}
test('Chromium: WebGPU path renders expected pattern when available @webgpu', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium');

  await page.goto('/', { waitUntil: 'load' });
  const hasWebGpuAdapter = await page.evaluate(async () => {
    const gpu = (navigator as any).gpu as any;
    if (!gpu) return false;
    try {
      const adapter = await gpu.requestAdapter();
      return !!adapter;
    } catch {
      return false;
    }
  });
  if (!hasWebGpuAdapter) {
    if (isWebGPURequired()) {
      throw new Error('WebGPU adapter unavailable in this Chromium environment');
    }
    test.skip(true, 'WebGPU adapter unavailable in this Chromium environment');
  }

  await page.goto('/web/gpu-smoke.html?backend=webgpu', { waitUntil: 'load' });

  await waitForReady(page);
  const initState = await page.evaluate(() => (window as any).__aeroTest);
  if (initState?.error) {
    if (isWebGPURequired()) {
      throw new Error(`WebGPU init failed: ${String(initState.error)}`);
    }
    test.skip(true, `WebGPU init failed: ${String(initState.error)}`);
  }

  const samples = await getSamples(page);
  expect(samples.backend).toBe('webgpu');
  expectPattern(samples);
});

test('WebGL2: forced backend renders expected pattern', async ({ page }) => {
  await page.goto('/web/gpu-smoke.html?backend=webgl2', { waitUntil: 'load' });
  const samples = await getSamples(page);
  expect(samples.backend).toBe('webgl2');
  expectPattern(samples);
});

test('WebKit/Firefox: auto backend selection falls back to WebGL2 when WebGPU is unavailable', async ({
  page,
  browserName,
}) => {
  test.skip(browserName === 'chromium');

  await page.goto('/web/gpu-smoke.html?backend=auto', { waitUntil: 'load' });
  const samples = await getSamples(page);
  expect(samples.backend).toBe('webgl2');
  expectPattern(samples);
});
