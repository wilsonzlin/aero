import { expect, test, type Page } from '@playwright/test';

import { isWebGPURequired } from '../util/env';

type SampleResult = {
  backend: string;
  width: number;
  height: number;
  topLeft: number[];
  topRight: number[];
  bottomLeft: number[];
  bottomRight: number[];
  pass?: boolean;
  error?: string;
};

async function waitForReady(page: Page) {
  await page.waitForFunction(() => (window as any).__aeroTest?.ready === true);
}

async function getSamples(page: Page): Promise<SampleResult> {
  await waitForReady(page);
  return await page.evaluate(async () => {
    const api = (window as any).__aeroTest;
    if (!api?.samplePixels) {
      throw new Error(`__aeroTest missing samplePixels; error=${api?.error ?? 'none'}`);
    }
    const samples = await api.samplePixels();
    return { ...samples, pass: api.pass, error: api.error };
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

test('GPU worker: WebGPU path renders expected pattern when available @webgpu', async ({ page, browserName }) => {
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

  // Force the smoke page to request the WebGPU backend. The default page
  // behavior prefers WebGL2 to keep non-WebGPU CI runs deterministic.
  await page.goto('/web/gpu-worker-smoke.html?backend=webgpu', { waitUntil: 'load' });
  const samples = await getSamples(page);

  const statusText = await page.evaluate(() => document.getElementById('status')?.textContent ?? '');
  const statusPreview = statusText.length > 500 ? `${statusText.slice(0, 500)}…` : statusText;

  if (samples.backend !== 'webgpu') {
    const message = `GPU worker did not use WebGPU backend (got=${samples.backend}); status=${statusPreview}`;
    if (isWebGPURequired()) {
      throw new Error(message);
    }
    test.skip(true, message);
  }
  if (samples.width < 16 || samples.height < 16) {
    const message = `GPU worker WebGPU screenshot too small (${samples.width}x${samples.height}); status=${statusPreview}`;
    if (isWebGPURequired()) {
      throw new Error(message);
    }
    test.skip(true, message);
  }
  expectPattern(samples);
});
