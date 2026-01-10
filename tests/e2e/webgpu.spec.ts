import { expect, test } from '@playwright/test';

import fs from 'node:fs/promises';

import { runWebGpuScenario } from '../../bench/scenarios/webgpu';

test('webgpu', async ({ page }, testInfo) => {
  test.skip(
    process.env.AERO_WEBGPU_BENCH !== '1',
    'WebGPU bench is opt-in (set AERO_WEBGPU_BENCH=1 to enable).',
  );

  test.skip(testInfo.project.name !== 'chromium', 'WebGPU bench scenario only runs on Chromium.');

  const output = await runWebGpuScenario(page);

  expect(typeof output.bench?.supported).toBe('boolean');

  const outPath = testInfo.outputPath('webgpu.json');
  await fs.writeFile(outPath, `${JSON.stringify(output, null, 2)}\n`, 'utf8');
  testInfo.attach('webgpu.json', { path: outPath, contentType: 'application/json' });
});
