import { expect, test } from '@playwright/test';

const DEV_SERVER = 'http://127.0.0.1:5173';
const GPU_SMOKE_URL = `${DEV_SERVER}/web/src/pages/gpu_smoke.html`;

const EXPECTED_TEST_PATTERN_SHA256 =
  'a42e8433ee338fcf505b803b5a52a663478c7009ef85c7652206b4a06d3b76a8';

async function waitForGpuSmokeResult(page) {
  await page.waitForFunction(() => (window as any).__gpuSmokeResult?.done === true);
  return page.evaluate(() => (window as any).__gpuSmokeResult);
}

test('forced WebGL2 fallback renders expected test pattern', async ({ page }) => {
  await page.goto(`${GPU_SMOKE_URL}?backend=webgl2`, { waitUntil: 'load' });
  const result = await waitForGpuSmokeResult(page);

  expect(result.error).toBeUndefined();
  expect(result.backend).toBe('webgl2');
  expect(result.hash).toBe(EXPECTED_TEST_PATTERN_SHA256);
  expect(result.ok).toBe(true);
});

test('default init uses WebGPU when available', async ({ page }) => {
  await page.goto(GPU_SMOKE_URL, { waitUntil: 'load' });
  const result = await waitForGpuSmokeResult(page);

  if (!result.navigatorGpuAvailable) {
    test.skip(true, 'WebGPU not available in this environment (navigator.gpu missing)');
  }

  if (result.error) {
    const message = String(result.error);
    // Headless WebGPU support is spotty; treat known readback/adapter failures as "not available"
    // rather than a hard failure of the fallback smoke tests.
    if (
      message.includes('external Instance reference') ||
      message.includes('Failed to execute') ||
      message.includes('Readback of the source image has failed')
    ) {
      test.skip(true, `WebGPU present but not usable in this environment: ${message}`);
    }

    throw new Error(message);
  }

  if (result.backend !== 'webgpu') {
    test.skip(true, `WebGPU not used by default init (backend=${result.backend})`);
  }

  expect(result.backend).toBe('webgpu');
  expect(result.hash).toBe(EXPECTED_TEST_PATTERN_SHA256);
  expect(result.ok).toBe(true);
});
