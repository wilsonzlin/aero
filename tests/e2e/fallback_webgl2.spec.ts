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
  expect(samples.width).toBe(64);
  expect(samples.height).toBe(64);

  expect(samples.topLeft).toEqual([255, 0, 0, 255]);
  expect(samples.topRight).toEqual([0, 255, 0, 255]);
  expect(samples.bottomLeft).toEqual([0, 0, 255, 255]);
  expect(samples.bottomRight).toEqual([255, 255, 255, 255]);
}

test('Chromium: WebGPU path renders expected pattern when available', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium');

  await page.goto('http://127.0.0.1:5173/web/gpu-smoke.html?backend=webgpu', { waitUntil: 'load' });
  const hasWebGpu = await page.evaluate(() => !!(navigator as any).gpu);
  test.skip(!hasWebGpu, 'WebGPU not available in this Chromium build');

  const samples = await getSamples(page);
  expect(samples.backend).toBe('webgpu');
  expectPattern(samples);
});

test('WebKit/Firefox: WebGL2 fallback renders expected pattern', async ({ page, browserName }) => {
  test.skip(browserName === 'chromium');

  await page.goto('http://127.0.0.1:5173/web/gpu-smoke.html?backend=webgl2', { waitUntil: 'load' });
  const samples = await getSamples(page);
  expect(samples.backend).toBe('webgl2');
  expectPattern(samples);
});
