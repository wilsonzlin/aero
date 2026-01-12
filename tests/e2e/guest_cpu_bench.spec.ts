import { expect, test } from '@playwright/test';

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? 'http://127.0.0.1:4173';

test('guest cpu bench smoke test', async ({ page }) => {
  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: 'load' });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const aero = (globalThis as any).aero;
    return typeof aero?.bench?.runGuestCpuBench === 'function';
  });

  const res = await page.evaluate(async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const runGuestCpuBench = (globalThis as any).aero?.bench?.runGuestCpuBench;
    if (typeof runGuestCpuBench !== 'function') {
      throw new Error('window.aero.bench.runGuestCpuBench is not available');
    }

    // Return only fields we need for assertions to avoid structured clone issues
    // if the full payload includes bigint fields.
    const res = await runGuestCpuBench({
      variant: 'alu32',
      mode: 'interpreter',
      seconds: 0.1,
    });

    return {
      expected_checksum: res.expected_checksum,
      observed_checksum: res.observed_checksum,
      mips: res.mips,
      run_mips: res.run_mips,
    };
  });

  expect(res.expected_checksum).toBe(res.observed_checksum);
  expect(res.mips).toBeGreaterThan(0);
  expect(Array.isArray(res.run_mips)).toBe(true);
  expect(res.run_mips.length).toBeGreaterThanOrEqual(1);

  const perfExport = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return (globalThis as any).aero.perf.export();
  });

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const guestCpu = (perfExport as any).benchmarks?.guest_cpu;
  expect(guestCpu).toBeTruthy();
  expect(Array.isArray(guestCpu.results)).toBe(true);
  expect(guestCpu.results.length).toBeGreaterThan(0);
  expect(guestCpu.results[0].expected_checksum).toBe(guestCpu.results[0].observed_checksum);
});

