import { expect, test } from '@playwright/test';

test('VM watchdog trips on a non-yielding CPU worker without freezing the page', async ({ page }) => {
  await page.goto('/', { waitUntil: 'load' });
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

  const beforeInstructions = await page.evaluate(() => (window.__aeroVm?.lastHeartbeat as any)?.totalInstructions ?? 0);
  await page.click('#vm-step');
  await page.waitForFunction((prev) => ((window as any).__aeroVm?.lastHeartbeat?.totalInstructions ?? 0) > prev, beforeInstructions);
  await page.waitForFunction(() => window.__aeroVm?.state === 'paused');
});

test('VM reports resource limit errors and can reset without reload', async ({ page }) => {
  await page.goto('/', { waitUntil: 'load' });
  await page.waitForSelector('#vm-safety-panel');

  await page.fill('#vm-guest-mib', '64');
  await page.fill('#vm-max-guest-mib', '32');

  await page.click('#vm-start-coop');

  await page.waitForFunction(() => {
    const text = document.querySelector('#vm-error')?.textContent ?? '';
    return text.includes('ResourceLimitExceeded') || text.includes('guest RAM request');
  });

  await page.click('#vm-reset');
  await page.fill('#vm-max-guest-mib', '512');
  await page.click('#vm-start-coop');
  await page.waitForFunction(() => window.__aeroVm?.lastHeartbeatAt && window.__aeroVm.lastHeartbeatAt > 0);
});

test('VM enforces cache limits without killing the VM', async ({ page }) => {
  await page.goto('/', { waitUntil: 'load' });
  await page.waitForSelector('#vm-safety-panel');

  await page.fill('#vm-max-disk-cache-mib', '1');
  await page.click('#vm-start-coop');
  await page.waitForFunction(() => window.__aeroVm?.lastHeartbeatAt && window.__aeroVm.lastHeartbeatAt > 0);

  await page.fill('#vm-cache-write-mib', '2');
  await page.click('#vm-write-disk-cache');

  await page.waitForFunction(() => {
    const text = document.querySelector('#vm-error')?.textContent ?? '';
    return text.includes('ResourceLimitExceeded') || text.includes('disk cache request');
  });

  await page.waitForFunction(() => window.__aeroVm?.state === 'running');
});

test('VM auto-saves a crash snapshot when enabled', async ({ page }) => {
  await page.goto('/', { waitUntil: 'load' });
  await page.waitForSelector('#vm-safety-panel');

  await page.check('#vm-auto-snapshot');
  await page.click('#vm-start-crash');

  await page.waitForFunction(() => {
    const text = document.querySelector('#vm-error')?.textContent ?? '';
    return text.includes('InternalError');
  });

  const raw = await page.evaluate(() => localStorage.getItem('aero:lastCrashSnapshot'));
  expect(raw).not.toBeNull();

  const parsed = JSON.parse(raw!);
  expect(parsed.reason).toBe('crash');

  await page.click('#vm-load-saved-snapshot');
  await page.waitForFunction(() => {
    const text = document.querySelector('#vm-snapshot')?.textContent ?? '';
    return text.includes('"reason": "crash"');
  });

  await page.click('#vm-clear-saved-snapshot');
  const cleared = await page.evaluate(() => localStorage.getItem('aero:lastCrashSnapshot'));
  expect(cleared).toBeNull();
});
