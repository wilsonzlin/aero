import type { Page } from '@playwright/test';

import type { WebGpuBenchOptions, WebGpuBenchResult } from '../../web/src/bench/webgpu_bench';

export type WebGpuScenarioRunOptions = {
  /**
   * Base URL where the Aero web app is served.
   *
   * By default we use the dev server from `playwright.config.ts` (5173) and
   * load the standalone `web/` entrypoint.
   */
  baseUrl?: string;

  benchOptions?: WebGpuBenchOptions;
};

export type WebGpuScenarioOutput = {
  scenario: 'webgpu';
  bench: WebGpuBenchResult;
  perfExport: unknown;
};

export const DEFAULT_WEBGPU_BENCH_OPTIONS: Required<WebGpuBenchOptions> = {
  frames: 60,
  warmupFrames: 5,
  width: 256,
  height: 256,
  drawCallsPerFrame: 200,
  pipelineSwitchesPerFrame: 50,
  compute: false,
  computeWorkgroups: 256,
};

export async function runWebGpuScenario(page: Page, opts: WebGpuScenarioRunOptions = {}): Promise<WebGpuScenarioOutput> {
  const baseUrl = opts.baseUrl ?? 'http://127.0.0.1:5173';
  const url = `${baseUrl.replace(/\/$/, '')}/web/`;
  await page.goto(url, { waitUntil: 'load' });

  await page.waitForFunction(() => {
    const aero = (window as unknown as { aero?: unknown }).aero;
    if (!aero || typeof aero !== 'object') return false;
    const bench = (aero as { bench?: unknown }).bench;
    if (!bench || typeof bench !== 'object') return false;
    return typeof (bench as { runWebGpuBench?: unknown }).runWebGpuBench === 'function';
  });

  const benchOptions = {
    ...DEFAULT_WEBGPU_BENCH_OPTIONS,
    ...(opts.benchOptions ?? {}),
  };

  const bench: WebGpuBenchResult = await page.evaluate(async (optionsArg) => {
    const aero = (window as unknown as { aero?: unknown }).aero;
    if (!aero || typeof aero !== 'object') throw new Error('window.aero is missing');
    const bench = (aero as { bench?: unknown }).bench;
    if (!bench || typeof bench !== 'object') throw new Error('window.aero.bench is missing');
    const fn = (bench as { runWebGpuBench?: unknown }).runWebGpuBench;
    if (typeof fn !== 'function') throw new Error('window.aero.bench.runWebGpuBench is missing');
    return await (fn as (opts: unknown) => Promise<unknown>).call(bench, optionsArg);
  }, benchOptions);

  const perfExport = await page.evaluate(() => {
    const aero = (window as unknown as { aero?: unknown }).aero;
    if (!aero || typeof aero !== 'object') return undefined;
    const perf = (aero as { perf?: unknown }).perf;
    if (!perf || typeof perf !== 'object') return undefined;
    const fn = (perf as { export?: unknown }).export;
    if (typeof fn !== 'function') return undefined;
    return (fn as () => unknown).call(perf);
  });

  return {
    scenario: 'webgpu',
    bench,
    perfExport,
  };
}
