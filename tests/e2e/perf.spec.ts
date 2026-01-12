import { expect, test } from '@playwright/test';

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? 'http://127.0.0.1:4173';

async function assertPerfExportAvailable(url: string, page: import('@playwright/test').Page) {
  await page.goto(url, { waitUntil: 'load' });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const aero = (globalThis as any).aero;
    const perf = aero?.perf;
    return (
      perf &&
      typeof perf === 'object' &&
      typeof perf.captureStart === 'function' &&
      typeof perf.captureStop === 'function' &&
      typeof perf.export === 'function'
    );
  });

  const exported = await page.evaluate(async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const perf = (globalThis as any).aero.perf;
    if (typeof perf.captureReset === 'function') perf.captureReset();
    perf.captureStart();
    await new Promise((resolve) => setTimeout(resolve, 250));
    perf.captureStop();
    return perf.export();
  });

  expect(exported.kind).toBe('aero-perf-capture');
  expect(exported.version).toBe(2);
  expect(exported.records.length).toBeGreaterThan(0);
  expect(exported.memory).toBeTruthy();
  expect(exported.responsiveness).toBeTruthy();
  expect(exported.jit).toBeTruthy();

  const json = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return JSON.stringify((globalThis as any).aero.perf.export());
  });
  expect(json).toContain('"kind":"aero-perf-capture"');
  expect(json).toContain('"version":2');
}

test('perf export works on dev server', async ({ page }) => {
  await assertPerfExportAvailable('/', page);
});

test('perf export works on preview server', async ({ page }) => {
  await assertPerfExportAvailable(`${PREVIEW_ORIGIN}/`, page);
});
