import { expect, test } from '@playwright/test';

test('VM watchdog trips on a non-yielding CPU worker without freezing the page', async ({ page }) => {
  await page.goto('http://127.0.0.1:5173/', { waitUntil: 'load' });
  await page.waitForSelector('#vm-safety-panel');

  const beforeTicks = await page.evaluate(() => window.__aeroUiTicks ?? 0);

  await page.click('#vm-start-hang');

  // UI should remain responsive while the worker is spinning.
  await page.waitForTimeout(100);
  const midTicks = await page.evaluate(() => window.__aeroUiTicks ?? 0);
  expect(midTicks).toBeGreaterThan(beforeTicks);

  // Watchdog should terminate the worker and surface a structured error.
  await page.waitForFunction(() => {
    const text = document.querySelector('#vm-error')?.textContent ?? '';
    return text.includes('WatchdogTimeout');
  });

  await page.click('#vm-reset');
  await page.click('#vm-start-coop');

  await page.waitForFunction(() => window.__aeroVm?.lastHeartbeatAt && window.__aeroVm.lastHeartbeatAt > 0);
  await page.click('#vm-pause');
  await page.waitForFunction(() => window.__aeroVm?.state === 'paused');
});

