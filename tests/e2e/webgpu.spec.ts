import { expect, test, type Page } from '@playwright/test';

import fs from 'node:fs/promises';

import { runWebGpuScenario } from '../../bench/scenarios/webgpu';

function isWebGPURequired() {
  return process.env.AERO_REQUIRE_WEBGPU === '1';
}

async function ensureNavigatorGpu(page: Page) {
  const hasNavigatorGpu = await page.evaluate(() => !!navigator.gpu);
  if (hasNavigatorGpu) return;

  const message =
    'WebGPU is unavailable: `navigator.gpu` is missing. ' +
    "If you're running WebGPU CI, ensure the `chromium-webgpu` project is used and `AERO_REQUIRE_WEBGPU=1` is set.";

  if (isWebGPURequired()) {
    throw new Error(message);
  }

  test.skip(true, message);
}

test.describe('WebGPU smoke tests @webgpu', () => {
  test.beforeEach(async ({ page }) => {
    await ensureNavigatorGpu(page);
  });

  test('can request adapter + device @webgpu', async ({ page }) => {
    const result = await page.evaluate(async () => {
      const adapter = await navigator.gpu.requestAdapter({
        powerPreference: 'high-performance',
      });

      if (!adapter) {
        return {
          ok: false as const,
          reason: '`navigator.gpu.requestAdapter()` returned null',
        };
      }

      try {
        const device = await adapter.requestDevice();
        device.destroy?.();
        return { ok: true as const };
      } catch (err) {
        return {
          ok: false as const,
          reason: `adapter.requestDevice() threw: ${String(err)}`,
        };
      }
    });

    if (!result.ok) {
      if (isWebGPURequired()) {
        throw new Error(`WebGPU is unavailable: ${result.reason}`);
      }
      test.skip(true, `WebGPU is unavailable: ${result.reason}`);
    }

    expect(result.ok).toBe(true);
  });
});

test('webgpu bench', async ({ page }, testInfo) => {
  test.skip(process.env.AERO_WEBGPU_BENCH !== '1', 'WebGPU bench is opt-in (set AERO_WEBGPU_BENCH=1 to enable).');
  test.skip(testInfo.project.name !== 'chromium', 'WebGPU bench scenario only runs on Chromium.');

  const output = await runWebGpuScenario(page);

  expect(typeof output.bench?.supported).toBe('boolean');

  const outPath = testInfo.outputPath('webgpu.json');
  await fs.writeFile(outPath, `${JSON.stringify(output, null, 2)}\n`, 'utf8');
  testInfo.attach('webgpu.json', { path: outPath, contentType: 'application/json' });
});

