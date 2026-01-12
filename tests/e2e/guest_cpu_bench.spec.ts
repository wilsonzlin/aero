import { expect, test } from '@playwright/test';

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? 'http://127.0.0.1:4173';

function alu32ExpectedChecksum(iters: number): string {
  let eax = 0x9abc_def0 >>> 0;
  const edx = 0x7f4a_7c15 >>> 0;
  for (let i = 0; i < iters; i++) {
    eax = (eax + edx) >>> 0;
    const ebx = eax >>> 13;
    eax = (eax ^ ebx) >>> 0;
    eax = (eax << 1) >>> 0;
  }
  return `0x${eax.toString(16).padStart(8, '0')}`;
}

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
    const canonical = await runGuestCpuBench({
      variant: 'alu32',
      mode: 'interpreter',
      seconds: 0.1,
    });

    const itersRun = await runGuestCpuBench({
      variant: 'alu32',
      mode: 'interpreter',
      iters: 1234,
    });

    let bothError: string | undefined;
    try {
      await runGuestCpuBench({
        variant: 'alu32',
        mode: 'interpreter',
        seconds: 0.1,
        iters: 1234,
      });
    } catch (e) {
      bothError = e instanceof Error ? e.message : String(e);
    }

    return {
      canonical_expected_checksum: canonical.expected_checksum,
      canonical_observed_checksum: canonical.observed_checksum,
      canonical_mips: canonical.mips,
      canonical_run_mips: canonical.run_mips,
      iters_expected_checksum: itersRun.expected_checksum,
      iters_observed_checksum: itersRun.observed_checksum,
      iters_per_run: itersRun.iters_per_run,
      both_error: bothError,
    };
  });

  expect(res.canonical_expected_checksum).toBe(res.canonical_observed_checksum);
  expect(res.canonical_expected_checksum).toBe(alu32ExpectedChecksum(10_000));
  expect(res.canonical_mips).toBeGreaterThan(0);
  expect(Array.isArray(res.canonical_run_mips)).toBe(true);
  expect(res.canonical_run_mips.length).toBeGreaterThanOrEqual(1);
  expect(res.iters_expected_checksum).toBe(res.iters_observed_checksum);
  expect(res.iters_expected_checksum).toBe(alu32ExpectedChecksum(1234));
  expect(res.iters_per_run).toBe(1234);
  expect(res.both_error).toContain('mutually exclusive');

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
