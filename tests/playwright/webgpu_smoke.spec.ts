import { expect, test, type Page } from '@playwright/test';

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

test('GPU worker: WebGPU path renders expected pattern when available', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium');

  await page.goto('http://127.0.0.1:5173/', { waitUntil: 'load' });
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
  test.skip(!hasWebGpuAdapter, 'WebGPU adapter unavailable in this Chromium environment');

  await page.goto('http://127.0.0.1:5173/web/gpu-worker-smoke.html', { waitUntil: 'load' });
  const samples = await getSamples(page);
  expect(samples.backend).toBe('webgpu');
  expectPattern(samples);
});
