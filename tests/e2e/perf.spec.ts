import { expect, test } from '@playwright/test';

async function assertPerfExportAvailable(url: string, page: import('@playwright/test').Page) {
  await page.goto(url, { waitUntil: 'load' });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const aero = (globalThis as any).aero;
    return aero?.perf?.getStats?.().frames > 0;
  });

  const exported = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return (globalThis as any).aero.perf.export();
  });

  expect(exported.schema_version).toBe(1);
  expect(exported.samples.frame_count).toBeGreaterThan(0);
  expect(exported.samples.frames.length).toBeGreaterThan(0);
  expect(typeof exported.samples.frames[0].counters.instructions).toBe('string');

  const json = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return JSON.stringify((globalThis as any).aero.perf.export());
  });
  expect(json).toContain('"schema_version":1');

  await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).aero.perf.setEnabled(false);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).aero.perf.setEnabled(true);
  });
}

test('perf export works on dev server', async ({ page }) => {
  await assertPerfExportAvailable('http://127.0.0.1:5173/', page);
});

test('perf export works on preview server', async ({ page }) => {
  await assertPerfExportAvailable('http://127.0.0.1:4173/', page);
});

