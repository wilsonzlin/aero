import { expect, test } from '@playwright/test';

import { run as runMemoryStability } from '../../bench/scenarios/memory_stability';

test('memory_stability (informational)', async ({ page }, testInfo) => {
  const baseUrl = testInfo.project.use.baseURL ?? 'http://127.0.0.1:5173';
  const result = await runMemoryStability(page, `${baseUrl}/`);

  await testInfo.attach('memory_stability.json', {
    body: Buffer.from(JSON.stringify(result, null, 2)),
    contentType: 'application/json',
  });

  expect(result.scenario).toBe('memory_stability');
  expect(result.start).not.toBeNull();
  expect(result.end).not.toBeNull();
});
